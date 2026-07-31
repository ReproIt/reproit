/*
 * Outbound-exchange capture and hermetic replay for reproit-backend-java.
 *
 * Java has no monkeypatching, so the boundary is explicit and OPT-IN, like
 * the Rust SDK: route outbound HTTP through {@link Http#send} and database
 * statements through {@link Db#run}, and every dependency exchange (request
 * AND response) is recorded onto the ambient request trace, bounded and
 * redacted at source. Anything not routed through these entry points is
 * invisible to capture and unavailable at replay.
 *
 * With `REPROIT_REPLAY` naming a `reproit-backend-capture` payload the SAME
 * entry points serve the recorded exchanges: no socket is opened and no
 * database is contacted. An unmatched call emits the structured
 * `REPROIT:DIVERGENCE` line and answers 599 (HTTP) or throws (db).
 *
 * The ambient trace is a ThreadLocal, so a handler that hands work to
 * another thread must carry it there: wrap the task with
 * {@link #propagate(Runnable)} / {@link #propagate(Callable)} at SUBMISSION
 * time, or hand out a {@link #propagate(Executor)} executor. An unscoped
 * call is simply not recorded, never half-recorded.
 */
package dev.reproit.backend;

import java.io.IOException;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.Callable;
import java.util.concurrent.Executor;

public final class Instrument {
    private static final ThreadLocal<BackendTrace> AMBIENT = new ThreadLocal<>();
    private static final Object SESSION_LOCK = new Object();
    private static boolean sessionResolved = false;
    private static Replay session = null;

    private Instrument() {}

    /**
     * Run `body` with `trace` ambient for {@link Http#send} and
     * {@link Db#run}. The servlet filter scopes each request automatically;
     * call this directly for hand-rolled servers or worker threads.
     */
    public static <T> T scope(BackendTrace trace, Callable<T> body) throws Exception {
        BackendTrace previous = AMBIENT.get();
        AMBIENT.set(trace);
        try {
            return body.call();
        } finally {
            if (previous == null) {
                AMBIENT.remove();
            } else {
                AMBIENT.set(previous);
            }
        }
    }

    /** Run `body` with `trace` ambient, for adapters without a return value. */
    public static void scopeRun(BackendTrace trace, ThrowingRunnable body) throws Exception {
        scope(trace, () -> {
            body.run();
            return null;
        });
    }

    /** A body that may throw, for {@link #scopeRun}. */
    public interface ThrowingRunnable {
        void run() throws Exception;
    }

    /**
     * Carry the CURRENT ambient trace onto whichever thread runs `body`.
     *
     * The trace is read at wrap time, on the calling thread, and re-scoped
     * inside the task, so `pool.submit(Instrument.propagate(task))` records
     * the pool thread's dependency calls onto the originating request. Wrap
     * at submission, never inside the task: by then the ambient is gone.
     *
     * With no ambient trace this is an identity wrapper, so it is always safe
     * to apply and an unscoped call stays unrecorded rather than
     * half-recorded.
     */
    public static Runnable propagate(Runnable body) {
        BackendTrace carried = AMBIENT.get();
        if (carried == null) {
            return body;
        }
        return () -> {
            BackendTrace previous = AMBIENT.get();
            AMBIENT.set(carried);
            try {
                body.run();
            } finally {
                restore(previous);
            }
        };
    }

    /** {@link #propagate(Runnable)} for a value-returning task. */
    public static <T> Callable<T> propagate(Callable<T> body) {
        BackendTrace carried = AMBIENT.get();
        if (carried == null) {
            return body;
        }
        return () -> {
            BackendTrace previous = AMBIENT.get();
            AMBIENT.set(carried);
            try {
                return body.call();
            } finally {
                restore(previous);
            }
        };
    }

    /**
     * An executor that propagates the trace ambient at the moment each task
     * is handed to it, so `execute` needs no per-call wrapping.
     */
    public static Executor propagate(Executor delegate) {
        return command -> delegate.execute(propagate(command));
    }

    private static void restore(BackendTrace previous) {
        if (previous == null) {
            AMBIENT.remove();
        } else {
            AMBIENT.set(previous);
        }
    }

    static BackendTrace ambient() {
        BackendTrace trace = AMBIENT.get();
        return trace != null && !trace.finished() ? trace : null;
    }

    /**
     * Load the replay session (when `REPROIT_REPLAY` is set) and pin the
     * process envelope. Idempotent; the first {@link Http#send} or
     * {@link Db#run} triggers it lazily, but calling it from `main` pins the
     * time zone before any zone-sensitive code runs.
     */
    public static void init() {
        session();
    }

    /** True when this process serves a recorded capture instead of live calls. */
    public static boolean replaying() {
        return session() != null;
    }

    /** The seeded replay stream, or null outside replay mode. */
    public static Replay.Rng replayRng() {
        Replay active = session();
        return active == null ? null : active.rng();
    }

