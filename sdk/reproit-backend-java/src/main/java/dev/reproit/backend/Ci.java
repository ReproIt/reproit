/*
 * CI capture mode for reproit-backend-java: the flaky-CI wedge.
 *
 * Java port of the Node reference's ci.js. The trigger identity is the TEST
 * (suite + test id), not an inbound HTTP request. With `REPROIT_CI_CAPTURE=1`
 * every test runs inside its own trace, so the wrapped outbound clients
 * (ReproitHttpClient, ReproitJdbc, Instrument.Http/Db) record dependency
 * exchanges and the determinism envelope exactly as production capture does;
 * a FAILING test emits a version-2 `reproit-backend-capture` capsule to a
 * bounded on-disk spool. With `REPROIT_REPLAY` set the SAME entry points
 * re-run only the capsule's named test while the SDK serves the recorded
 * exchanges in process, and report the observed result as a structured
 * stderr marker for `reproit check`. Without either env everything is inert.
 *
 * The wire is the existing capture payload: the test identity rides in the
 * `operation` field as `test:<suite>#<test>`, and the failed assertion is
 * the existing `backend-authored-invariant` registry oracle (a test IS an
 * authored invariant). No new protocol fields, no new oracle ids.
 *
 * Two integration surfaces share one core: {@link ReproitCi} is the JUnit 5
 * extension for real suites, and {@link #suite(String)} is the dependency-free
 * micro-runner (the shape the fixtures and hermetic gates use, so a replay
 * compiles with plain javac and needs no jars and no network).
 *
 * Honest limit: replay pins the envelope and the recorded exchanges, which
 * is the whole boundary this SDK can see. A race the boundary cannot see
 * (scheduling, shared memory) is not reproduced by this capsule; `reproit
 * check` reports such runs Inconclusive, never a fake reproduction.
 */
package dev.reproit.backend;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicLong;
import java.util.regex.Pattern;

public final class Ci {
    /** Test-trigger identity prefix inside the existing `operation` field. */
    public static final String TEST_TRIGGER_PREFIX = "test:";
    /**
     * The registry oracle a failed test capsule carries: an authored
     * invariant (the test's own assertion) was violated. Existing id, not a
     * new one.
     */
    public static final String TEST_FAILURE_ORACLE = "backend-authored-invariant";
    /** Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE. */
    public static final String RESULT_MARKER = "REPROIT:CI-TEST ";
    public static final String SPOOL_MARKER = "REPROIT:CI-CAPSULE ";

    // Spool bounds. The cap covers the TOTAL bytes on disk; spilled capsules
    // beyond it are dropped and counted (in-process stats plus the on-disk
    // `dropped.count`), never silently.
    public static final String DEFAULT_SPOOL_DIR = ".reproit/ci-spool";
    public static final long DEFAULT_SPOOL_MAX_BYTES = 16L * 1024 * 1024;
    static final long SPOOL_MAX_FLOOR_BYTES = 4 * 1024;
    static final long SPOOL_MAX_CEIL_BYTES = 64L * 1024 * 1024;
    // Suite and test names share the operation field's 256-code-point bound.
    static final int MAX_NAME = 120;
    static final int MAX_ERROR_CHARS = 2048;

    private static final Pattern TOKEN = Pattern.compile("^[A-Za-z0-9._:-]{1,128}$");
    private static final AtomicLong TRACE_SEQ = new AtomicLong(1);
    private static final AtomicLong SPOOLED = new AtomicLong();
    private static final AtomicLong DROPPED = new AtomicLong();
    private static final AtomicLong FAILED_CAPTURES = new AtomicLong();

    /** The ambient environment, replaceable by tests so the suite states it. */
    static Map<String, String> environment = System.getenv();

    private Ci() {}

    public record Stats(long spooledCapsules, long droppedCapsules, long failedCaptures) {}

    public static Stats stats() {
        return new Stats(SPOOLED.get(), DROPPED.get(), FAILED_CAPTURES.get());
    }

    static void resetStatsForTest() {
        SPOOLED.set(0);
        DROPPED.set(0);
        FAILED_CAPTURES.set(0);
    }

    enum Mode { OFF, CAPTURE, REPLAY }

    static String replayPath() {
        String value = environment.get("REPROIT_REPLAY");
        return value != null && !value.isEmpty() ? value : null;
    }

    static Mode mode() {
        if (replayPath() != null) return Mode.REPLAY;
        if ("1".equals(environment.get("REPROIT_CI_CAPTURE"))) return Mode.CAPTURE;
        return Mode.OFF;
    }

