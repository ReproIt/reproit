using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using ReproIt.Windows;
using Xunit;

// Both this class and CausalHandlerTest configure the handler through process
// environment variables, which xunit would otherwise let them do concurrently
// from two collections.
[assembly: CollectionBehavior(DisableTestParallelization = true)]

namespace ReproIt.ParityTests
{
    /// <summary>
    /// Executes the shared behavioral vectors for the FROZEN runner wire, which
    /// is deliberately not the capture wire. This SDK is replay only: it never
    /// records a capture batch, so it has no inline body budget, no header table
    /// and no $reproit placeholder. Its whole shared surface with the rest of the
    /// fleet is the secret-key predicate, and eight languages hand implement that
    /// predicate. A divergence about which keys count as secret is silent in both
    /// directions: too narrow and a credential ships inside a capsule, too wide
    /// and a field replay needs is scrubbed into a placeholder that never
    /// matches. ../capture-behavior-v1.json states the predicate once so a defect
    /// is found once instead of eight times.
    ///
    /// One difference from the capture wire is deliberate and is asserted here so
    /// it cannot be closed by accident: idempotency_key IS secret on the capture
    /// wire and is NOT secret here. The runner list is thirteen parts, one
    /// shorter, because changing it would change bytes the fuzz harness compares.
    ///
    /// The predicate is private, so it is driven through the public handler: the
    /// body slot takes every folding case (JSON keys are unconstrained) and the
    /// header slot takes the subset that is a legal HTTP header name.
    /// </summary>
    public class BehaviorVectorsTest
    {
        private static readonly Regex HeaderName =
            new Regex(@"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$");

        private sealed class StubHandler : HttpMessageHandler
        {
            public string Body;
            protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request,
                                                                   CancellationToken token) =>
                Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) {
                    Content = new StringContent(Body, Encoding.UTF8, "application/json")
                });
        }

        [Fact]
        public async Task CausalRedactionFoldingCases()
        {
            // Copied next to the test assembly by the csproj, exactly like the
            // golden signature vectors.
            JsonElement causal =
                JsonDocument.Parse(File.ReadAllText("capture-behavior-v1.json"))
                    .RootElement.GetProperty("causalRedaction");
            string placeholder = causal.GetProperty("placeholder").GetString();
            JsonElement[] cases = causal.GetProperty("foldingCases").EnumerateArray().ToArray();
            Assert.NotEmpty(cases);

            string dir = Path.Combine(Path.GetTempPath(), "reproit-vectors-" + Guid.NewGuid());
            Directory.CreateDirectory(dir);
            string network = Path.Combine(dir, "network.jsonl");
            Environment.SetEnvironmentVariable("REPROIT_NETWORK_FILE", network);
            Environment.SetEnvironmentVariable("REPROIT_DEVICE", "a");
            JsonElement exchange;
            try
            {
                var body = new Dictionary<string, string>();
                foreach (JsonElement item in cases)
                    body[item.GetProperty("field").GetString()] = "raw-value";
                var stub = new StubHandler { Body = JsonSerializer.Serialize(body) };
                using var client = new HttpClient(new ReproItCausalHandler(stub));
                var request = new HttpRequestMessage(HttpMethod.Get, "https://app.test/feed");
                foreach (JsonElement item in cases)
                {
                    string field = item.GetProperty("field").GetString();
                    if (HeaderName.IsMatch(field))
                        request.Headers.TryAddWithoutValidation(field, "raw-value");
                }
                await client.SendAsync(request);
                exchange = JsonDocument.Parse(File.ReadAllText(network).Trim()).RootElement;
            }
            finally
            {
                Environment.SetEnvironmentVariable("REPROIT_NETWORK_FILE", null);
                Environment.SetEnvironmentVariable("REPROIT_DEVICE", null);
                Directory.Delete(dir, true);
            }

            JsonElement headers = exchange.GetProperty("requestHeaders");
            JsonElement redacted = exchange.GetProperty("responseBody");
            foreach (JsonElement item in cases)
            {
                string field = item.GetProperty("field").GetString();
                bool secret = item.GetProperty("secret").GetBoolean();
                string want = secret ? placeholder : "raw-value";
                Assert.Equal(want, redacted.GetProperty(field).GetString());
                if (!HeaderName.IsMatch(field))
                    continue;
                // Header names survive with whatever case the transport used.
                string emitted = headers.EnumerateObject()
                                     .First(x => string.Equals(x.Name, field,
                                                               StringComparison.OrdinalIgnoreCase))
                                     .Value.GetString();
                Assert.Equal(want, emitted);
            }
        }
    }
}