    static Replay session() {
        synchronized (SESSION_LOCK) {
            if (!sessionResolved) {
                sessionResolved = true;
                String path = System.getenv("REPROIT_REPLAY");
                if (path != null && !path.isBlank()) {
                    session = Replay.load(path);
                    if (session != null) session.pinEnvelope();
                }
            }
            return session;
        }
    }

    // Tests drive the session directly rather than mutating the environment.
    static void resetSessionForTest(Replay replacement) {
        synchronized (SESSION_LOCK) {
            sessionResolved = replacement != null;
            session = replacement;
        }
    }

    /** Outbound HTTP through the exchange boundary. */
    public static final class Http {
        private Http() {}

        /** The uniform response both modes produce. */
        public record ExchangeResponse(int status, Map<String, String> headers, byte[] body) {
            public String text() {
                return new String(body, StandardCharsets.UTF_8);
            }

            public Object json() {
                return Json.parse(text());
            }
        }

        /**
         * Send `request` through the boundary. Capture mode executes it and
         * records request and response onto the ambient trace; replay mode
         * serves the recorded exchange with no network at all. A 599 with a
         * `{"reproit": ...}` body is a divergence, never a guess.
         */
        public static ExchangeResponse send(HttpClient client, HttpRequest request)
                throws IOException, InterruptedException {
            String method = request.method();
            String url = request.uri().toString();
            byte[] requestBody = bodyOf(request);
            String requestContentType =
                request.headers().firstValue("content-type").orElse("");
            Replay active = session();
            if (active != null) {
                Map<String, Object> probe = new LinkedHashMap<>();
                probe.put("method", method);
                probe.put("url", url);
                probe.putAll(Exchange.boundedBody(requestBody, requestContentType));
                Map<String, Object> recorded = active.matched("http", probe);
                if (recorded == null) return diverged599("diverged");
                Object rawResponse = recorded.get("response");
                Map<String, Object> response = rawResponse instanceof Map<?, ?> map
                    ? castMap(map) : Map.of();
                if (Boolean.TRUE.equals(response.get("truncated"))) {
                    // The capture kept identity but not bytes; serving a
                    // guessed body would be a silent lie.
                    active.diverge("http", probe);
                    return diverged599("truncated-exchange-body");
                }
                return servedResponse(response);
            }
            HttpResponse<byte[]> response =
                client.send(request, HttpResponse.BodyHandlers.ofByteArray());
            Map<String, String> responseHeaders = new LinkedHashMap<>();
            response.headers().map().forEach((name, values) -> {
                if (!values.isEmpty()) responseHeaders.put(name, values.get(0));
            });
            BackendTrace trace = ambient();
            if (trace != null) {
                Map<String, String> requestHeaders = new LinkedHashMap<>();
                request.headers().map().forEach((name, values) -> {
                    if (!values.isEmpty()) requestHeaders.put(name, values.get(0));
                });
                try {
                    trace.effect("call", new BackendTrace.Effect()
                        .resource(request.uri().getHost())
                        .key(method + " " + Replay.pathAndQuery(url))
                        .exchange(Exchange.http(
                            method,
                            url,
                            requestHeaders,
                            requestBody,
                            requestContentType,
                            response.statusCode(),
                            responseHeaders,
                            response.body(),
                            responseHeaders.getOrDefault("content-type", ""))));
                } catch (RuntimeException ignored) {
                    // The trace may have finished or overflowed; the host call goes on.
                }
            }
            return new ExchangeResponse(
                response.statusCode(), responseHeaders, response.body());
        }

        private static ExchangeResponse servedResponse(Map<String, Object> response) {
            int status = response.get("status") instanceof Number number
                ? number.intValue() : 200;
            Map<String, String> headers = new LinkedHashMap<>();
            if (response.get("headers") instanceof Map<?, ?> recorded) {
                for (Map.Entry<?, ?> entry : recorded.entrySet()) {
                    String name = String.valueOf(entry.getKey()).toLowerCase(Locale.ROOT);
                    if (name.equals("content-length") || name.equals("transfer-encoding")
                        || name.equals("content-encoding")) {
                        continue;
                    }
                    if (entry.getValue() != null) {
                        headers.put(name, String.valueOf(entry.getValue()));
                    }
                }
            }
            Object body = response.get("body");
            byte[] bytes;
            if (body == null) {
                bytes = new byte[0];
            } else if (body instanceof String text) {
                bytes = text.getBytes(StandardCharsets.UTF_8);
            } else {
                bytes = Json.canonicalJson(body).getBytes(StandardCharsets.UTF_8);
            }
            return new ExchangeResponse(status, headers, bytes);
        }

        private static ExchangeResponse diverged599(String reason) {
            Map<String, Object> body = new LinkedHashMap<>();
            body.put("reproit", reason);
            return new ExchangeResponse(
                599,
                new LinkedHashMap<>(Map.of("content-type", "application/json")),
                Json.canonicalJson(body).getBytes(StandardCharsets.UTF_8));
        }

