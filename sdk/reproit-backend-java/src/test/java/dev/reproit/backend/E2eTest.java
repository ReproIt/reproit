// Functional end-to-end test: a real Jetty servlet container running
// ReproitFilter with a planted 500, real HTTP requests, and a local stub
// ingest server (com.sun.net.httpserver). Asserts the finding batch arrives
// correctly tagged with the reproitCapture sequence, and that a scan-time
// request round-trips the x-reproit-events header.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

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
import java.util.ArrayList;
import java.util.Base64;
import java.util.EnumSet;
import java.util.List;
import java.util.Map;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.servlet.FilterHolder;
import org.eclipse.jetty.servlet.ServletContextHandler;
import org.eclipse.jetty.servlet.ServletHolder;
import org.junit.jupiter.api.Test;

class E2eTest {
    record Received(String authorization, Map<String, Object> batch) {}

    static final class OkServlet extends HttpServlet {
        @Override
        protected void doGet(HttpServletRequest request, HttpServletResponse response)
                throws IOException {
            response.setContentType("application/json");
            response.getWriter().write("{\"ok\":true}");
        }
    }

    static final class BoomServlet extends HttpServlet {
        @Override
        protected void doPost(HttpServletRequest request, HttpServletResponse response)
                throws IOException {
            BackendTrace trace =
                (BackendTrace) request.getAttribute(ReproitFilter.REQUEST_ATTRIBUTE);
            assertNotNull(trace);
            trace.effect("write", new BackendTrace.Effect().resource("orders").key("1"));
            response.setStatus(500);
            response.setContentType("application/json");
            response.getWriter().write("{\"error\":\"boom\"}");
        }
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> at(Object value, String... path) {
        Map<String, Object> current = (Map<String, Object>) value;
        for (String key : path) {
            current = (Map<String, Object>) current.get(key);
        }
        return current;
    }

    @Test
    void plantedFiveHundredShipsATaggedFindingBatchToTheStubIngest() throws Exception {
        List<Received> received = new ArrayList<>();
        HttpServer ingest = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        ingest.createContext("/v1/events", exchange -> {
            String body = new String(
                exchange.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
            synchronized (received) {
                received.add(new Received(
                    exchange.getRequestHeaders().getFirst("Authorization"),
                    (Map<String, Object>) Json.parse(body)));
            }
            byte[] reply = "{\"accepted\":true}".getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, reply.length);
            exchange.getResponseBody().write(reply);
            exchange.close();
        });
        ingest.start();
        String ingestUrl =
            "http://127.0.0.1:" + ingest.getAddress().getPort() + "/v1/events";

        Capture capture = Capture.create(new Capture.Config()
            .endpoint(ingestUrl)
            .apiKey("sk_live_test")
            .appId("app-e2e")
            .build("9.9.9")
            .flushIntervalMs(100));
        assertNotNull(capture);

        Server server = new Server();
        ServerConnector connector = new ServerConnector(server);
        connector.setHost("127.0.0.1");
        connector.setPort(0);
        server.addConnector(connector);
        ServletContextHandler handler = new ServletContextHandler();
        handler.setContextPath("/");
        handler.addFilter(new FilterHolder(new ReproitFilter(capture)), "/*",
            EnumSet.of(DispatcherType.REQUEST));
        handler.addServlet(new ServletHolder(new OkServlet()), "/ok");
        handler.addServlet(new ServletHolder(new BoomServlet()), "/boom");
        server.setHandler(handler);
        server.start();
        String base = "http://127.0.0.1:" + connector.getLocalPort();
        HttpClient client = HttpClient.newHttpClient();

        try {
            HttpResponse<String> boom = client.send(
                HttpRequest.newBuilder(URI.create(base + "/boom"))
                    .header("Content-Type", "application/json")
                    .POST(HttpRequest.BodyPublishers.ofString(
                        "{\"item\":\"widget\",\"apiKey\":\"sk_live_leak\"}"))
                    .build(),
                HttpResponse.BodyHandlers.ofString());
            assertEquals(500, boom.statusCode());
            assertEquals(true, capture.flush(5000));

            assertEquals(1, received.size());
            assertEquals("Bearer sk_live_test", received.get(0).authorization());
            Map<String, Object> batch = received.get(0).batch();
            EventBatchV1.validateEventBatch(batch);
            assertEquals("app-e2e", batch.get("appId"));
            assertEquals("9.9.9", at(batch, "deployment").get("version"));
            List<Map<String, Object>> findings = new ArrayList<>();
            for (Object frame : (List<?>) batch.get("frames")) {
                Map<String, Object> event = at(frame, "event");
                if ("finding".equals(event.get("kind"))) findings.add(event);
            }
            assertEquals(1, findings.size());
            Map<String, Object> finding = findings.get(0);
            assertEquals(Capture.SERVER_ERROR_ORACLE, at(finding, "identity").get("oracle"));
            assertEquals("reproit-backend-java", at(finding, "context").get("capture"));
            Map<String, Object> replay = at(finding, "context", "reproitCapture");
            assertEquals(Capture.CAPTURE_FORMAT, replay.get("format"));
            assertEquals(Capture.SERVER_ERROR_ORACLE, replay.get("oracle"));
            List<?> events = (List<?>) replay.get("events");
            List<Object> kinds = new ArrayList<>();
            for (Object event : events) kinds.add(((Map<?, ?>) event).get("kind"));
            assertEquals(List.of("start", "effect", "return"), kinds);
            assertEquals("orders", ((Map<?, ?>) events.get(1)).get("resource"));
            assertEquals(500L, ((Map<?, ?>) events.get(2)).get("status"));
            assertEquals(false, ((Map<?, ?>) events.get(2)).get("success"));
            // The secret-shaped input field was structurally redacted before upload.
            Map<String, Object> start = (Map<String, Object>) events.get(0);
            assertEquals(true, at(start, "input", "body", "apiKey", "$reproit").get("redacted"));
            assertEquals("widget", at(start, "input", "body").get("item"));

            // Scan-time request: header round-trip, no capture of the healthy call.
            HttpResponse<String> ok = client.send(
                HttpRequest.newBuilder(URI.create(base + "/ok"))
                    .header("x-reproit-trace", "trace-e2e")
                    .header("x-reproit-actor", "alice")
                    .GET()
                    .build(),
                HttpResponse.BodyHandlers.ofString());
            assertEquals(200, ok.statusCode());
            String header = ok.headers().firstValue("x-reproit-events").orElse(null);
            assertNotNull(header, "expected an x-reproit-events response header");
            List<?> traced = (List<?>) Json.parse(new String(
                Base64.getUrlDecoder().decode(header), StandardCharsets.UTF_8));
            assertEquals("trace-e2e", ((Map<?, ?>) traced.get(0)).get("traceId"));
            assertEquals("alice", ((Map<?, ?>) traced.get(0)).get("actor"));
            Map<?, ?> last = (Map<?, ?>) traced.get(traced.size() - 1);
            assertEquals("return", last.get("kind"));
            assertEquals(200L, last.get("status"));
            assertEquals(1, capture.stats().capturedOperations());
        } finally {
            server.stop();
            ingest.stop(0);
        }
    }
}
