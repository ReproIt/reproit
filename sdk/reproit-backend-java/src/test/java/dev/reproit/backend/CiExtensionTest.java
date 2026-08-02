// The JUnit 5 integration surface of CI capture mode: @ExtendWith(ReproitCi)
// gives real JUnit tests the same capture/replay semantics CiTest pins for
// the micro-runner. The fixture class is a nested static class NOT matching
// surefire's discovery patterns, executed through the embedded platform
// launcher so each scenario states its mode via the env seam first.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.platform.engine.discovery.DiscoverySelectors.selectClass;

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
import org.junit.jupiter.api.extension.ExtendWith;
import org.junit.jupiter.api.io.TempDir;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;
import org.junit.platform.launcher.listeners.SummaryGeneratingListener;
import org.junit.platform.launcher.listeners.TestExecutionSummary;

class CiExtensionTest {
    @TempDir
    Path work;

    private Map<String, String> savedEnvironment;
    private PrintStream savedErr;
    private ByteArrayOutputStream stderr;

    // The fixture talks to whatever upstream the running scenario booted.
    static String upstreamUrl;

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

    /**
     * The extended suite. Named to dodge surefire's *Test discovery patterns:
     * it must only run through the launcher below, after a scenario has
     * stated its mode.
     */
    @ExtendWith(ReproitCi.class)
    static class ExtensionFixture {
        @Test
        void assertsTheUpstreamAnswer() throws Exception {
            HttpClient client = HttpClient.newHttpClient();
            Instrument.Http.ExchangeResponse response = Instrument.Http.send(
                client, HttpRequest.newBuilder(URI.create(upstreamUrl)).GET().build());
            long got = ((Number) ((Map<?, ?>) response.json()).get("n")).longValue();
            if (got != 8) {
                throw new AssertionError("upstream answered " + got + ", expected 8");
            }
        }

        @Test
        void unrelatedTestPasses() {
            // Present so replay mode has something to skip.
        }
    }

    private static TestExecutionSummary launch() {
        LauncherDiscoveryRequest request = LauncherDiscoveryRequestBuilder.request()
            .selectors(selectClass(ExtensionFixture.class))
            .build();
        SummaryGeneratingListener listener = new SummaryGeneratingListener();
        LauncherFactory.create().execute(request, listener);
        return listener.getSummary();
    }

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

    private Path captureRun() throws Exception {
        Ci.environment = Map.of(
            "REPROIT_CI_CAPTURE", "1", "REPROIT_CI_SPOOL", work.resolve("spool").toString());
        TestExecutionSummary summary = launch();
        assertEquals(1, summary.getTestsFailedCount());
        assertEquals(2, summary.getTestsStartedCount());
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
    void aFailingJunitTestSpoolsTheCapsuleUnderClassAndMethodIdentity() throws Exception {
        HttpServer server = upstream();
        try {
            upstreamUrl = "http://127.0.0.1:" + server.getAddress().getPort() + "/n";
            Path file = captureRun();
            Map<String, Object> capsule = (Map<String, Object>) Json.parse(
                Files.readString(file, StandardCharsets.UTF_8));
            assertEquals(
                "test:ExtensionFixture#assertsTheUpstreamAnswer", capsule.get("operation"));
            assertEquals(Ci.TEST_FAILURE_ORACLE, capsule.get("oracle"));
            List<Map<String, Object>> events =
                (List<Map<String, Object>>) capsule.get("events");
            assertEquals(1, events.stream()
                .filter(event -> event.get("exchange") instanceof Map<?, ?>).count());
        } finally {
            server.stop(0);
        }
    }

    @Test
    void replayRunsOnlyTheNamedTestAndReportsTheMarker() throws Exception {
        HttpServer server = upstream();
        Path file;
        try {
            upstreamUrl = "http://127.0.0.1:" + server.getAddress().getPort() + "/n";
            file = captureRun();
        } finally {
            // The replay run has no upstream; the SDK serves the recording.
            server.stop(0);
        }
        stderr.reset();
        Ci.environment = Map.of("REPROIT_REPLAY", file.toString());
        Instrument.resetSessionForTest(Replay.load(file.toString()));
        TestExecutionSummary summary = launch();
        // Only the named test runs; the unrelated one is disabled by name.
        assertEquals(1, summary.getTestsStartedCount());
        assertEquals(1, summary.getTestsFailedCount());
        assertTrue(stderrText().contains(
            Ci.RESULT_MARKER
            + "{\"operation\":\"test:ExtensionFixture#assertsTheUpstreamAnswer\","
            + "\"status\":\"failed\""), stderrText());
    }

    private String stderrText() {
        return stderr.toString(StandardCharsets.UTF_8);
    }
}
