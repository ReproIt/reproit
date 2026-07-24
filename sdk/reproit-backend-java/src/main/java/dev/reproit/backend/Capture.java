/*
 * Production capture mode: config-gated self-sampling upload of finished
 * operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
 *
 * Java port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
 * untouched: this class only adds a place to hand a finished BackendTrace
 * when no `x-reproit-trace` header exists. Operations that end in a server
 * error (HTTP 5xx) or report `success == false` are always captured; healthy
 * operations only under an optional per-mille baseline sample (default 0).
 *
 * Everything is bounded and capture failure is invisible to the host app:
 * a fixed-depth queue drops oldest on overflow, batches and retries are
 * capped, uploads run on one daemon thread via java.net.http.HttpClient, and
 * `record` never blocks or throws.
 */
package dev.reproit.backend;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.locks.Condition;
import java.util.concurrent.locks.ReentrantLock;
import java.util.regex.Pattern;

public final class Capture {
    // Payload format identifier of the replayable capture object attached to
    // the finding context (`context.reproitCapture`).
    public static final String CAPTURE_FORMAT = "reproit-backend-capture";
    public static final int CAPTURE_VERSION = 1;
    // First-class registry oracle id for an operation that returned HTTP 5xx.
    public static final String SERVER_ERROR_ORACLE = "backend-server-error";

    // Bounds. Queue overflow drops the OLDEST pending operation; an oversized
    // capture payload drops trailing effect events before it drops itself.
    static final int MAX_QUEUE_OPERATIONS = 64;
    static final int MAX_BATCH_OPERATIONS = 16;
    static final int MAX_CAPTURE_JSON_BYTES = 48 * 1024;
    static final int MIN_FLUSH_INTERVAL_MS = 100;
    static final int MAX_RETRY_LIMIT = 5;

    // The ingest protocol token charset (`validate_token` in reproit-protocol).
    private static final Pattern TOKEN = Pattern.compile("^[A-Za-z0-9._:-]{1,128}$");

    /** Capture configuration; plain fields with chainable setters. */
    public static final class Config {
        String endpoint;
        String apiKey;
        String appId;
        String build;
        int healthySamplePerMille = 0;
        long flushIntervalMs = 3000;
        long requestTimeoutMs = 5000;
        int retryLimit = 2;

        public Config endpoint(String value) { this.endpoint = value; return this; }
        public Config apiKey(String value) { this.apiKey = value; return this; }
        public Config appId(String value) { this.appId = value; return this; }
        public Config build(String value) { this.build = value; return this; }
        public Config healthySamplePerMille(int value) {
            this.healthySamplePerMille = value;
            return this;
        }
        public Config flushIntervalMs(long value) { this.flushIntervalMs = value; return this; }
        public Config requestTimeoutMs(long value) { this.requestTimeoutMs = value; return this; }
        public Config retryLimit(int value) { this.retryLimit = value; return this; }
    }

    public record Stats(
        long capturedOperations, long droppedOperations, long sentBatches, long failedBatches) {}

    record Operation(String operation, Integer status, List<Map<String, Object>> events) {}

    private final String endpoint;
    private final String apiKey;
    private final String appId;
    private final String build;
    private final int healthySamplePerMille;
    private final long flushIntervalMs;
    private final long requestTimeoutMs;
    private final int retryLimit;
    private final HttpClient client;

    private final ReentrantLock lock = new ReentrantLock();
    private final Condition signal = lock.newCondition();
    private final ArrayDeque<Operation> queue = new ArrayDeque<>();
    private boolean sending = false;
    private boolean flushNow = false;
    private final AtomicLong traceSeq = new AtomicLong(1);
    private final AtomicLong batchSeq = new AtomicLong(1);
    private long capturedOperations = 0;
    private long droppedOperations = 0;
    private long sentBatches = 0;
    private long failedBatches = 0;

    /**
     * Start capture mode. Returns null (capture disabled, host unaffected)
     * when the config is unusable: empty endpoint/key or identifiers the
     * ingest protocol would reject.
     */
    public static Capture create(Config config) {
        if (config == null) return null;
        if (config.endpoint == null || config.endpoint.strip().isEmpty()) return null;
        if (config.apiKey == null || config.apiKey.strip().isEmpty()) return null;
        if (config.appId == null || !TOKEN.matcher(config.appId).matches()) return null;
        if (config.build != null && !TOKEN.matcher(config.build).matches()) return null;
        try {
            return new Capture(config);
        } catch (RuntimeException unusable) {
            return null;
        }
    }

    private Capture(Config config) {
        this.endpoint = config.endpoint;
        this.apiKey = config.apiKey;
        this.appId = config.appId;
        this.build = config.build;
        this.healthySamplePerMille = Math.max(0, config.healthySamplePerMille);
        this.flushIntervalMs = Math.max(MIN_FLUSH_INTERVAL_MS, config.flushIntervalMs);
        this.requestTimeoutMs = config.requestTimeoutMs;
        this.retryLimit = Math.min(MAX_RETRY_LIMIT, Math.max(0, config.retryLimit));
        this.client = HttpClient.newBuilder()
            .connectTimeout(Duration.ofMillis(Math.max(1, this.requestTimeoutMs)))
            .build();
        Thread worker = new Thread(this::runWorker, "reproit-capture");
        worker.setDaemon(true);
        worker.start();
    }

