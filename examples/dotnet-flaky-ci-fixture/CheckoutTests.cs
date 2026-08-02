// Planted order-dependent test failure that fires only under CI-like conditions, for the
// flaky-CI wedge (Track 3), .NET twin of examples/flaky-ci-fixture.
//
// The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and leaks state
// into the shared config service: it switches the service to its legacy response format,
// which returns the tax rate as a string. The second test then computes a wrong total and
// fails. A plain local run never takes the legacy branch, so the suite passes and the
// failure looks unreproducible ("flaky"). The capsule spooled by the CI run carries the
// recorded legacy response, so `reproit check <capsule> --exec "dotnet test --filter
// FullyQualifiedName=... --logger \"console;verbosity=detailed\" 1>&2"` re-executes the
// exact failing run anywhere. The logger + redirect are load-bearing: the VSTest host
// swallows raw test console output, and detailed console logging re-prints the SDK's
// stderr markers verbatim on stdout for the redirect to hand to `reproit check`.

using System.Net;
using System.Text;
using ReproitBackend;
using Xunit;
using Xunit.Abstractions;
using Xunit.Sdk;

namespace FlakyCiFixture;

// The shared config service both tests talk to. Stateful on purpose: the legacy-format
// test leaks its toggle into it. Never started under replay, where the SDK serves the
// recorded exchanges in process and any real socket attempt would surface as a divergence,
// not a connection.
public sealed class ConfigService : IDisposable
{
    public const string Url = "http://127.0.0.1:19989";

    private readonly HttpListener? _listener;
    private volatile bool _legacy;

    public ConfigService()
    {
        if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("REPROIT_REPLAY")))
        {
            return;
        }
        _listener = new HttpListener();
        _listener.Prefixes.Add(Url + "/");
        _listener.Start();
        _ = Task.Run(ServeAsync);
    }

    private async Task ServeAsync()
    {
        while (_listener!.IsListening)
        {
            HttpListenerContext context;
            try
            {
                context = await _listener.GetContextAsync();
            }
            catch (Exception)
            {
                return;
            }
            if (context.Request.HttpMethod == "POST" &&
                context.Request.Url!.AbsolutePath == "/format/legacy")
            {
                _legacy = true;
                context.Response.StatusCode = 204;
                context.Response.Close();
                continue;
            }
            var body = Encoding.UTF8.GetBytes(
                _legacy ? "{\"rate\":\"0.25\"}" : "{\"rate\":0.25}");
            context.Response.StatusCode = 200;
            context.Response.ContentType = "application/json";
            await context.Response.OutputStream.WriteAsync(body);
            context.Response.Close();
        }
    }

    public void Dispose()
    {
        try
        {
            _listener?.Stop();
        }
        catch (Exception)
        {
            // Shutdown noise only.
        }
    }
}

// The state leak needs the legacy toggle to run FIRST; xUnit v2's default case order is not
// a contract, so the order is pinned by name.
public sealed class AlphabeticalOrderer : ITestCaseOrderer
{
    public IEnumerable<TTestCase> OrderTestCases<TTestCase>(IEnumerable<TTestCase> testCases)
        where TTestCase : ITestCase =>
        testCases.OrderBy(testCase => testCase.TestMethod.Method.Name, StringComparer.Ordinal);
}

[TestCaseOrderer("FlakyCiFixture.AlphabeticalOrderer", "FlakyCiFixture")]
public class CheckoutTests : IClassFixture<ConfigService>
{
    // Every outbound call goes through the SDK boundary: recorded in capture mode, served
    // from the capsule in replay mode.
    private static readonly HttpClient Client = new(Instrument.Handler());

    [Fact]
    public Task T1_LegacyConfigFormatToggles() =>
        Ci.TestAsync("checkout", "legacy config format toggles", async () =>
        {
            // CI-only: this is the state leak that makes the next test order dependent.
            // A local run never takes this branch.
            if (Environment.GetEnvironmentVariable("CI_LEGACY_MATRIX") != "1") return;
            var response = await Client.PostAsync(ConfigService.Url + "/format/legacy", null);
            Assert.Equal(204, (int)response.StatusCode);
        });

    [Fact]
    public Task T2_OrderTotalAppliesTheConfiguredTaxRate() =>
        Ci.TestAsync("checkout", "order total applies the configured tax rate", async () =>
            Assert.Equal(125d, await Order.TotalAsync(Client, ConfigService.Url, 100)));
}
