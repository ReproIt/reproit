/*
 * Production capture mode: config-gated self-sampling upload of finished
 * operation traces to the Reproit Cloud ingest endpoint
 * (`/v1/capture-batches`).
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

public final class Capture implements TraceSink {
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
        String commit;
        int healthySamplePerMille = 0;
        long flushIntervalMs = 3000;
        long requestTimeoutMs = 5000;
        int retryLimit = 2;

        public Config endpoint(String value) { this.endpoint = value; return this; }
        public Config apiKey(String value) { this.apiKey = value; return this; }
        public Config appId(String value) { this.appId = value; return this; }
        public Config build(String value) { this.build = value; return this; }
        public Config commit(String value) { this.commit = value; return this; }
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
    private final String commit;
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
        if (config.commit != null && !TOKEN.matcher(config.commit).matches()) return null;
        try {
            return new Capture(config);
        } catch (RuntimeException unusable) {
            return null;
        }
    }

    /**
     * Code identity for the capture, in priority order: explicit config, then
     * the common CI and platform environment. Never shells out to git.
     */
    static String resolveCommit(Config config) {
        return resolveCommit(config, environment);
    }

    /**
     * Overload taking the environment explicitly, mirroring the Python SDK's
     * {@code resolve_commit(..., env=None)}. A suite that pins an exact
     * deployment shape must STATE its environment rather than inherit it: a
     * GitHub runner always sets GITHUB_SHA and a laptop never does, so the
     * batch grows a commit key only in CI and the test is green locally and red
     * on the runner. That is exactly how this surfaced, on the first push after
     * this SDK started gating.
     */
    static String resolveCommit(Config config, Map<String, String> env) {
        String[] candidates = {
            config.commit, env.get("REPROIT_COMMIT"), env.get("GITHUB_SHA"),
        };
        for (String candidate : candidates) {
            if (candidate != null && TOKEN.matcher(candidate).matches()) return candidate;
        }
        return null;
    }

    /** The ambient environment, replaceable by tests so the suite states it. */
    static Map<String, String> environment = System.getenv();

    private Capture(Config config) {
        this.endpoint = config.endpoint;
        this.apiKey = config.apiKey;
        this.appId = config.appId;
        this.build = config.build;
        this.commit = resolveCommit(config);
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
    @Override
    public TraceContext context() {
        String traceId =
            "cap-" + System.currentTimeMillis() + "-" + traceSeq.getAndIncrement();
        return new TraceContext(traceId, null, 0, build, null, true);
    }

    /**
     * Hand a finished trace to the sampler. Unfinished traces are ignored.
     * Never blocks and never fails visibly; overflow drops the oldest queued
     * operation.
     */
    @Override
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
                    while (queue.size() < 1 && !flushNow) {
                        long remaining = deadline - System.nanoTime();
                        if (remaining <= 0) break;
                        if (signal.awaitNanos(remaining) <= 0) break;
                    }
                    flushNow = false;
                    int take = Math.min(queue.size(), 1);
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

    // Build one source-neutral capture-batch-v1 payload.
    Map<String, Object> buildBatch(List<Operation> operations) {
        if (operations.size() != 1) {
            throw new IllegalArgumentException(
                "a causal capture batch must contain exactly one operation");
        }
        Operation operation = operations.get(0);
        String batchId =
            "cb-java-" + System.currentTimeMillis() + "-" + batchSeq.getAndIncrement();
        Map<String, Object> first = operation.events().isEmpty()
            ? Map.of() : operation.events().get(0);
        String traceId = first.get("traceId") instanceof String value ? value : null;
        List<Map<String, Object>> events = new ArrayList<>();
        class Builder {
            String parent;

            void add(Map<String, Object> event) {
                add(event, null);
            }

            // Real monotonic offsets from the trace's envelope stamps; the
            // ordinal fallback only applies to traces recorded without
            // capture mode.
            void add(Map<String, Object> event, Map<String, Object> source) {
                int sequence = events.size() + 1;
                String eventId = "evt_backend-java_" + sequence;
                long monotonicNs = source != null && source.get("monoNs") instanceof Number stamp
                    ? Math.max(1, stamp.longValue())
                    : sequence;
                Map<String, Object> item = new LinkedHashMap<>();
                item.put("id", eventId);
                item.put("sequence", (long) sequence);
                item.put("monotonicNs", monotonicNs);
                item.put("causalParentIds", parent == null ? List.of() : List.of(parent));
                if (traceId != null) item.put("traceId", traceId);
                item.put("event", event);
                events.add(item);
                parent = eventId;
            }
        }
        Builder builder = new Builder();
        builder.add(new LinkedHashMap<>(Map.of(
            "kind", "operation-start", "name", operation.operation())));
        Object input = first.get("input");
        Map<String, Object> value = new LinkedHashMap<>();
        if (input == null) {
            value.put("representation", "structural");
            value.put("shape", Map.of("type", "unknown"));
        } else {
            value.put("representation", "replayable");
            value.put("value", input);
            value.put("redaction", "redacted-at-source");
        }
        Map<String, Object> trigger = new LinkedHashMap<>();
        trigger.put("kind", "trigger");
        trigger.put("trigger", "http-request");
        trigger.put("subject", operation.operation());
        trigger.put("value", value);
        builder.add(trigger, first);
        // Determinism envelope: where and when the capture happened, and a
        // seed that makes REPLAY runs deterministic. Honesty note: the seed
        // does not reproduce the app's original randomness; it pins the
        // replay's.
        Map<String, Object> attributes = new LinkedHashMap<>();
        attributes.put(
            "observedAtMs",
            first.get("at") instanceof Number at ? at.longValue() : System.currentTimeMillis());
        attributes.put("tz", java.util.TimeZone.getDefault().getID());
        attributes.put("runtime", "java " + System.getProperty("java.version"));
        attributes.put("os", System.getProperty("os.name"));
        attributes.put("arch", System.getProperty("os.arch"));
        attributes.put("replaySeed", replaySeed());
        String imageDigest = System.getenv("REPROIT_IMAGE_DIGEST");
        if (imageDigest != null && TOKEN.matcher(imageDigest).matches()) {
            attributes.put("imageDigest", imageDigest);
        }
        Map<String, Object> envelope = new LinkedHashMap<>();
        envelope.put("kind", "checkpoint");
        envelope.put("name", "determinism-envelope");
        envelope.put("attributes", attributes);
        builder.add(envelope, first);
        for (Map<String, Object> source : operation.events()) {
            if (!"effect".equals(source.get("kind"))) continue;
            String effect = source.get("effect") instanceof String text && !text.isEmpty()
                ? text : "backend-effect";
            String subject = source.get("resource") instanceof String text && !text.isEmpty()
                ? text : operation.operation();
            Map<String, Object> captured = new LinkedHashMap<>();
            captured.put("representation", "replayable");
            captured.put("value", source);
            captured.put("redaction", "redacted-at-source");
            Map<String, Object> causal = new LinkedHashMap<>();
            causal.put("kind", "effect");
            causal.put("effect", effect);
            causal.put("subject", subject);
            causal.put("value", captured);
            builder.add(causal, source);
        }
        // Nest the raw return event exactly like the raw effect events, so
        // the batch can be projected back to a replayable backend capture.
        // The subject names the carrier: `backend_capture_from_batch` in
        // reproit-protocol keys the inversion on "operation-return".
        Map<String, Object> returned = operation.events().stream()
            .filter(event -> "return".equals(event.get("kind")))
            .reduce((left, right) -> right)
            .orElse(null);
        if (returned != null) {
            Map<String, Object> captured = new LinkedHashMap<>();
            captured.put("representation", "replayable");
            captured.put("value", returned);
            captured.put("redaction", "redacted-at-source");
            Map<String, Object> carrier = new LinkedHashMap<>();
            carrier.put("kind", "effect");
            carrier.put("effect", "operation-return");
            carrier.put("subject", "operation-return");
            carrier.put("value", captured);
            builder.add(carrier, returned);
        }
        boolean success = returned != null && Boolean.TRUE.equals(returned.get("success"));
        builder.add(new LinkedHashMap<>(Map.of(
            "kind", "operation-end",
            "name", operation.operation(),
            "outcome", success ? "succeeded" : "failed")));
        if (operation.status() != null && operation.status() >= 500) {
            String signature = SERVER_ERROR_ORACLE + ":" + operation.operation();
            String message = "backend operation " + operation.operation()
                + " returned HTTP " + operation.status();
            Map<String, Object> failure = new LinkedHashMap<>();
            failure.put("observation", "exception");
            failure.put("authority", "runtime-diagnosis");
            failure.put("summary", message);
            failure.put("signature", signature);
            failure.put("observationPoint", operation.operation());
            failure.put("artifactIds", List.of());
            builder.add(new LinkedHashMap<>(Map.of(
                "kind", "observation", "failure", failure)));
        }
        Map<String, Object> batch = new LinkedHashMap<>();
        batch.put("version", 1);
        batch.put("batchId", batchId);
        batch.put("projectId", appId);
        batch.put("sessionId", traceId == null ? batchId : traceId);
        batch.put("emitter", Map.of(
            "id", "backend-java", "kind", "runtime-sdk",
            "component", "backend", "runtime", "java"));
        batch.put("observedAt", Long.toString(System.currentTimeMillis()));
        batch.put("policy", Map.of(
            "consent", "application-telemetry", "retentionClass", "standard"));
        List<Map<String, Object>> capabilities = new ArrayList<>();
        capabilities.add(Map.of("capability", "http", "completeness", "complete"));
        capabilities.add(Map.of(
            "capability", "database",
            "completeness", "partial",
            "detail", "effect records do not prove complete database state capture"));
        // Declared only when Instrument actually recorded exchanges, so the
        // capsule completeness model never over-claims on captures from apps
        // that never routed a call through the boundary.
        boolean hasExchanges = operation.events().stream()
            .anyMatch(event -> event.get("exchange") instanceof Map<?, ?>);
        if (hasExchanges) {
            capabilities.add(Map.of(
                "capability", "network",
                "completeness", "complete",
                "detail", "outbound dependency exchanges recorded with responses"));
        }
        batch.put("capabilities", capabilities);
        batch.put("events", events);
        batch.put("artifacts", List.of());
        if (build != null || commit != null) {
            Map<String, Object> deployment = new LinkedHashMap<>();
            if (build != null) deployment.put("version", build);
            if (commit != null) deployment.put("commit", commit);
            batch.put("deployment", deployment);
        }
        return batch;
    }

    // 16 hex characters, the width the replay stream reads.
    private static String replaySeed() {
        byte[] seed = new byte[8];
        new java.security.SecureRandom().nextBytes(seed);
        StringBuilder out = new StringBuilder(16);
        for (byte value : seed) out.append(String.format("%02x", value));
        return out.toString();
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
