// Planted order-dependent test failure that fires only under CI-like
// conditions, for the flaky-CI wedge (Track 3), Java port of
// examples/flaky-ci-fixture.
//
// The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
// leaks state into the shared config service: it switches the service to its
// legacy response format, which returns the tax rate as a string. The second
// test then computes a wrong total and fails. A plain local run never takes
// the legacy branch, so the suite passes and the failure looks
// unreproducible ("flaky"). The capsule spooled by the CI run carries the
// recorded legacy response, so `reproit check <capsule> --exec "java -cp
// classes CheckoutTest"` re-executes the exact failing run anywhere.
//
// Uses the SDK's dependency-free Ci micro-runner (the JUnit-shaped surface
// is ReproitCi) so replay compiles with plain javac and needs no jars and no
// network. Compile:
//   javac -d classes -sourcepath sdk/reproit-backend-java/src/main/java \
//     examples/java-flaky-ci-fixture/CheckoutTest.java

import com.sun.net.httpserver.HttpServer;
import dev.reproit.backend.Ci;
import dev.reproit.backend.Instrument;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.nio.charset.StandardCharsets;

public final class CheckoutTest {
    private static final int PORT = 19992;
    private static final String CONFIG_URL = "http://127.0.0.1:" + PORT;

    // The shared config service both tests talk to. Stateful on purpose: the
    // legacy-format test leaks its toggle into it. Never started under
    // replay, where the SDK serves the recorded exchanges in process and any
    // real socket attempt would surface as a divergence, not a connection.
    private static boolean legacy = false;

    private CheckoutTest() {}

    private static HttpServer configService() throws Exception {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", PORT), 0);
        server.createContext("/", exchange -> {
            if ("POST".equals(exchange.getRequestMethod())
                    && "/format/legacy".equals(exchange.getRequestURI().getPath())) {
                legacy = true;
                exchange.sendResponseHeaders(204, -1);
                exchange.close();
                return;
            }
            byte[] body = (legacy ? "{\"rate\":\"0.25\"}" : "{\"rate\":0.25}")
                .getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            exchange.getResponseBody().write(body);
            exchange.close();
        });
        server.start();
        return server;
    }

    public static void main(String[] args) throws Exception {
        Instrument.init();
        HttpServer server =
            System.getenv("REPROIT_REPLAY") == null ? configService() : null;
        HttpClient client = HttpClient.newHttpClient();
        Ci.Suite suite = Ci.suite("checkout");

        suite.test("legacy config format toggles", () -> {
            // CI-only: this is the state leak that makes the next test order
            // dependent. A local run never takes this branch.
            if (!"1".equals(System.getenv("CI_LEGACY_MATRIX"))) return;
            Instrument.Http.ExchangeResponse response = Instrument.Http.send(
                client,
                HttpRequest.newBuilder(URI.create(CONFIG_URL + "/format/legacy"))
                    .POST(HttpRequest.BodyPublishers.noBody()).build());
            if (response.status() != 204) {
                throw new AssertionError("legacy toggle answered " + response.status());
            }
        });

        suite.test("order total applies the configured tax rate", () -> {
            long total = Order.orderTotal(100, CONFIG_URL, client);
            if (total != 125) {
                throw new AssertionError("order total expected 125, got " + total);
            }
        });

        if (server != null) server.stop(0);
        System.exit(suite.exitCode());
    }
}
