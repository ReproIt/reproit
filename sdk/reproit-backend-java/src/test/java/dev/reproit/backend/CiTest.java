// CI capture mode (Ci.java), mirroring the Node reference's ci.test.js: a
// failing test spools a test-trigger capsule with the recorded exchange, a
// replay run re-executes only the named test and reports the structured
// result marker, the spool cap drops loudly, and without either env the
// runner is untouched. Scenarios drive the env seam (Ci.environment) and the
// replay session seam (Instrument.resetSessionForTest) instead of spawning
// child JVMs; mode is decided at suite() time, exactly like the reference.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CiTest {
    @TempDir
    Path work;

    private Map<String, String> savedEnvironment;
    private PrintStream savedErr;
    private ByteArrayOutputStream stderr;

    @BeforeEach
    void isolate() {
        savedEnvironment = Ci.environment;
        Ci.resetStatsForTest();
        savedErr = System.err;
        stderr = new ByteArrayOutputStream();
        System.setErr(new PrintStream(stderr, true, StandardCharsets.UTF_8));
        Instrument.resetSessionForTest(null);
    }

    @AfterEach
    void restore() {
        Ci.environment = savedEnvironment;
        System.setErr(savedErr);
        Instrument.resetSessionForTest(null);
    }

    private String stderrText() {
        return stderr.toString(StandardCharsets.UTF_8);
    }

    /** One upstream stub answering {"n":7}, like the reference fixture's. */
    private static HttpServer upstream() throws Exception {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/n", exchange -> {
            byte[] body = "{\"n\":7}".getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            exchange.getResponseBody().write(body);
            exchange.close();
        });
        server.start();
        return server;
    }

    /** The suite under test: one upstream call, one assertion on the answer. */
    private static void assertsTheUpstreamAnswer(String url, long expected) throws Throwable {
        HttpClient client = HttpClient.newHttpClient();
        Instrument.Http.ExchangeResponse response = Instrument.Http.send(
            client, HttpRequest.newBuilder(URI.create(url)).GET().build());
        Object n = ((Map<?, ?>) response.json()).get("n");
        long got = ((Number) n).longValue();
        if (got != expected) {
            throw new AssertionError("upstream answered " + got + ", expected " + expected);
        }
    }

    private Path capturedCapsule(String url) throws Exception {
        Ci.environment = Map.of(
            "REPROIT_CI_CAPTURE", "1", "REPROIT_CI_SPOOL", work.resolve("spool").toString());
        Ci.Suite suite = Ci.suite("unit");
        suite.test("asserts the upstream answer", () -> assertsTheUpstreamAnswer(url, 8));
        assertEquals(1, suite.exitCode());
        List<Path> files;
        try (var entries = Files.list(work.resolve("spool"))) {
            files = entries.filter(
                path -> path.getFileName().toString().startsWith("capsule-")).toList();
        }
        assertEquals(1, files.size());
        return files.get(0);
    }

    @Test
    @SuppressWarnings("unchecked")
    void aFailingTestSpoolsATestTriggerCapsuleWithTheExchange() throws Exception {
        HttpServer server = upstream();
        try {
            String url = "http://127.0.0.1:" + server.getAddress().getPort() + "/n";
            Path file = capturedCapsule(url);
            assertTrue(stderrText().contains(Ci.SPOOL_MARKER), stderrText());
            Map<String, Object> capsule = (Map<String, Object>) Json.parse(
                Files.readString(file, StandardCharsets.UTF_8));
            assertEquals("reproit-backend-capture", capsule.get("format"));
            assertEquals(2L, ((Number) capsule.get("version")).longValue());
            assertEquals("test:unit#asserts the upstream answer", capsule.get("operation"));
            assertEquals(Ci.TEST_FAILURE_ORACLE, capsule.get("oracle"));
            Map<String, Object> envelope = (Map<String, Object>) capsule.get("envelope");
            assertNotNull(envelope.get("replaySeed"));
            List<Map<String, Object>> events =
                (List<Map<String, Object>>) capsule.get("events");
            List<Map<String, Object>> exchanges = events.stream()
                .filter(event -> event.get("exchange") instanceof Map<?, ?>).toList();
            assertEquals(1, exchanges.size());
            Map<String, Object> exchange =
                (Map<String, Object>) exchanges.get(0).get("exchange");
            Map<String, Object> response = (Map<String, Object>) exchange.get("response");
            assertEquals(Map.of("n", 7L), response.get("body"));
            Map<String, Object> returned = events.get(events.size() - 1);
            assertEquals(Boolean.FALSE, returned.get("success"));
            Map<String, Object> output = (Map<String, Object>) returned.get("output");
            assertEquals("upstream answered 7, expected 8", output.get("error"));
        } finally {
            server.stop(0);
        }
    }

    @Test
    void replayRerunsTheNamedTestAndReportsFailedThenPassed() throws Exception {
        HttpServer server = upstream();
        Path file;
        String url = "http://127.0.0.1:" + server.getAddress().getPort() + "/n";
        try {
            file = capturedCapsule(url);
        } finally {
            // No upstream exists in either replay run; the SDK serves the
            // recording.
            server.stop(0);
        }
        stderr.reset();

        Ci.environment = Map.of("REPROIT_REPLAY", file.toString());
        Instrument.resetSessionForTest(Replay.load(file.toString()));
        Ci.Suite failed = Ci.suite("unit");
        failed.test("skipped by replay", () -> {
            throw new AssertionError("must not run");
        });
        failed.test("asserts the upstream answer", () -> assertsTheUpstreamAnswer(url, 8));
        assertEquals(1, failed.exitCode());
        String failedLine = stderrText().lines()
            .filter(line -> line.startsWith(Ci.RESULT_MARKER)).findFirst().orElseThrow();
        @SuppressWarnings("unchecked")
        Map<String, Object> report = (Map<String, Object>) Json.parse(
            failedLine.substring(Ci.RESULT_MARKER.length()));
        assertEquals("failed", report.get("status"));
        assertEquals("test:unit#asserts the upstream answer", report.get("operation"));
        assertEquals("upstream answered 7, expected 8", report.get("failure"));

        stderr.reset();
        Instrument.resetSessionForTest(Replay.load(file.toString()));
        Ci.Suite passed = Ci.suite("unit");
        passed.test("asserts the upstream answer", () -> assertsTheUpstreamAnswer(url, 7));
        assertEquals(0, passed.exitCode());
        assertTrue(stderrText().contains("\"status\":\"passed\""), stderrText());
    }

    @Test
    void aFullSpoolDropsTheCapsuleAndCountsTheDrop() throws Exception {
        Path spool = work.resolve("full");
        Files.createDirectories(spool);
        // Pre-fill the spool to the floor cap so the next capsule cannot fit.
        Files.writeString(spool.resolve("existing.json"), "x".repeat(4 * 1024));
        Ci.environment = Map.of(
            "REPROIT_CI_CAPTURE", "1",
            "REPROIT_CI_SPOOL", spool.toString(),
            "REPROIT_CI_SPOOL_MAX", String.valueOf(4 * 1024));
        Ci.Suite suite = Ci.suite("unit");
        suite.test("fails without dependencies", () -> {
            throw new AssertionError("planted");
        });
        assertEquals(1, suite.exitCode());
        try (var entries = Files.list(spool)) {
            assertTrue(entries.noneMatch(
                path -> path.getFileName().toString().startsWith("capsule-")));
        }
        assertEquals("1", Files.readString(spool.resolve("dropped.count")).strip());
        assertEquals(1, Ci.stats().droppedCapsules());
    }

    @Test
    void withoutCaptureOrReplayEnvTheRunnerIsUntouched() {
        Ci.environment = Map.of();
        Ci.Suite suite = Ci.suite("unit");
        suite.test("fails plainly", () -> {
            throw new AssertionError("plain failure");
        });
        assertEquals(1, suite.exitCode());
        assertFalse(stderrText().contains(Ci.SPOOL_MARKER));
        assertFalse(stderrText().contains(Ci.RESULT_MARKER));
    }
}