    /**
     * Synthesized trace context for capture-mode operations, replacing the
     * scan-time `x-reproit-trace` header requirement.
     */
    public TraceContext context() {
        String traceId =
            "cap-" + System.currentTimeMillis() + "-" + traceSeq.getAndIncrement();
        return new TraceContext(traceId, null, 0, build, null);
    }

    /**
     * Hand a finished trace to the sampler. Unfinished traces are ignored.
     * Never blocks and never fails visibly; overflow drops the oldest queued
     * operation.
     */
    public void record(BackendTrace trace) {
        try {
            List<Map<String, Object>> events = trace.events();
            Map<String, Object> returned = null;
            for (int index = events.size() - 1; index >= 0; index--) {
                if ("return".equals(events.get(index).get("kind"))) {
                    returned = events.get(index);
                    break;
                }
            }
            if (returned == null) return;
            Object rawSuccess = returned.get("success");
            boolean success = rawSuccess instanceof Boolean bool ? bool : true;
            Integer status = null;
            if (returned.get("status") instanceof Number number
                    && !(returned.get("status") instanceof Double)
                    && !(returned.get("status") instanceof Float)) {
                long value = number.longValue();
                if (value >= 0 && value <= 0xffff) status = (int) value;
            }
            boolean error = !success || (status != null && status >= 500);
            if (!error && !sampleHealthy()) return;
            Object operation = events.isEmpty() ? null : events.get(0).get("operation");
            if (!(operation instanceof String name)) return;
            lock.lock();
            try {
                capturedOperations += 1;
                queue.addLast(new Operation(name, status, new ArrayList<>(events)));
                if (queue.size() > MAX_QUEUE_OPERATIONS) {
                    queue.removeFirst();
                    droppedOperations += 1;
                }
                signal.signalAll();
            } finally {
                lock.unlock();
            }
        } catch (Throwable ignored) {
            // Capture must never surface errors into the host app.
        }
    }

    /**
     * Block up to `timeoutMs` until every queued operation has been sent (or
     * dropped). Returns false on timeout. Intended for tests, examples, and
     * graceful shutdown.
     */
    public boolean flush(long timeoutMs) {
        long deadline = System.nanoTime() + timeoutMs * 1_000_000L;
        lock.lock();
        try {
            flushNow = true;
            signal.signalAll();
            while (!queue.isEmpty() || sending) {
                long remaining = deadline - System.nanoTime();
                if (remaining <= 0) return false;
                signal.awaitNanos(remaining);
            }
            return true;
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return false;
        } finally {
            lock.unlock();
        }
    }

    public Stats stats() {
        lock.lock();
        try {
            return new Stats(capturedOperations, droppedOperations, sentBatches, failedBatches);
        } finally {
            lock.unlock();
        }
    }

    private boolean sampleHealthy() {
        if (healthySamplePerMille <= 0) return false;
        if (healthySamplePerMille >= 1000) return true;
        return ThreadLocalRandom.current().nextDouble() * 1000 < healthySamplePerMille;
    }

    private void runWorker() {
        while (true) {
            try {
                List<Operation> operations = nextBatch();
                boolean sent = send(buildBatch(operations));
                lock.lock();
                try {
                    if (sent) {
                        sentBatches += 1;
                    } else {
                        failedBatches += 1;
                        droppedOperations += operations.size();
                    }
                    sending = false;
                    signal.signalAll();
                } finally {
                    lock.unlock();
                }
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return;
            } catch (Throwable ignored) {
                // Fail closed: drop, never crash the host.
                lock.lock();
                try {
                    sending = false;
                    signal.signalAll();
                } finally {
                    lock.unlock();
                }
            }
        }
    }

    // Wait for work, gather up to the batch cap within one flush interval,
    // then drain. `flushNow` (set by flush()) cuts the gather short.
    private List<Operation> nextBatch() throws InterruptedException {
        lock.lock();
        try {
            while (true) {
                if (!queue.isEmpty()) {
                    long deadline = System.nanoTime() + flushIntervalMs * 1_000_000L;
                    while (queue.size() < MAX_BATCH_OPERATIONS && !flushNow) {
                        long remaining = deadline - System.nanoTime();
                        if (remaining <= 0) break;
                        if (signal.awaitNanos(remaining) <= 0) break;
                    }
                    flushNow = false;
                    int take = Math.min(queue.size(), MAX_BATCH_OPERATIONS);
                    List<Operation> operations = new ArrayList<>(take);
                    for (int index = 0; index < take; index++) {
                        operations.add(queue.removeFirst());
                    }
                    sending = true;
                    return operations;
                }
                flushNow = false;
                signal.await();
            }
        } finally {
            lock.unlock();
        }
    }

