package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.DispatcherType;
import jakarta.servlet.http.HttpServlet;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
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
import org.junit.jupiter.api.Test;

/** Real Jetty middleware and bounded per-dependency capture regression gate. */
class AdapterBenchmarkTest {
    static final int DEPENDENCIES = 64;

    static final class JsonServlet extends HttpServlet {
        @Override
        protected void doGet(HttpServletRequest request, HttpServletResponse response)
                throws java.io.IOException {
            response.setContentType("application/json");
            response.getWriter().write("{\"account\":{\"id\":42,\"ok\":true}}");
        }
    }

    private static int configured(String name, int fallback) {
        String value = System.getenv(name);
        return value == null ? fallback : Math.max(1, Integer.parseInt(value));
    }

    private static double median(List<Double> values) {
        List<Double> sorted = new ArrayList<>(values);
        sorted.sort(Double::compare);
        return sorted.get(sorted.size() / 2);
    }

    private static double httpCost(boolean mounted, boolean traced, int runs) throws Exception {
        Server server = new Server();
        ServerConnector connector = new ServerConnector(server);
        connector.setHost("127.0.0.1");
        connector.setPort(0);
        server.addConnector(connector);
        ServletContextHandler context = new ServletContextHandler();
        context.setContextPath("/");
        if (mounted) {
            context.addFilter(new FilterHolder(new ReproitFilter()), "/*",
                EnumSet.of(DispatcherType.REQUEST));
        }
        context.addServlet(new ServletHolder(new JsonServlet()), "/account");
        server.setHandler(context);
        server.start();
        HttpClient client = HttpClient.newBuilder().version(HttpClient.Version.HTTP_1_1).build();
        URI uri = URI.create("http://127.0.0.1:" + connector.getLocalPort() + "/account?id=42");
        try {
            for (int index = 0; index < Math.min(500, runs / 4); index++) send(client, uri, traced);
            long started = System.nanoTime();
            for (int index = 0; index < runs; index++) send(client, uri, traced);
            return (System.nanoTime() - started) / 1000.0 / runs;
        } finally {
            server.stop();
        }
    }

    private static void send(HttpClient client, URI uri, boolean traced) throws Exception {
        HttpRequest.Builder request = HttpRequest.newBuilder(uri).GET();
        if (traced) request.header("x-reproit-trace", "bench-trace");
        HttpResponse<Void> response = client.send(
            request.build(), HttpResponse.BodyHandlers.discarding());
        if (response.statusCode() != 200) throw new AssertionError(response.statusCode());
    }

    private static double dependencyCost(boolean captured, int runs) {
        TraceContext context = new TraceContext("dependency-benchmark", null, 1, null, null);
        Map<String, Object> exchange = Map.of(
            "request", Map.of("method", "GET", "url", "http://pricing.test/quote?tier=gold"),
            "response", Map.of("status", 200, "body", Map.of("price", 42)));
        long started = System.nanoTime();
        for (int run = 0; run < runs; run++) {
            BackendTrace trace = BackendTrace.begin(
                context, "dependencyBenchmark", new BackendTrace.Options());
            if (captured) {
                for (int index = 0; index < DEPENDENCIES; index++) {
                    trace.effect("call", new BackendTrace.Effect()
                        .resource("pricing").key(Integer.toString(index)).exchange(exchange));
                }
            }
        }
        return (System.nanoTime() - started) / 1000.0 / (runs * DEPENDENCIES);
    }

    @Test
    void realMiddlewareAndDependencyCaptureStayWithinCeilings() throws Exception {
        int runs = configured("REPROIT_ADAPTER_BENCH_RUNS", 1000);
        int rounds = configured("REPROIT_ADAPTER_BENCH_ROUNDS", 5);
        Map<String, List<Double>> http = new LinkedHashMap<>();
        Map<String, List<Double>> dependency = new LinkedHashMap<>();
        for (String key : List.of("baseline", "inactive", "active", "control")) {
            http.put(key, new ArrayList<>());
        }
        for (String key : List.of("baseline", "captured", "control")) {
            dependency.put(key, new ArrayList<>());
        }
        for (int round = 0; round < rounds; round++) {
            http.get("baseline").add(httpCost(false, false, runs));
            http.get("inactive").add(httpCost(true, false, runs));
            http.get("active").add(httpCost(true, true, runs));
            http.get("control").add(httpCost(false, false, runs));
            dependency.get("baseline").add(dependencyCost(false, runs));
            dependency.get("captured").add(dependencyCost(true, runs));
            dependency.get("control").add(dependencyCost(false, runs));
        }
        double baseline = median(http.get("baseline"));
        double noise = Math.abs(median(http.get("control")) - baseline);
        double inactive = median(http.get("inactive")) - baseline;
        double active = median(http.get("active")) - baseline;
        double depBaseline = median(dependency.get("baseline"));
        double depNoise = Math.abs(median(dependency.get("control")) - depBaseline);
        double depCost = median(dependency.get("captured")) - depBaseline;
        assertTrue(noise < 500, "HTTP noise " + noise + "us");
        assertTrue(inactive < 500, "inactive cost " + inactive + "us");
        assertTrue(active < 1500, "active cost " + active + "us");
        assertTrue(depNoise < 10, "dependency noise " + depNoise + "us");
        assertTrue(depCost < 50, "dependency cost " + depCost + "us");
        System.out.printf(
            "{\"language\":\"java\",\"runs\":%d,\"rounds\":%d,"
                + "\"noiseFloorMicros\":%.2f,\"baselineMicros\":%.2f,"
                + "\"inactiveCostMicros\":%.2f,\"activeCostMicros\":%.2f,"
                + "\"dependencyNoiseFloorMicros\":%.2f,"
                + "\"dependencyCaptureCostMicros\":%.2f,\"dependencyCeilingMicros\":50}%n",
            runs, rounds, noise, baseline, inactive, active, depNoise, depCost);
    }
}
