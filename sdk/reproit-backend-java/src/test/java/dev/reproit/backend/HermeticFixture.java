// Money-test fixture: a real Jetty container running ReproitFilter whose
// /quote operation 500s because an upstream pricing service returns
// {"prices": null} and the handler indexes into it.
//
// MODE=capture: boots the upstream and the app, fires the failing request,
// and writes a version-2 reproit-backend-capture (exchanges + envelope) to
// CAPTURE_OUT. Default (server) mode boots ONLY the app on $PORT; with
// REPROIT_REPLAY set the SDK serves the recorded exchanges, so no upstream
// and no database exist. FIXED=1 applies the fix.
package dev.reproit.backend;

import com.sun.net.httpserver.HttpServer;
import jakarta.servlet.DispatcherType;
import jakarta.servlet.http.HttpServlet;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.EnumSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.servlet.FilterHolder;
import org.eclipse.jetty.servlet.ServletContextHandler;
import org.eclipse.jetty.servlet.ServletHolder;

public final class HermeticFixture {
    private static final int UPSTREAM_PORT = 19981;
    private static final String UPSTREAM =
        "http://127.0.0.1:" + UPSTREAM_PORT + "/prices?tier=gold";
    private static final boolean FIXED = "1".equals(System.getenv("FIXED"));

    private HermeticFixture() {}

    /** The /quote handler: one db read, one upstream call, then the defect. */
    public static final class QuoteServlet extends HttpServlet {
        @Override
        protected void doGet(HttpServletRequest request, HttpServletResponse response)
                throws IOException {
            response.setContentType("application/json");
            try {
                Instrument.Db.run(
                    "SELECT id, symbol FROM issuers WHERE symbol = $1",
                    List.of(String.valueOf(request.getParameter("symbol"))),
                    () -> {
                        if (!"capture".equals(System.getenv("MODE"))) {
                            throw new IllegalStateException(
                                "live database reached during hermetic replay");
                        }
                        return new Instrument.Db.Outcome(
                            "SELECT", 1, List.of(Map.of("id", 7L, "symbol", "ACME")));
                    });
                Instrument.Http.ExchangeResponse upstream = Instrument.Http.send(
                    HttpClient.newHttpClient(),
                    HttpRequest.newBuilder(URI.create(UPSTREAM)).GET().build());
                Object prices = ((Map<?, ?>) upstream.json()).get("prices");
                if (FIXED && !(prices instanceof List)) {
                    response.getWriter().write("{\"first\":null,\"note\":\"no prices\"}");
                    return;
                }
                Object first = ((List<?>) prices).get(0);
                response.getWriter().write("{\"first\":" + first + "}");
            } catch (RuntimeException | InterruptedException | IOException failure) {
                response.setStatus(500);
                response.getWriter().write("{\"error\":\"internal\"}");
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Instrument.init();
        boolean capturing = "capture".equals(System.getenv("MODE"));
        HttpServer upstream = null;
        if (capturing) {
            upstream = HttpServer.create(new InetSocketAddress("127.0.0.1", UPSTREAM_PORT), 0);
            upstream.createContext("/prices", exchange -> {
                byte[] reply = "{\"prices\":null}".getBytes(StandardCharsets.UTF_8);
                exchange.getResponseHeaders().set("Content-Type", "application/json");
                exchange.sendResponseHeaders(200, reply.length);
                exchange.getResponseBody().write(reply);
                exchange.close();
            });
            upstream.start();
        }

        List<BackendTrace> recorded = new ArrayList<>();
        Server server = new Server();
        ServerConnector connector = new ServerConnector(server);
        connector.setHost("127.0.0.1");
        connector.setPort(capturing ? 19980 : Integer.parseInt(
            System.getenv().getOrDefault("PORT", "19980")));
        server.addConnector(connector);
        ServletContextHandler handler = new ServletContextHandler();
        handler.setContextPath("/");
        // The filter runs without a Capture: the fixture writes the capture
        // file itself so the money test needs no cloud endpoint.
        handler.addFilter(
            new FilterHolder(new ReproitFilter(new FileSink(recorded))), "/*",
            EnumSet.of(DispatcherType.REQUEST));
        handler.addServlet(new ServletHolder(new QuoteServlet()), "/quote");
        server.setHandler(handler);
        server.start();

        if (!capturing) {
            server.join();
            return;
        }
        String base = "http://127.0.0.1:" + connector.getLocalPort();
        HttpResponse<String> failing = HttpClient.newHttpClient().send(
            HttpRequest.newBuilder(URI.create(base + "/quote?symbol=ACME")).GET().build(),
            HttpResponse.BodyHandlers.ofString());
        System.out.println("capture fixture status " + failing.statusCode());
        writeCapture(recorded.get(recorded.size() - 1));
        server.stop();
        upstream.stop(0);
        System.exit(0);
    }

    /**
     * Supplies the capture-mode context and keeps the finished trace in
     * memory, so the money test needs no cloud endpoint.
     */
    private static final class FileSink implements TraceSink {
        private final List<BackendTrace> traces;

        FileSink(List<BackendTrace> traces) {
            this.traces = traces;
        }

        @Override
        public TraceContext context() {
            return new TraceContext("cap-java-1", null, 0, "money-fixture", null, true);
        }

        @Override
        public void record(BackendTrace trace) {
            traces.add(trace);
        }
    }

    static void writeCapture(BackendTrace trace) throws IOException {
        Map<String, Object> envelope = Capture.determinismEnvelope(null);
        Map<String, Object> payload = new LinkedHashMap<>();
        payload.put("format", Capture.CAPTURE_FORMAT);
        payload.put("version", 2);
        payload.put("operation", trace.events().get(0).get("operation"));
        payload.put("oracle", Capture.SERVER_ERROR_ORACLE);
        payload.put("envelope", envelope);
        payload.put("events", trace.events());
        Files.writeString(
            Path.of(System.getenv("CAPTURE_OUT")),
            Json.canonicalJson(payload),
            StandardCharsets.UTF_8);
    }
}
