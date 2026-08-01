// Capsule parity surface: the delegating HttpClient (buffered, streaming,
// abandoned bodies), the delegating JDBC wrap (record, re-serve, replay
// stub, divergence), strict per-operation ordinal matching, the bodyDelta
// vocabulary, and the pinned clock and random the no-weaving boundary
// allows.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.lang.reflect.Proxy;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

class CapsuleParityTest {
    private static final TraceContext CAPTURE_CONTEXT =
        new TraceContext("cap-2", null, 0, null, null, true);

    @AfterEach
    void clearSession() {
        Instrument.resetSessionForTest(null);
    }

    private static BackendTrace trace() {
        return BackendTrace.begin(CAPTURE_CONTEXT, "GET /quote", new BackendTrace.Options());
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> exchangesOf(BackendTrace trace) {
        List<Map<String, Object>> found = new ArrayList<>();
        for (Map<String, Object> event : trace.events()) {
            if (event.get("exchange") instanceof Map<?, ?> exchange) {
                found.add((Map<String, Object>) exchange);
            }
        }
        return found;
    }

    // ------------------------------------------------------------------
    // Delegating HttpClient, capture side.
    // ------------------------------------------------------------------

    @Test
    void delegatingClientRecordsABufferedExchange() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/prices", exchange -> {
            byte[] reply = "{\"prices\":null}".getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, reply.length);
            exchange.getResponseBody().write(reply);
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        BackendTrace trace = trace();
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
        try {
            HttpResponse<String> response = Instrument.scope(trace, () -> client.send(
                HttpRequest.newBuilder(URI.create(base + "/prices?tier=gold")).GET().build(),
                HttpResponse.BodyHandlers.ofString()));
            assertEquals(200, response.statusCode());
            assertEquals("{\"prices\":null}", response.body());
        } finally {
            upstream.stop(0);
        }
        List<Map<String, Object>> exchanges = exchangesOf(trace);
        assertEquals(1, exchanges.size());
        Map<?, ?> request = (Map<?, ?>) exchanges.get(0).get("request");
        assertEquals("GET", request.get("method"));
        assertTrue(String.valueOf(request.get("url")).endsWith("/prices?tier=gold"));
        Map<?, ?> response = (Map<?, ?>) exchanges.get(0).get("response");
        assertEquals(200L, response.get("status"));
        Map<?, ?> body = (Map<?, ?>) response.get("body");
        assertTrue(body.containsKey("prices"));
        assertNull(body.get("prices"));
    }

    @Test
    void sseStreamRecordsChunkBoundariesAtEofThroughTheTee() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/stream", exchange -> {
            exchange.getResponseHeaders().set("Content-Type", "text/event-stream");
            exchange.sendResponseHeaders(200, 0);
            var out = exchange.getResponseBody();
            for (String part : List.of("data: a\n\n", "data: b\n\n", "data: c\n\n")) {
                out.write(part.getBytes(StandardCharsets.UTF_8));
                out.flush();
                try {
                    Thread.sleep(40);
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                }
            }
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        BackendTrace trace = trace();
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
        try {
            // The app consumes the stream itself (ofLines); recording rides
            // the same subscription and lands at EOF.
            List<String> lines = Instrument.scope(trace, () -> client.send(
                HttpRequest.newBuilder(URI.create(base + "/stream")).GET().build(),
                HttpResponse.BodyHandlers.ofLines()).body().collect(Collectors.toList()));
            assertEquals(6, lines.size());
        } finally {
            upstream.stop(0);
        }
        List<Map<String, Object>> exchanges = exchangesOf(trace);
        assertEquals(1, exchanges.size());
        Map<?, ?> response = (Map<?, ?>) exchanges.get(0).get("response");
        assertEquals("data: a\n\ndata: b\n\ndata: c\n\n", response.get("body"));
        Map<?, ?> stream = (Map<?, ?>) response.get("stream");
        assertNotNull(stream, "an SSE response must record its stream shape");
        List<?> chunks = (List<?>) stream.get("chunks");
        long total = chunks.stream().mapToLong(c -> ((Number) c).longValue()).sum();
        assertEquals(27L, total, "boundaries must cover every observed byte");
        assertTrue(chunks.size() >= 2, "flushed SSE chunks must keep their boundaries");
    }

    @Test
    void anAbandonedBodyRecordsNothing() throws Exception {
        HttpServer upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        upstream.createContext("/slow", exchange -> {
            exchange.getResponseHeaders().set("Content-Type", "text/event-stream");
            exchange.sendResponseHeaders(200, 0);
            var out = exchange.getResponseBody();
            out.write("data: a\n\n".getBytes(StandardCharsets.UTF_8));
            out.flush();
            try {
                Thread.sleep(200);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            }
            exchange.close();
        });
        upstream.start();
        String base = "http://127.0.0.1:" + upstream.getAddress().getPort();
        BackendTrace trace = trace();
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
        try {
            HttpResponse<java.io.InputStream> response = Instrument.scope(
                trace, () -> client.send(
                    HttpRequest.newBuilder(URI.create(base + "/slow")).GET().build(),
                    HttpResponse.BodyHandlers.ofInputStream()));
            // The app walks away without reading: no EOF, no exchange.
            response.body().close();
        } finally {
            upstream.stop(0);
        }
        assertEquals(0, exchangesOf(trace).size(),
            "an abandoned body must record nothing, never a half exchange");
    }

    // ------------------------------------------------------------------
    // Delegating HttpClient, replay side.
    // ------------------------------------------------------------------

    @Test
    void replayServesTheRecordedStreamChunkForChunk() throws Exception {
        Instrument.resetSessionForTest(Replay.load(writeStreamCapture().toString()));
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
        List<Integer> observed = new ArrayList<>();
        HttpResponse<String> response = client.send(
            HttpRequest.newBuilder(URI.create("http://llm.internal/stream")).GET().build(),
            info -> {
                HttpResponse.BodySubscriber<String> down =
                    HttpResponse.BodySubscribers.ofString(StandardCharsets.UTF_8);
                return new HttpResponse.BodySubscriber<String>() {
                    @Override
                    public void onSubscribe(java.util.concurrent.Flow.Subscription s) {
                        down.onSubscribe(s);
                    }

                    @Override
                    public void onNext(List<java.nio.ByteBuffer> item) {
                        observed.add(item.stream().mapToInt(
                            java.nio.ByteBuffer::remaining).sum());
                        down.onNext(item);
                    }

                    @Override
                    public void onError(Throwable t) {
                        down.onError(t);
                    }

                    @Override
                    public void onComplete() {
                        down.onComplete();
                    }

                    @Override
                    public java.util.concurrent.CompletionStage<String> getBody() {
                        return down.getBody();
                    }
                };
            });
        assertEquals(200, response.statusCode());
        assertEquals("data: a\n\ndata: b\n\ndata: c\n\n", response.body());
        assertEquals(List.of(9, 9, 9), observed,
            "the app must observe the recorded chunk boundaries");
    }

    @Test
    void replayDivergenceAnswers599WithTheOrderedMarker() throws Exception {
        Instrument.resetSessionForTest(Replay.load(writeStreamCapture().toString()));
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
        PrintStream original = System.err;
        ByteArrayOutputStream held = new ByteArrayOutputStream();
        System.setErr(new PrintStream(held, true, StandardCharsets.UTF_8));
        HttpResponse<String> response;
        try {
            response = client.send(
                HttpRequest.newBuilder(URI.create("http://llm.internal/other")).GET().build(),
                HttpResponse.BodyHandlers.ofString());
        } finally {
            System.setErr(original);
        }
        assertEquals(599, response.statusCode());
        assertEquals("{\"reproit\":\"diverged\"}", response.body());
        String marker = held.toString(StandardCharsets.UTF_8).lines()
            .filter(line -> line.startsWith(Replay.DIVERGENCE_MARKER))
            .findFirst().orElseThrow();
        // Field order is INSERTION order, the byte-compared contract.
        assertTrue(marker.startsWith(
            Replay.DIVERGENCE_MARKER + "{\"protocol\":\"http\",\"got\":"), marker);
    }

    // ------------------------------------------------------------------
    // bodyDelta vocabulary.
    // ------------------------------------------------------------------

    @Test
    void bodyDeltaNamesTheFirstDifferingMessageIndex() {
        Map<String, Object> recorded = Map.of("messages", List.of(
            Map.of("role", "user", "content", "hello"),
            Map.of("role", "assistant", "content", "hi"),
            Map.of("role", "user", "content", "weather?")));
        Map<String, Object> live = Map.of("messages", List.of(
            Map.of("role", "user", "content", "hello"),
            Map.of("role", "assistant", "content", "hi"),
            Map.of("role", "user", "content", "DIFFERENT")));
        Map<String, Object> delta = Replay.bodyDelta(recorded, live);
        assertEquals("message", delta.get("kind"));
        assertEquals(2L, delta.get("firstDifferingMessage"));
        assertEquals(3L, delta.get("recordedMessages"));
        assertEquals(3L, delta.get("liveMessages"));
    }

    @Test
    void bodyDeltaFallsBackToTheByteOffset() {
        Map<String, Object> delta = Replay.bodyDelta("abcdef", "abcXef");
        assertEquals("byte", delta.get("kind"));
        assertEquals(3L, delta.get("offset"));
    }

    @Test
    void bodyDeltaIsSilentWhenEitherBodyIsAbsentButNotForNull() {
        assertNull(Replay.bodyDelta(Replay.ABSENT, "anything"));
        assertNull(Replay.bodyDelta("anything", Replay.ABSENT));
        // A recorded JSON null wildcards, so no delta either; but ABSENT and
        // null must stay DIFFERENT claims: null is a matched value.
        assertNull(Replay.bodyDelta(null, "anything"));
        assertNotNull(Replay.bodyDelta("x", "y"));
    }

    // ------------------------------------------------------------------
    // Strict per-operation ordinals.
    // ------------------------------------------------------------------

    @Test
    void interleavedOperationsConsumeTheirOwnOrdinals() throws Exception {
        // pg A, http X, pg A again: the second A must serve the SECOND
        // recorded response even though X sits between them.
        String payload = """
            {"format":"reproit-backend-capture","version":2,"operation":"GET /q",
             "oracle":"backend-server-error","events":[
              {"kind":"effect","sequence":1,"exchange":{"protocol":"pg",
               "request":{"text":"SELECT n"},
               "response":{"command":"SELECT","rowCount":1,"rows":[{"n":1}]}}},
              {"kind":"effect","sequence":2,"exchange":{"protocol":"http",
               "request":{"method":"GET","url":"http://svc/x"},
               "response":{"status":200,"body":"ok"}}},
              {"kind":"effect","sequence":3,"exchange":{"protocol":"pg",
               "request":{"text":"SELECT n"},
               "response":{"command":"SELECT","rowCount":1,"rows":[{"n":2}]}}}
             ]}""";
        Path file = Files.createTempFile("reproit-ordinal", ".json");
        Files.writeString(file, payload, StandardCharsets.UTF_8);
        file.toFile().deleteOnExit();
        Replay session = Replay.load(file.toString());
        Map<String, Object> first = session.matched("pg", Map.of("text", "SELECT n"));
        Map<String, Object> second = session.matched("pg", Map.of("text", "SELECT n"));
        assertEquals(
            List.of(Map.of("n", 1L)), ((Map<?, ?>) first.get("response")).get("rows"));
        assertEquals(
            List.of(Map.of("n", 2L)), ((Map<?, ?>) second.get("response")).get("rows"));
        Map<String, Object> probe = new LinkedHashMap<>();
        probe.put("method", "GET");
        probe.put("url", "http://svc/x");
        assertNotNull(session.matched("http", probe));
    }

    // ------------------------------------------------------------------
    // Delegating JDBC wrap.
    // ------------------------------------------------------------------

    /** A fake driver connection that must never be reached during replay. */
    private static Connection fakeLiveConnection() {
        return (Connection) Proxy.newProxyInstance(
            CapsuleParityTest.class.getClassLoader(),
            new Class<?>[] {Connection.class},
            (proxy, method, args) -> {
                if (method.getName().equals("prepareStatement")) {
                    return fakeLiveStatement();
                }
                return switch (method.getName()) {
                    case "close", "commit" -> null;
                    case "isClosed" -> Boolean.FALSE;
                    default -> throw new SQLException("fake: " + method.getName());
                };
            });
    }

    private static PreparedStatement fakeLiveStatement() {
        return (PreparedStatement) Proxy.newProxyInstance(
            CapsuleParityTest.class.getClassLoader(),
            new Class<?>[] {PreparedStatement.class},
            (proxy, method, args) -> {
                if (method.getName().startsWith("set")) return null;
                if (method.getName().equals("executeQuery")) {
                    Map<String, Object> row = new LinkedHashMap<>();
                    row.put("id", 7L);
                    row.put("symbol", "ACME");
                    return ReproitJdbc.recordedResultSet(List.of(row));
                }
                if (method.getName().equals("close")) return null;
                throw new SQLException("fake: " + method.getName());
            });
    }

    @Test
    void jdbcRecordsThePgWireShapeAndReservesTheRows() throws Exception {
        BackendTrace trace = trace();
        Instrument.scope(trace, () -> {
            Connection connection = ReproitJdbc.connect(CapsuleParityTest::fakeLiveConnection);
            PreparedStatement statement = connection.prepareStatement(
                "SELECT id, symbol FROM issuers WHERE symbol = $1");
            statement.setString(1, "ACME");
            ResultSet rows = statement.executeQuery();
            assertTrue(rows.next());
            assertEquals(7L, rows.getLong("id"));
            assertEquals("ACME", rows.getString(2));
            assertFalse(rows.next());
            return null;
        });
        List<Map<String, Object>> exchanges = exchangesOf(trace);
        assertEquals(1, exchanges.size());
        assertEquals("pg", exchanges.get(0).get("protocol"));
        Map<?, ?> request = (Map<?, ?>) exchanges.get(0).get("request");
        assertEquals("SELECT id, symbol FROM issuers WHERE symbol = $1", request.get("text"));
        assertEquals(List.of("ACME"), request.get("values"));
        Map<?, ?> response = (Map<?, ?>) exchanges.get(0).get("response");
        assertEquals("SELECT", response.get("command"));
        assertEquals(1L, response.get("rowCount"));
        assertEquals(List.of(Map.of("id", 7L, "symbol", "ACME")), response.get("rows"));
    }

    @Test
    void jdbcReplayServesTheCaptureWithTheDatabaseDown() throws Exception {
        Instrument.resetSessionForTest(Replay.load(writeDbCapture().toString()));
        // The connect stub: the live source must NEVER be invoked.
        Connection connection = ReproitJdbc.connect(() -> {
            throw new IllegalStateException("live database dialed during hermetic replay");
        });
        PreparedStatement statement = connection.prepareStatement(
            "SELECT id, symbol FROM issuers WHERE symbol = $1");
        statement.setString(1, "ACME");
        ResultSet rows = statement.executeQuery();
        assertTrue(rows.next());
        assertEquals(7L, rows.getLong("id"));
        assertEquals("ACME", rows.getString("symbol"));
        connection.close();
    }

    @Test
    void jdbcReplayDivergesLoudlyAndReRaisesRecordedErrors() throws Exception {
        Instrument.resetSessionForTest(Replay.load(writeDbCapture().toString()));
        Connection connection = ReproitJdbc.connect(() -> {
            throw new IllegalStateException("live database dialed during hermetic replay");
        });
        PrintStream original = System.err;
        System.setErr(new PrintStream(new ByteArrayOutputStream(), true, StandardCharsets.UTF_8));
        try {
            SQLException diverged = assertThrows(SQLException.class, () ->
                connection.prepareStatement("SELECT something else").executeQuery());
            assertTrue(diverged.getMessage().contains("diverged"));
        } finally {
            System.setErr(original);
        }
        SQLException recorded = assertThrows(SQLException.class, () -> {
            PreparedStatement statement =
                connection.prepareStatement("INSERT INTO audit VALUES ($1)");
            statement.setString(1, "x");
            statement.executeUpdate();
        });
        assertEquals("duplicate key", recorded.getMessage());
        assertEquals("23505", recorded.getSQLState());
    }

    // ------------------------------------------------------------------
    // Pinned clock and random.
    // ------------------------------------------------------------------

    @Test
    void theEnvelopePinsClockOffsetAndSeededRandom() throws Exception {
        Replay session = Replay.load(writeDbCapture().toString());
        session.pinEnvelope();
        Instrument.resetSessionForTest(session);
        long observed = 1753747200000L;
        long now = Instrument.clock().millis();
        assertTrue(Math.abs(now - observed) < 60_000,
            "the SDK clock must read the capture moment, got " + now);
        java.util.Random first = Instrument.random();
        java.util.Random second = session.random();
        double left = first.nextDouble();
        assertEquals(left, second.nextDouble(), 0.0, "the seeded stream must be stable");
        assertTrue(left >= 0 && left < 1);
        int bounded = first.nextInt(100);
        assertTrue(bounded >= 0 && bounded < 100);
    }

    // ------------------------------------------------------------------
    // Capture payloads for the replay tests.
    // ------------------------------------------------------------------

    private static Path writeStreamCapture() throws Exception {
        String payload = """
            {"format":"reproit-backend-capture","version":2,"operation":"GET /quote",
             "oracle":"backend-server-error",
             "envelope":{"observedAtMs":1753747200000,"tz":"Europe/Berlin",
                         "replaySeed":"00ff00ff00ff00ff"},
             "events":[
              {"kind":"effect","sequence":1,"exchange":{"protocol":"http",
               "request":{"method":"GET","url":"http://llm.internal/stream"},
               "response":{"status":200,
                 "headers":{"content-type":"text/event-stream"},
                 "body":"data: a\\n\\ndata: b\\n\\ndata: c\\n\\n",
                 "stream":{"chunks":[9,9,9]}}}}
             ]}""";
        Path file = Files.createTempFile("reproit-stream", ".json");
        Files.writeString(file, payload, StandardCharsets.UTF_8);
        file.toFile().deleteOnExit();
        return file;
    }

    private static Path writeDbCapture() throws Exception {
        String payload = """
            {"format":"reproit-backend-capture","version":2,"operation":"GET /quote",
             "oracle":"backend-server-error",
             "envelope":{"observedAtMs":1753747200000,"tz":"Europe/Berlin",
                         "replaySeed":"00ff00ff00ff00ff"},
             "events":[
              {"kind":"effect","sequence":1,"exchange":{"protocol":"pg",
               "request":{"text":"SELECT id, symbol FROM issuers WHERE symbol = $1",
                          "values":["ACME"]},
               "response":{"command":"SELECT","rowCount":1,
                           "rows":[{"id":7,"symbol":"ACME"}]}}},
              {"kind":"effect","sequence":2,"exchange":{"protocol":"pg",
               "request":{"text":"INSERT INTO audit VALUES ($1)","values":["x"]},
               "response":{"error":{"message":"duplicate key","code":"23505"}}}}
             ]}""";
        Path file = Files.createTempFile("reproit-db", ".json");
        Files.writeString(file, payload, StandardCharsets.UTF_8);
        file.toFile().deleteOnExit();
        return file;
    }
}