    static String boundedName(String value) {
        String trimmed = String.valueOf(value).strip();
        return trimmed.length() > MAX_NAME ? trimmed.substring(0, MAX_NAME) : trimmed;
    }

    static String operationFor(String suiteName, String testName) {
        return TEST_TRIGGER_PREFIX + boundedName(suiteName) + "#" + boundedName(testName);
    }

    static String boundedError(Throwable error) {
        String message = error == null ? "null"
            : error.getMessage() != null ? error.getMessage() : String.valueOf(error);
        return message.length() > MAX_ERROR_CHARS
            ? message.substring(0, MAX_ERROR_CHARS) : message;
    }

    /** Synthesized trace context: the CI job stands where production stood. */
    static TraceContext ciContext() {
        String commit = null;
        for (String candidate : new String[] {
                environment.get("REPROIT_COMMIT"), environment.get("GITHUB_SHA")}) {
            if (candidate != null && TOKEN.matcher(candidate).matches()) {
                commit = candidate;
                break;
            }
        }
        String traceId =
            "ci-" + System.currentTimeMillis() + "-" + TRACE_SEQ.getAndIncrement();
        return new TraceContext(traceId, null, 0, commit, null, true);
    }

    static Path spoolDir() {
        String dir = environment.get("REPROIT_CI_SPOOL");
        return Path.of(dir != null && !dir.isEmpty() ? dir : DEFAULT_SPOOL_DIR);
    }

    static long spoolMaxBytes() {
        long parsed;
        try {
            parsed = Long.parseLong(String.valueOf(environment.get("REPROIT_CI_SPOOL_MAX")));
        } catch (NumberFormatException absent) {
            return DEFAULT_SPOOL_MAX_BYTES;
        }
        return Math.min(SPOOL_MAX_CEIL_BYTES, Math.max(SPOOL_MAX_FLOOR_BYTES, parsed));
    }

    private static void recordDrop(Path dir) throws IOException {
        Path counter = dir.resolve("dropped.count");
        long dropped = 0;
        try {
            dropped = Long.parseLong(Files.readString(counter, StandardCharsets.UTF_8).strip());
        } catch (IOException | NumberFormatException first) {
            // First drop: the counter does not exist yet.
        }
        Files.writeString(counter, (dropped + 1) + "\n", StandardCharsets.UTF_8);
    }

    /**
     * Write one capsule inside the byte cap; over-cap capsules are dropped
     * and counted. Returns the file path or null.
     */
    static Path spool(Map<String, Object> payload) throws IOException {
        String body = Json.canonicalJson(payload);
        byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        Path dir = spoolDir();
        Files.createDirectories(dir);
        long used = 0;
        try (DirectoryStream<Path> entries = Files.newDirectoryStream(dir, "*.json")) {
            for (Path entry : entries) {
                try {
                    used += Files.size(entry);
                } catch (IOException removed) {
                    // A concurrently removed entry counts as zero.
                }
            }
        }
        if (used + bytes.length > spoolMaxBytes()) {
            DROPPED.incrementAndGet();
            recordDrop(dir);
            return null;
        }
        String digest = Exchange.sha256Hex(bytes).substring(0, 12);
        Path file = dir.resolve("capsule-" + digest + ".json");
        Files.write(file, bytes);
        SPOOLED.incrementAndGet();
        Map<String, Object> marker = new LinkedHashMap<>();
        marker.put("file", file.toString());
        marker.put("operation", payload.get("operation"));
        System.err.print(SPOOL_MARKER + Json.orderedJson(marker) + "\n");
        return file;
    }

    private static void finishAndSpool(BackendTrace trace, String operation, Throwable error) {
        try {
            Map<String, Object> output = new LinkedHashMap<>();
            output.put("error", boundedError(error));
            trace.finish(output, null, false, false);
            Map<String, Object> first = trace.events().get(0);
            Long observedAt = first.get("at") instanceof Number at ? at.longValue() : null;
            Map<String, Object> payload = new LinkedHashMap<>();
            payload.put("format", Capture.CAPTURE_FORMAT);
            payload.put("version", 2L);
            payload.put("operation", operation);
            payload.put("oracle", TEST_FAILURE_ORACLE);
            // Same envelope shape production capture records; the seed pins
            // the REPLAY run's randomness, it does not reproduce the test
            // run's.
            payload.put("envelope", Capture.determinismEnvelope(observedAt));
            payload.put("events", trace.events());
            spool(payload);
        } catch (RuntimeException | IOException ignored) {
            // Capture must never mask the test's own failure.
            FAILED_CAPTURES.incrementAndGet();
        }
    }

