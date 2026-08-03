// Module under test for the Java flaky-CI fixture: computes an order total
// from the tax rate the config service answers.
//
// The planted bug: the config service's LEGACY format returns the rate as a
// STRING ("0.25"). The untyped legacy branch silently treats a non-numeric
// rate as zero, so a 100 subtotal totals 100 instead of 125. FIXED=1 applies
// the fix: parse the string rate before arithmetic.

import dev.reproit.backend.Instrument;
import dev.reproit.backend.Json;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.util.Map;

public final class Order {
    private Order() {}

    public static long orderTotal(long subtotal, String configUrl, HttpClient client)
            throws Exception {
        Instrument.Http.ExchangeResponse response = Instrument.Http.send(
            client, HttpRequest.newBuilder(URI.create(configUrl + "/tax-rate")).GET().build());
        Object rate = ((Map<?, ?>) response.json()).get("rate");
        double applied;
        if (rate instanceof Number number) {
            applied = number.doubleValue();
        } else if ("1".equals(System.getenv("FIXED"))) {
            applied = Double.parseDouble(String.valueOf(rate));
        } else {
            // The planted bug: a legacy string rate falls through as zero.
            applied = 0.0;
        }
        return Math.round(subtotal * (1 + applied));
    }
}
