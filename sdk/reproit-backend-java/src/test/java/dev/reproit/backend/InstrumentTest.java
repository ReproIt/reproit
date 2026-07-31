// Exchange capture and hermetic replay: bounds, redaction inside exchange
// bodies, the ambient-trace boundary, strict matching, and the structured
// divergence marker.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import java.io.PrintStream;
import java.io.ByteArrayOutputStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

class InstrumentTest {
    private static final TraceContext CAPTURE_CONTEXT =
        new TraceContext("cap-1", null, 0, null, null, true);

    @AfterEach
    void clearSession() {
        Instrument.resetSessionForTest(null);
    }

    private static BackendTrace trace() {
        return BackendTrace.begin(CAPTURE_CONTEXT, "GET /quote", new BackendTrace.Options()
            .input(Map.of("query", Map.of("symbol", "ACME"))));
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> exchangeOf(BackendTrace trace) {
        for (Map<String, Object> event : trace.events()) {
            if (event.get("exchange") instanceof Map<?, ?> exchange) {
                return (Map<String, Object>) exchange;
            }
        }
        return null;
    }

    @Test
    void httpExchangesRecordRequestAndResponseOnTheAmbientTrace() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/prices", exchange -> {
            byte[] reply = "{\"prices\":null,\"apiKey\":\"sk-live-secret\"}"
                .getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, reply.length);
            exchange.getResponseBody().write(reply);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        BackendTrace trace = trace();
        try {
            Instrument.scope(trace, () -> Instrument.Http.send(
                HttpClient.newHttpClient(),
                HttpRequest.newBuilder(URI.create(base + "/prices?tier=gold")).GET().build()));
        } finally {
            upstream.stop(0);
        }
        Map<String, Object> exchange = exchangeOf(trace);
        assertNotNull(exchange, "expected a recorded exchange");
        assertEquals("http", exchange.get("protocol"));
        Map<?, ?> request = (Map<?, ?>) exchange.get("request");
        assertEquals("GET", request.get("method"));
        Map<?, ?> response = (Map<?, ?>) exchange.get("response");
        assertEquals(200L, response.get("status"));
        Map<?, ?> body = (Map<?, ?>) response.get("body");
        assertNull(body.get("prices"));
        // Structural redaction applies INSIDE captured exchange bodies.
        assertEquals(
            true, ((Map<?, ?>) ((Map<?, ?>) body.get("apiKey")).get("$reproit")).get("redacted"));
    }

    @Test
    void anUnscopedCallIsNotRecordedRatherThanHalfRecorded() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/ping", exchange -> {
            exchange.sendResponseHeaders(204, -1);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        BackendTrace trace = trace();
        try {
            Instrument.Http.ExchangeResponse response = Instrument.Http.send(
                HttpClient.newHttpClient(),
                HttpRequest.newBuilder(URI.create(base + "/ping")).GET().build());
            assertEquals(204, response.status());
        } finally {
            upstream.stop(0);
        }
        assertNull(exchangeOf(trace), "an unscoped call must not attach to a trace");
    }

    @Test
    void oversizedBodiesKeepProvableIdentityOnly() {
        byte[] big = new byte[Exchange.MAX_EXCHANGE_BODY_BYTES + 1];
        java.util.Arrays.fill(big, (byte) 'x');
        Map<String, Object> bounded = Exchange.boundedBody(big, "text/plain");
        assertEquals(Boolean.TRUE, bounded.get("truncated"));
        assertEquals((long) big.length, bounded.get("bodyBytes"));
        assertEquals(64, String.valueOf(bounded.get("bodySha256")).length());
        assertFalse(bounded.containsKey("body"));
    }

    @Test
    void headersAreCappedAndLowercased() {
        Map<String, String> headers = new java.util.LinkedHashMap<>();
        for (int index = 0; index < Exchange.MAX_EXCHANGE_HEADERS + 5; index++) {
            headers.put("X-Header-" + index, "value-" + index);
        }
        Map<?, ?> bounded = (Map<?, ?>) Exchange.boundedHeaders(headers).get("headers");
        assertEquals(Exchange.MAX_EXCHANGE_HEADERS, bounded.size());
        assertTrue(bounded.containsKey("x-header-0"));
    }

    @Test
    void dbStatementsRecordRowsAndErrors() throws Exception {
        BackendTrace trace = trace();
        Instrument.scope(trace, () -> Instrument.Db.run(
            "SELECT id FROM issuers WHERE symbol = $1",
            List.of("ACME"),
            () -> new Instrument.Db.Outcome("SELECT", 1, List.of(Map.of("id", 7L)))));
        Map<String, Object> exchange = exchangeOf(trace);
        assertNotNull(exchange);
        assertEquals("pg", exchange.get("protocol"));
        Map<?, ?> response = (Map<?, ?>) exchange.get("response");
        assertEquals(1L, response.get("rowCount"));
        assertEquals(List.of(Map.of("id", 7L)), response.get("rows"));
        // A read stays a read so state oracles keep their meaning.
        assertEquals("read", trace.events().stream()
            .filter(event -> event.containsKey("exchange"))
            .findFirst().orElseThrow().get("effect"));
    }

    @Test
    void replayServesRecordedExchangesWithoutTouchingTheNetwork() throws Exception {
        Path capture = writeCapture();
        Instrument.resetSessionForTest(Replay.load(capture.toString()));
        assertTrue(Instrument.replaying());
        // The port is closed: any real socket attempt would fail.
        Instrument.Http.ExchangeResponse response = Instrument.Http.send(
            HttpClient.newHttpClient(),
            HttpRequest.newBuilder(URI.create("http://pricing.internal/prices?tier=gold"))
                .GET().build());
        assertEquals(200, response.status());
        assertEquals(Map.of("prices", List.of(1L, 2L)), response.json());
        Instrument.Db.Outcome outcome = Instrument.Db.run(
            "SELECT id FROM issuers WHERE symbol = $1",
            List.of("ACME"),
            () -> {
                throw new IllegalStateException("replay must never reach the live statement");
            });
        assertEquals(1, outcome.rowCount());
    }

    @Test
    void anUnmatchedCallDivergesWithTheStructuredMarker() throws Exception {
        Path capture = writeCapture();
        Instrument.resetSessionForTest(Replay.load(capture.toString()));
        PrintStream original = System.err;
        ByteArrayOutputStream captured = new ByteArrayOutputStream();
        System.setErr(new PrintStream(captured, true, StandardCharsets.UTF_8));
        Instrument.Http.ExchangeResponse response;
        try {
            response = Instrument.Http.send(
                HttpClient.newHttpClient(),
                HttpRequest.newBuilder(URI.create("http://pricing.internal/unknown"))
                    .GET().build());
        } finally {
            System.setErr(original);
        }
        assertEquals(599, response.status());
        String marker = captured.toString(StandardCharsets.UTF_8);
        assertTrue(marker.startsWith(Replay.DIVERGENCE_MARKER), marker);
        Map<?, ?> report = (Map<?, ?>) Json.parse(
            marker.substring(Replay.DIVERGENCE_MARKER.length()).trim());
        assertEquals("http", report.get("protocol"));
        assertEquals(2L, report.get("total"));
    }

    @Test
    void aDivergedDbCallThrowsRatherThanGuessing() throws Exception {
        Path capture = writeCapture();
        Instrument.resetSessionForTest(Replay.load(capture.toString()));
        PrintStream original = System.err;
        System.setErr(new PrintStream(new ByteArrayOutputStream(), true, StandardCharsets.UTF_8));
        try {
            assertThrows(Instrument.Db.DbException.class, () -> Instrument.Db.run(
                "SELECT something else", List.of(), () -> new Instrument.Db.Outcome(null, 0,
                    List.of())));
        } finally {
            System.setErr(original);
        }
    }

    @Test
    void redactionPlaceholdersMatchAnyLiveValue() {
        Object recorded = Map.of("password", Map.of("$reproit",
            Map.of("redacted", true, "type", "string", "length", 8L)));
        assertTrue(Replay.matches(recorded, Map.of("password", "anything-at-all")));
        assertFalse(Replay.matches(Map.of("item", "widget"), Map.of("item", "gadget")));
    }

    @Test
    void theEnvelopeSeedYieldsAStableStream() throws Exception {
        Path capture = writeCapture();
        Replay session = Replay.load(capture.toString());
        assertNotNull(session);
        Replay.Rng first = session.rng();
        Replay.Rng second = session.rng();
        assertNotNull(first);
        double left = first.nextDouble();
        double right = second.nextDouble();
        assertEquals(left, right);
        assertTrue(left >= 0 && left < 1);
    }

    // A version-2 capture carrying one http and one pg exchange.
    private static Path writeCapture() throws Exception {
        String payload = """
            {
              "format": "reproit-backend-capture",
              "version": 2,
              "operation": "GET /quote",
              "oracle": "backend-server-error",
              "envelope": {
                "observedAtMs": 1753747200000,
                "tz": "Europe/Berlin",
                "replaySeed": "00ff00ff00ff00ff"
              },
              "events": [
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":1,"kind":"start","input":{}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":2,"kind":"effect","effect":"read","resource":"pg",
                 "exchange":{"protocol":"pg",
                   "request":{"text":"SELECT id FROM issuers WHERE symbol = $1",
                              "values":["ACME"]},
                   "response":{"command":"SELECT","rowCount":1,"rows":[{"id":7}]}}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":3,"kind":"effect","effect":"call","resource":"pricing",
                 "exchange":{"protocol":"http",
                   "request":{"method":"GET","url":"http://pricing.internal/prices?tier=gold"},
                   "response":{"status":200,"headers":{"content-type":"application/json"},
                               "body":{"prices":[1,2]}}}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":4,"kind":"return","output":{},"status":500,"success":false,
                 "effectsComplete":true}
              ]
            }
            """;
        Path file = Files.createTempFile("reproit-java-replay", ".json");
        Files.writeString(file, payload, StandardCharsets.UTF_8);
        file.toFile().deleteOnExit();
        return file;
    }

    @Test
    void propagateCarriesTheTraceOntoAPoolThread() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/prices", exchange -> {
            byte[] reply = "{\"prices\":[1,2]}".getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, reply.length);
            exchange.getResponseBody().write(reply);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        ExecutorService pool = Executors.newSingleThreadExecutor();
        BackendTrace trace = trace();
        try {
            // Wrap at SUBMISSION time, on the request thread, so the pool
            // thread inherits the ambient trace.
            Instrument.scopeRun(trace, () -> pool.submit(Instrument.propagate(() ->
                Instrument.Http.send(
                    HttpClient.newHttpClient(),
                    HttpRequest.newBuilder(URI.create(base + "/prices")).GET().build())))
                .get(30, TimeUnit.SECONDS));
        } finally {
            pool.shutdownNow();
            upstream.stop(0);
        }
        Map<String, Object> exchange = exchangeOf(trace);
        assertNotNull(exchange, "a call on a pool thread must attach to the originating trace");
        assertEquals(200L, ((Map<?, ?>) exchange.get("response")).get("status"));
    }

    @Test
    void aPropagatingExecutorNeedsNoPerCallWrapping() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/ping", exchange -> {
            exchange.sendResponseHeaders(204, -1);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        ExecutorService pool = Executors.newSingleThreadExecutor();
        BackendTrace trace = trace();
        java.util.concurrent.CountDownLatch done = new java.util.concurrent.CountDownLatch(1);
        try {
            Instrument.scopeRun(trace, () -> {
                Instrument.propagate((java.util.concurrent.Executor) pool).execute(() -> {
                    try {
                        Instrument.Http.send(
                            HttpClient.newHttpClient(),
                            HttpRequest.newBuilder(URI.create(base + "/ping")).GET().build());
                    } catch (Exception ignored) {
                        // The assertion below is what fails the test.
                    } finally {
                        done.countDown();
                    }
                });
                assertTrue(done.await(30, TimeUnit.SECONDS));
            });
        } finally {
            pool.shutdownNow();
            upstream.stop(0);
        }
        assertNotNull(exchangeOf(trace), "the propagating executor must carry the trace");
    }

    @Test
    void propagateWithoutAnAmbientTraceRecordsNothing() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/ping", exchange -> {
            exchange.sendResponseHeaders(204, -1);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        ExecutorService pool = Executors.newSingleThreadExecutor();
        BackendTrace trace = trace();
        try {
            // No scope around the submission: propagate is an identity wrapper
            // and the call stays unrecorded rather than half-recorded.
            pool.submit(Instrument.propagate(() -> Instrument.Http.send(
                HttpClient.newHttpClient(),
                HttpRequest.newBuilder(URI.create(base + "/ping")).GET().build())))
                .get(30, TimeUnit.SECONDS);
        } finally {
            pool.shutdownNow();
            upstream.stop(0);
        }
        assertNull(exchangeOf(trace));
    }
}
