// Money-test fixture for Java capsule parity: a com.sun.net.httpserver app
// with the reproit SDK whose /quote operation 500s because an upstream
// pricing service returns {"prices": null} and the handler indexes into it.
// The upstream call goes through the delegating ReproitHttpClient and the
// database call through a ReproitJdbc-wrapped fake driver connection (the
// same fake-driver idiom the Python fixture uses: a driver that MUST never
// be reached during hermetic replay). The connection is opened at BOOT, so
// replay proves the connect stub lets the app start with the database down.
//
// MODE=capture boots the upstream plus the app, fires the failing request,
// and writes a version 2 reproit-backend-capture (exchanges plus envelope)
// to CAPTURE_OUT. Default (server) mode boots ONLY the app on $PORT; with
// REPROIT_REPLAY set the SDK serves the recorded exchanges in process, so
// neither the upstream nor the database exists. FIXED=1 applies the fix.
//
// Compile: javac -d classes -sourcepath sdk/reproit-backend-java/src/main/java \
//   examples/java-backend-fixture/Main.java

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import dev.reproit.backend.BackendTrace;
import dev.reproit.backend.Capture;
import dev.reproit.backend.Instrument;
import dev.reproit.backend.Json;
import dev.reproit.backend.ReproitHttpClient;
import dev.reproit.backend.ReproitJdbc;
import dev.reproit.backend.TraceContext;
import java.io.IOException;
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
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class Main {
    private static final int UPSTREAM_PORT = 19991;
    private static final int CAPTURE_PORT = 19990;
    private static final boolean CAPTURING = "capture".equals(System.getenv("MODE"));
    private static final boolean FIXED = "1".equals(System.getenv("FIXED"));

    private Main() {}

    /**
     * A JDBC-driver-shaped fake that MUST never be reached for real: in
     * capture mode a canned result stands in for a live database; in replay
     * mode ReproitJdbc.connect returns the in-process stub instead.
     */
    private static Connection fakeDriverConnection() {
        if (!CAPTURING) {
            throw new IllegalStateException("live database dialed during hermetic replay");
        }
        return (Connection) Proxy.newProxyInstance(
            Main.class.getClassLoader(),
            new Class<?>[] {Connection.class},
            (proxy, method, args) -> {
                if (method.getName().equals("prepareStatement")) return fakeDriverStatement();
                return switch (method.getName()) {
                    case "close", "commit" -> null;
                    case "isClosed" -> Boolean.FALSE;
                    default -> throw new SQLException("fake driver: " + method.getName());
                };
            });
    }

    private static PreparedStatement fakeDriverStatement() {
        return (PreparedStatement) Proxy.newProxyInstance(
            Main.class.getClassLoader(),
            new Class<?>[] {PreparedStatement.class},
            (proxy, method, args) -> {
                if (!CAPTURING) {
                    throw new SQLException("live database reached during hermetic replay");
                }
                if (method.getName().startsWith("set")) return null;
                if (method.getName().equals("executeQuery")) {
                    Map<String, Object> row = new LinkedHashMap<>();
                    row.put("id", 7L);
                    row.put("symbol", "ACME");
                    return ReproitJdbc.recordedResultSet(List.of(row));
                }
                if (method.getName().equals("close")) return null;
                throw new SQLException("fake driver: " + method.getName());
            });
    }

    private static void respond(HttpExchange exchange, int status, String body)
            throws IOException {
        byte[] raw = body.getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().set("Content-Type", "application/json");
        exchange.sendResponseHeaders(status, raw.length);
        exchange.getResponseBody().write(raw);
        exchange.close();
    }

    /** The /quote handler body: one db read, one upstream call, the defect. */
    private static int quote(Connection database, HttpClient client, StringBuilder body)
            throws Exception {
        PreparedStatement statement = database.prepareStatement(
            "SELECT id, symbol FROM issuers WHERE symbol = $1");
        statement.setString(1, "ACME");
        ResultSet issuer = statement.executeQuery();
        if (!issuer.next()) {
            body.append("{\"error\":\"unknown symbol\"}");
            return 404;
        }
        String url = "http://127.0.0.1:" + UPSTREAM_PORT + "/prices?tier=gold";
        HttpResponse<String> upstream = client.send(
            HttpRequest.newBuilder(URI.create(url)).GET().build(),
            HttpResponse.BodyHandlers.ofString());
        Object prices = ((Map<?, ?>) Json.parse(upstream.body())).get("prices");
        if (FIXED && !(prices instanceof List)) {
            body.append("{\"first\":null,\"note\":\"no prices available\"}");
            return 200;
        }
        Object first = ((List<?>) prices).get(0);
        body.append("{\"first\":").append(first).append("}");
        return 200;
    }

    public static void main(String[] args) throws Exception {
        Instrument.init();
        HttpServer upstream = null;
        if (CAPTURING) {
            upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", UPSTREAM_PORT), 0);
            upstream.createContext("/prices", exchange ->
                respond(exchange, 200, "{\"prices\":null}"));
            upstream.start();
        }

        // Boot-time connect: with REPROIT_REPLAY set this is the in-process
        // stub, which is exactly how the app starts with the database down.
        Connection database = ReproitJdbc.connect(Main::fakeDriverConnection);
        HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());

        List<BackendTrace> recorded = new java.util.ArrayList<>();
        int port = CAPTURING
            ? CAPTURE_PORT
            : Integer.parseInt(System.getenv().getOrDefault("PORT", "" + CAPTURE_PORT));
        HttpServer app = HttpServer.create(new InetSocketAddress("127.0.0.1", port), 0);
        app.createContext("/quote", exchange -> {
            TraceContext context = new TraceContext(
                "cap-money-java-fixture-1", null, 0, "java-money-fixture", null, true);
            BackendTrace trace = BackendTrace.begin(
                context, "GET /quote", new BackendTrace.Options()
                    .input(Map.of("query", Map.of("symbol", "ACME"))));
            StringBuilder body = new StringBuilder();
            int status;
            try {
                status = Instrument.scope(trace, () -> quote(database, client, body));
            } catch (Exception failure) {
                body.setLength(0);
                body.append("{\"error\":\"internal\"}");
                status = 500;
            }
            trace.finish(null, status, status < 500, true);
            recorded.add(trace);
            respond(exchange, status, body.toString());
        });
        app.start();

        if (!CAPTURING) {
            Thread.currentThread().join();
            return;
        }
        String url = "http://127.0.0.1:" + CAPTURE_PORT + "/quote?symbol=ACME";
        HttpResponse<String> failing = HttpClient.newHttpClient().send(
            HttpRequest.newBuilder(URI.create(url)).GET().build(),
            HttpResponse.BodyHandlers.ofString());
        System.out.println("capture fixture status " + failing.statusCode());
        writeCapture(recorded.get(recorded.size() - 1));
        app.stop(0);
        upstream.stop(0);
        System.exit(0);
    }

    /** The replayable version-2 capture payload, exchanges plus envelope. */
    private static void writeCapture(BackendTrace trace) throws IOException {
        Map<String, Object> first = trace.events().get(0);
        Long observedAt = first.get("at") instanceof Number at ? at.longValue() : null;
        Map<String, Object> payload = new LinkedHashMap<>();
        payload.put("format", Capture.CAPTURE_FORMAT);
        payload.put("version", 2);
        payload.put("operation", first.get("operation"));
        payload.put("oracle", Capture.SERVER_ERROR_ORACLE);
        payload.put("envelope", Capture.determinismEnvelope(observedAt));
        payload.put("events", trace.events());
        Files.writeString(
            Path.of(System.getenv("CAPTURE_OUT")),
            Json.canonicalJson(payload),
            StandardCharsets.UTF_8);
    }
}