    // Build one event-batch-v1 payload: every captured event ships as a
    // `backend` frame, and each 5xx operation additionally ships a `finding`
    // frame tagged `backend-server-error` whose context carries the full
    // replayable capture object.
    Map<String, Object> buildBatch(List<Operation> operations) {
        String batchId =
            "cap-" + System.currentTimeMillis() + "-" + batchSeq.getAndIncrement();
        List<Map<String, Object>> frames = new ArrayList<>();
        for (Operation operation : operations) {
            for (Map<String, Object> event : operation.events()) {
                Map<String, Object> backend = new LinkedHashMap<>();
                backend.put("kind", "backend");
                backend.put("evidence", event);
                frames.add(frame(batchId, frames.size() + 1, backend));
            }
            if (operation.status() == null || operation.status() < 500) continue;
            String signature = "backend:" + operation.operation();
            String message = "backend operation " + operation.operation()
                + " returned HTTP " + operation.status();
            Map<String, Object> context = new LinkedHashMap<>();
            context.put("capture", "reproit-backend-java");
            if (build != null) context.put("build", Map.of("version", build));
            Payload payload = capturePayload(operation);
            if (payload == null) {
                context.put("captureOmitted", Boolean.TRUE);
            } else {
                context.put("reproitCapture", payload.value());
                if (payload.droppedEffects() > 0) {
                    context.put("captureDroppedEffects", (long) payload.droppedEffects());
                }
            }
            Map<String, Object> identity = new LinkedHashMap<>();
            identity.put("oracle", SERVER_ERROR_ORACLE);
            identity.put("invariant", "backend:server-error");
            identity.put("kind", "server-error");
            identity.put("message", message);
            identity.put("frame", "");
            identity.put("trigger", signature);
            identity.put("boundary", signature);
            Map<String, Object> finding = new LinkedHashMap<>();
            finding.put("kind", "finding");
            finding.put("signature", signature);
            finding.put("message", message);
            finding.put("identity", identity);
            finding.put("path", List.of());
            finding.put("context", context);
            frames.add(frame(batchId, frames.size() + 1, finding));
        }
        Map<String, Object> batch = new LinkedHashMap<>();
        batch.put("version", 1);
        batch.put("batchId", batchId);
        batch.put("appId", appId);
        batch.put("frames", frames);
        batch.put("evidence", List.of());
        if (build != null) batch.put("deployment", Map.of("version", build));
        return batch;
    }

    private static Map<String, Object> frame(String runId, int sequence, Object event) {
        Map<String, Object> frame = new LinkedHashMap<>();
        frame.put("runId", runId);
        frame.put("sequence", sequence);
        frame.put("scope", Map.of("domain", "shared"));
        frame.put("event", event);
        return frame;
    }

    record Payload(Map<String, Object> value, int droppedEffects) {}

    // The replayable capture object (`reproit debug replay-capture` input).
    // Trailing effect events are dropped first when the payload exceeds the
    // context budget; a payload that stays oversized with only start/return
    // left is omitted entirely (null).
    static Payload capturePayload(Operation operation) {
        List<Map<String, Object>> events = new ArrayList<>(operation.events());
        int droppedEffects = 0;
        while (true) {
            Map<String, Object> value = new LinkedHashMap<>();
            value.put("format", CAPTURE_FORMAT);
            value.put("version", CAPTURE_VERSION);
            value.put("operation", operation.operation());
            value.put("oracle", SERVER_ERROR_ORACLE);
            value.put("events", events);
            byte[] encoded = Json.canonicalJson(value).getBytes(StandardCharsets.UTF_8);
            if (encoded.length <= MAX_CAPTURE_JSON_BYTES) {
                return new Payload(value, droppedEffects);
            }
            int lastEffect = -1;
            for (int index = events.size() - 1; index >= 0; index--) {
                if ("effect".equals(events.get(index).get("kind"))) {
                    lastEffect = index;
                    break;
                }
            }
            if (lastEffect < 0) return null;
            events.remove(lastEffect);
            droppedEffects += 1;
        }
    }

    private boolean send(Map<String, Object> batch) throws InterruptedException {
        String body = Json.canonicalJson(batch);
        for (int attempt = 0; attempt <= retryLimit; attempt++) {
            try {
                HttpRequest request = HttpRequest.newBuilder(URI.create(endpoint))
                    .timeout(Duration.ofMillis(Math.max(1, requestTimeoutMs)))
                    .header("Authorization", "Bearer " + apiKey)
                    .header("Content-Type", "application/json")
                    .POST(HttpRequest.BodyPublishers.ofString(body, StandardCharsets.UTF_8))
                    .build();
                HttpResponse<Void> response =
                    client.send(request, HttpResponse.BodyHandlers.discarding());
                int status = response.statusCode();
                if (status >= 200 && status < 300) return true;
                // A definitive client-side rejection cannot improve on retry.
                if (status >= 400 && status < 500) return false;
            } catch (InterruptedException interrupted) {
                throw interrupted;
            } catch (Exception ignored) {
                // Network failure: retry below.
            }
            if (attempt < retryLimit) {
                Thread.sleep(200L * attempt + 200);
            }
        }
        return false;
    }
}
