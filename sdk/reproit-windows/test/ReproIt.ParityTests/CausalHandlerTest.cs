using System;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using ReproIt.Windows;
using Xunit;

namespace ReproIt.ParityTests
{
    public class CausalHandlerTest
    {
        private sealed class LiveHandler : HttpMessageHandler
        {
            public int Calls;
            protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
            {
                Calls++;
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.Created)
                {
                    Content = new StringContent("{\"email\":\"a@b.c\",\"ok\":true}", Encoding.UTF8, "application/json")
                });
            }
        }

        [Fact]
        public async Task CaptureRedactsAndReplayIsFailClosed()
        {
            string dir = Path.Combine(Path.GetTempPath(), "reproit-dotnet-" + Guid.NewGuid());
            Directory.CreateDirectory(dir);
            string network = Path.Combine(dir, "network.jsonl");
            string action = Path.Combine(dir, "action");
            string capabilities = Path.Combine(dir, "capabilities.json");
            File.WriteAllText(action, "1"); File.WriteAllText(capabilities, "{}");
            Environment.SetEnvironmentVariable("REPROIT_NETWORK_FILE", network);
            Environment.SetEnvironmentVariable("REPROIT_ACTION_FILE", action);
            Environment.SetEnvironmentVariable("REPROIT_CAPABILITIES_FILE", capabilities);
            Environment.SetEnvironmentVariable("REPROIT_DEVICE", "a");
            try
            {
                var live = new LiveHandler();
                using (var client = new HttpClient(new ReproItCausalHandler(live)))
                {
                    var request = new HttpRequestMessage(HttpMethod.Post, "https://api.test/send");
                    request.Headers.TryAddWithoutValidation("Authorization", "raw");
                    request.Content = new StringContent("{\"token\":\"raw\",\"kind\":\"message\"}", Encoding.UTF8, "application/json");
                    Assert.Equal(HttpStatusCode.Created, (await client.SendAsync(request)).StatusCode);
                }
                string captured = File.ReadAllText(network);
                using (JsonDocument document = JsonDocument.Parse(captured))
                {
                    Assert.Equal("<reproit:secret>", document.RootElement.GetProperty("requestHeaders").GetProperty("Authorization").GetString());
                    Assert.Equal("<reproit:secret>", document.RootElement.GetProperty("requestBody").GetProperty("token").GetString());
                }
                Assert.DoesNotContain("a@b.c", captured);

                string capsule = Path.Combine(dir, "capsule.json");
                File.WriteAllText(capsule, "{\"exchanges\":[{\"id\":\"a-1-0\",\"actor\":\"a\",\"actionIndex\":1,\"ordinal\":0,\"protocol\":\"https\",\"method\":\"GET\",\"url\":\"https://api.test/config?a=1&b=2\",\"status\":200,\"responseHeaders\":{\"content-type\":\"application/json\"},\"responseBody\":{\"enabled\":true},\"required\":true}]}");
                Environment.SetEnvironmentVariable("REPROIT_CAPSULE", capsule);
                var forbidden = new LiveHandler();
                using var replay = new HttpClient(new ReproItCausalHandler(forbidden));
                Assert.Contains("enabled", await replay.GetStringAsync("https://api.test/config?b=2&a=1"));
                await Assert.ThrowsAsync<HttpRequestException>(() => replay.GetAsync("https://api.test/miss"));
                Assert.Equal(0, forbidden.Calls);
            }
            finally
            {
                foreach (string name in new[] { "REPROIT_NETWORK_FILE", "REPROIT_ACTION_FILE", "REPROIT_CAPABILITIES_FILE", "REPROIT_DEVICE", "REPROIT_CAPSULE" })
                    Environment.SetEnvironmentVariable(name, null);
                Directory.Delete(dir, true);
            }
        }
    }
}