    /** A test body; Throwable so JUnit assertion errors pass through raw. */
    public interface TestBody {
        void run() throws Throwable;
    }

    // Instrument.scope takes a Callable (throws Exception); AssertionError is
    // an Error, so every Throwable rides a carrier through the scope and is
    // unwrapped on the other side.
    private static final class Carrier extends RuntimeException {
        Carrier(Throwable cause) {
            super(cause);
        }
    }

    private static void scoped(BackendTrace trace, TestBody body) throws Throwable {
        try {
            Instrument.scope(trace, () -> {
                try {
                    body.run();
                } catch (Throwable failure) {
                    throw new Carrier(failure);
                }
                return null;
            });
        } catch (Carrier carried) {
            throw carried.getCause();
        }
    }

    /**
     * Run one test under capture mode: the test gets its own trace (so the
     * wrapped clients record exchanges), and a failure spools the capsule
     * before rethrowing. The failure always propagates to the runner.
     */
    static void captureRun(String suiteName, String testName, TestBody body) throws Throwable {
        String operation = operationFor(suiteName, testName);
        Map<String, Object> input = new LinkedHashMap<>();
        input.put("suite", boundedName(suiteName));
        input.put("test", boundedName(testName));
        BackendTrace trace = BackendTrace.begin(
            ciContext(), operation, new BackendTrace.Options().input(input));
        try {
            scoped(trace, body);
        } catch (Throwable failure) {
            finishAndSpool(trace, operation, failure);
            throw failure;
        }
        try {
            trace.finish(null, null, true, false);
        } catch (RuntimeException ignored) {
            // An over-long passing trace has nothing to spool anyway.
        }
    }

    /**
     * The capsule names exactly one test; everything else is skipped so the
     * process exit code speaks for the named test alone.
     */
    static String replayTarget() {
        String path = replayPath();
        if (path == null) throw new IllegalStateException("REPROIT_REPLAY is not set");
        String operation;
        try {
            Object parsed = Json.parse(Files.readString(Path.of(path), StandardCharsets.UTF_8));
            operation = parsed instanceof Map<?, ?> payload
                && payload.get("operation") instanceof String value ? value : null;
        } catch (IOException | RuntimeException unusable) {
            operation = null;
        }
        if (operation == null || !operation.startsWith(TEST_TRIGGER_PREFIX)) {
            throw new IllegalStateException(
                "REPROIT_REPLAY capsule does not carry a test trigger identity");
        }
        return operation;
    }

    private static void reportResult(String operation, String status, Throwable error) {
        Map<String, Object> detail = new LinkedHashMap<>();
        detail.put("operation", operation);
        detail.put("status", status);
        if (error != null) detail.put("failure", boundedError(error));
        System.err.print(RESULT_MARKER + Json.orderedJson(detail) + "\n");
    }

    /**
     * Run the replay target test and report the observed result marker. The
     * caller has already checked the operation matches {@link #replayTarget}.
     */
    static void replayRun(String suiteName, String testName, TestBody body) throws Throwable {
        String operation = operationFor(suiteName, testName);
        try {
            body.run();
        } catch (Throwable failure) {
            reportResult(operation, "failed", failure);
            throw failure;
        }
        reportResult(operation, "passed", null);
    }

    /**
     * The dependency-free micro-runner: `Ci.suite("checkout").test(name, body)`
     * runs each test immediately in the active mode, and `exitCode()` speaks
     * for the run (in replay mode, for the named test alone). The mode is
     * decided once, at suite() time, like the Node reference.
     */
    public static Suite suite(String suiteName) {
        Mode active = mode();
        return new Suite(
            suiteName, active, active == Mode.REPLAY ? replayTarget() : null);
    }

    public static final class Suite {
        private final String name;
        private final Mode mode;
        private final String target;
        private int failures = 0;

        private Suite(String name, Mode mode, String target) {
            this.name = name;
            this.mode = mode;
            this.target = target;
        }

        public void test(String testName, TestBody body) {
            String operation = operationFor(name, testName);
            if (mode == Mode.REPLAY && !operation.equals(target)) return;
            try {
                switch (mode) {
                    case CAPTURE -> captureRun(name, testName, body);
                    case REPLAY -> replayRun(name, testName, body);
                    default -> body.run();
                }
            } catch (Throwable failure) {
                failures += 1;
                System.err.println("not ok " + operation);
                failure.printStackTrace();
            }
        }

        /** 0 when every executed test passed; 1 otherwise. */
        public int exitCode() {
            return failures == 0 ? 0 : 1;
        }
    }
}
