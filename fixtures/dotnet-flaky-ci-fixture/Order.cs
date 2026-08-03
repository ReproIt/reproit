// Module under test for the .NET flaky-CI fixture: computes an order total from the tax
// rate the config service answers.
//
// The planted bug: the config service's LEGACY format returns the rate as a STRING
// ("0.25"). The buggy path only accepts a numeric rate and silently applies 0 for anything
// else, so a 100 subtotal totals 100 instead of 125. FIXED=1 applies the fix: coerce the
// rate to a number before arithmetic.

using System.Globalization;
using ReproitBackend;

namespace FlakyCiFixture;

public static class Order
{
    public static async Task<double> TotalAsync(
        HttpClient client, string configUrl, double subtotal)
    {
        var response = await client.GetAsync(configUrl + "/tax-rate");
        var body = Json.Parse(await response.Content.ReadAsStringAsync())
            as Dictionary<string, object?> ?? new Dictionary<string, object?>();
        var rate = body.GetValueOrDefault("rate");
        var applied = Environment.GetEnvironmentVariable("FIXED") == "1"
            ? Convert.ToDouble(rate, CultureInfo.InvariantCulture)
            : rate as double? ?? 0.0;
        return subtotal * (1 + applied);
    }
}