        private static byte[] bodyOf(HttpRequest request) {
            return request.bodyPublisher()
                .map(publisher -> {
                    BodyCollector collector = new BodyCollector();
                    publisher.subscribe(collector);
                    return collector.await();
                })
                .orElse(new byte[0]);
        }
    }

    /**
     * Database statements through the exchange boundary. Java has no driver
     * to monkeypatch, so the app routes each statement through {@link #run};
     * anything else is invisible to capture and unavailable at replay.
     */
    public static final class Db {
        private Db() {}

        /** One statement's result: rows plus the command tag. */
        public record Outcome(String command, long rowCount, List<Object> rows) {}

        /** A recorded or live database failure. */
        public static final class DbException extends RuntimeException {
            private final String code;

            public DbException(String message, String code) {
                super(message);
                this.code = code;
            }

            public String code() {
                return code;
            }
        }

        /** A live statement, for {@link #run}. */
        public interface Statement {
            Outcome execute() throws Exception;
        }

        /**
         * Run one statement through the boundary: replay mode serves the
         * recorded outcome without calling `live`; capture mode executes it
         * and records the exchange either way it settles.
         */
        public static Outcome run(String text, List<Object> values, Statement live) {
            Replay active = session();
            if (active != null) {
                Map<String, Object> probe = new LinkedHashMap<>();
                probe.put("text", text == null ? "" : text);
                if (values != null && !values.isEmpty()) {
                    probe.put("values", new ArrayList<>(values));
                }
                Map<String, Object> recorded = active.matched("pg", probe);
                if (recorded == null) {
                    throw new DbException("reproit: db call diverged from the capture", null);
                }
                Map<String, Object> response = recorded.get("response") instanceof Map<?, ?> map
                    ? castMap(map) : Map.of();
                if (response.get("error") instanceof Map<?, ?> error) {
                    Object message = error.get("message");
                    Object code = error.get("code");
                    throw new DbException(
                        message == null ? "recorded db error" : String.valueOf(message),
                        code == null ? null : String.valueOf(code));
                }
                long rowCount = response.get("rowCount") instanceof Number number
                    ? number.longValue() : 0;
                List<Object> rows = response.get("rows") instanceof List<?> list
                    ? new ArrayList<>(list) : List.of();
                Object command = response.get("command");
                return new Outcome(
                    command == null ? null : String.valueOf(command), rowCount, rows);
            }
            Outcome outcome;
            try {
                outcome = live.execute();
            } catch (Exception failure) {
                String code = failure instanceof DbException typed ? typed.code() : null;
                record(text, values, Exchange.dbError(String.valueOf(failure.getMessage()), code));
                if (failure instanceof RuntimeException runtime) throw runtime;
                throw new DbException(String.valueOf(failure.getMessage()), code);
            }
            record(text, values, Exchange.dbOutcome(
                outcome.command(), outcome.rowCount(), outcome.rows()));
            return outcome;
        }

        private static void record(
                String text, List<Object> values, Map<String, Object> outcome) {
            BackendTrace trace = ambient();
            if (trace == null) return;
            try {
                String key = text == null ? "" : text;
                trace.effect(Exchange.dbEffectKind(text), new BackendTrace.Effect()
                    .resource("pg")
                    .key(key.substring(0, Math.min(key.length(), 256)))
                    .exchange(Exchange.db(text, values, outcome)));
            } catch (RuntimeException ignored) {
                // The trace may have finished or overflowed; the host call goes on.
            }
        }
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> castMap(Map<?, ?> map) {
        return (Map<String, Object>) map;
    }

    /** Drains an HttpRequest body publisher synchronously and boundedly. */
    private static final class BodyCollector
            implements java.util.concurrent.Flow.Subscriber<java.nio.ByteBuffer> {
        private final java.io.ByteArrayOutputStream buffer = new java.io.ByteArrayOutputStream();
        private final java.util.concurrent.CountDownLatch done =
            new java.util.concurrent.CountDownLatch(1);

        @Override
        public void onSubscribe(java.util.concurrent.Flow.Subscription subscription) {
            subscription.request(Long.MAX_VALUE);
        }

        @Override
        public void onNext(java.nio.ByteBuffer item) {
            byte[] chunk = new byte[item.remaining()];
            item.get(chunk);
            // One byte past the budget is enough to know the size class.
            if (buffer.size() <= Exchange.MAX_EXCHANGE_BODY_BYTES) {
                buffer.write(chunk, 0, chunk.length);
            }
        }

        @Override
        public void onError(Throwable throwable) {
            done.countDown();
        }

        @Override
        public void onComplete() {
            done.countDown();
        }

        byte[] await() {
            try {
                // A body publisher is already materialized in memory here, so
                // this completes immediately; the bound stops a pathological
                // publisher from blocking the host call.
                if (!done.await(5, java.util.concurrent.TimeUnit.SECONDS)) return new byte[0];
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return new byte[0];
            }
            return buffer.toByteArray();
        }
    }
}
