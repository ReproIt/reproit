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

namespace ReproIt.Windows
{
    /// <summary>
    /// Causal HTTP adapter for WPF/WinUI applications. Construct it only through
    /// <see cref="ReproItClient.CreateHttpClient"/>. Outside a ReproIt run it is
    /// a transparent handler; during capture it writes redacted exchanges, and
    /// during replay every unmatched request fails without reaching the network.
    /// </summary>
    public sealed class ReproItCausalHandler : DelegatingHandler
    {
        private static readonly Regex Secret = new Regex(
            "password|passwd|secret|token|authorization|cookie|email|phone|api[-_. " +
            "]?key|publishable[-_. ]?key|private[-_. ]?key|access[-_. ]?key|signing[-_. ]?key",
            RegexOptions.IgnoreCase | RegexOptions.Compiled);
        private readonly object _gate = new object();
        private readonly string _networkPath =
            Environment.GetEnvironmentVariable("REPROIT_NETWORK_FILE");
        private readonly string _actionPath =
            Environment.GetEnvironmentVariable("REPROIT_ACTION_FILE");
        private readonly string _actor =
            Environment.GetEnvironmentVariable("REPROIT_DEVICE") ?? "a";
        private readonly List<JsonElement> _exchanges = new List<JsonElement>();
        private readonly HashSet<int> _used = new HashSet<int>();
        private int _lastAction = -1;
        private int _ordinal;
        private readonly bool _replay;

        public ReproItCausalHandler(HttpMessageHandler innerHandler = null)
            : base(innerHandler ?? new HttpClientHandler())
        {
            string capsule = Environment.GetEnvironmentVariable("REPROIT_CAPSULE");
            if (!string.IsNullOrWhiteSpace(capsule))
            {
                _replay = true;
                try
                {
                    using JsonDocument document = JsonDocument.Parse(File.ReadAllText(capsule));
                    if (document.RootElement.TryGetProperty("exchanges", out JsonElement values))
                    {
                        foreach (JsonElement value in values.EnumerateArray())
                        {
                            _exchanges.Add(value.Clone());
                        }
                    }
                }
                catch
                {
                    // Invalid/missing capsules intentionally become an empty,
                    // fail-closed replay rather than silently using live data.
                }
            }
            if (_networkPath != null || _replay)
            {
                MergeCapabilities();
            }
        }

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (_networkPath == null && !_replay)
            {
                return await base.SendAsync(request, cancellationToken).ConfigureAwait(false);
            }
            int action = ReadAction();
            int ordinal;
            lock (_gate)
            {
                if (_lastAction != action)
                {
                    _lastAction = action;
                    _ordinal = 0;
                }
                ordinal = _ordinal++;
            }
            if (_replay)
            {
                JsonElement? match = null;
                lock (_gate)
                {
                    for (int index = 0; index < _exchanges.Count; index++)
                    {
                        JsonElement exchange = _exchanges[index];
                        if (_used.Contains(index) || !Bool(exchange, "required") ||
                            Text(exchange, "actor") != _actor ||
                            Number(exchange, "actionIndex", "action_index") != action ||
                            !string.Equals(Text(exchange, "method"), request.Method.Method,
                                           StringComparison.OrdinalIgnoreCase) ||
                            Canonical(Text(exchange, "url")) !=
                                Canonical(request.RequestUri?.AbsoluteUri))
                        {
                            continue;
                        }
                        _used.Add(index);
                        match = exchange;
                        break;
                    }
                }
                if (match == null)
                {
                    throw new HttpRequestException(
                        $"CAPSULE:MISS {request.Method} {request.RequestUri} action={action}");
                }
                return Replay(match.Value, request);
            }

            byte[] requestBytes =
                request.Content == null
                    ? Array.Empty<byte>()
                    : await request.Content.ReadAsByteArrayAsync(cancellationToken)
                          .ConfigureAwait(false);
            Dictionary<string, string> requestHeaders =
                Headers(request.Headers, request.Content?.Headers);
            HttpResponseMessage response =
                await base.SendAsync(request, cancellationToken).ConfigureAwait(false);
            byte[] responseBytes =
                response.Content == null
                    ? Array.Empty<byte>()
                    : await response.Content.ReadAsByteArrayAsync(cancellationToken)
                          .ConfigureAwait(false);
            Dictionary<string, string> responseHeaders =
                Headers(response.Headers, response.Content?.Headers);
            if (response.Content != null)
            {
                response.Content = CopyContent(responseBytes, response.Content.Headers);
            }
            var exchangeValue = new Dictionary<string, object> {
                ["id"] = $"{_actor}-{action}-{ordinal}",
                ["actor"] = _actor,
                ["actionIndex"] = action,
                ["ordinal"] = ordinal,
                ["protocol"] = request.RequestUri?.Scheme ?? "http",
                ["method"] = request.Method.Method,
                ["url"] = request.RequestUri?.AbsoluteUri ?? "",
                ["requestHeaders"] = RedactHeaders(requestHeaders),
                ["requestBody"] = Body(requestBytes, requestHeaders),
                ["status"] = (int)response.StatusCode,
                ["responseHeaders"] = RedactHeaders(responseHeaders),
                ["responseBody"] = Body(responseBytes, responseHeaders),
                ["required"] = true,
            };
            lock (_gate)
            {
                File.AppendAllText(_networkPath,
                                   JsonSerializer.Serialize(exchangeValue) + Environment.NewLine);
            }
            return response;
        }

        private static HttpResponseMessage Replay(JsonElement exchange, HttpRequestMessage request)
        {
            var response = new HttpResponseMessage(
                (HttpStatusCode)Number(exchange, "status")) { RequestMessage = request };
            JsonElement body = Property(exchange, "responseBody", "response_body");
            byte[] bytes = body.ValueKind == JsonValueKind.String
                               ? Encoding.UTF8.GetBytes(body.GetString() ?? "")
                           : body.ValueKind is JsonValueKind.Null or JsonValueKind.Undefined
                               ? Array.Empty<byte>()
                               : Encoding.UTF8.GetBytes(body.GetRawText());
            response.Content = new ByteArrayContent(bytes);
            JsonElement headers = Property(exchange, "responseHeaders", "response_headers");
            if (headers.ValueKind == JsonValueKind.Object)
            {
                foreach (JsonProperty header in headers.EnumerateObject())
                {
                    if (!response.Headers.TryAddWithoutValidation(header.Name,
                                                                  header.Value.GetString()))
                        response.Content.Headers.TryAddWithoutValidation(header.Name,
                                                                         header.Value.GetString());
                }
            }
            return response;
        }

        private int ReadAction()
        {
            try
            {
                return int.Parse(File.ReadAllText(_actionPath).Trim());
            }
            catch
            {
                return 0;
            }
        }

        private static string Canonical(string raw)
        {
            if (!Uri.TryCreate(raw, UriKind.Absolute, out Uri uri))
                return raw ?? "";
            var pairs = uri.Query.TrimStart('?')
                            .Split('&', StringSplitOptions.RemoveEmptyEntries)
                            .OrderBy(x => x, StringComparer.Ordinal);
            var builder = new UriBuilder(uri) { Host = uri.Host.ToLowerInvariant(),
                                                Scheme = uri.Scheme.ToLowerInvariant(),
                                                Query = string.Join("&", pairs) };
            return builder.Uri.AbsoluteUri;
        }

        private static Dictionary<string, string> Headers(
            System.Net.Http.Headers.HttpHeaders first, System.Net.Http.Headers.HttpHeaders second)
        {
            var result = first.ToDictionary(x => x.Key, x => string.Join(",", x.Value),
                                            StringComparer.OrdinalIgnoreCase);
            if (second != null)
                foreach (var item in second)
                    result[item.Key] = string.Join(",", item.Value);
            return result;
        }

        private static Dictionary<string, string> RedactHeaders(
            Dictionary<string, string> headers) =>
            headers.ToDictionary(x => x.Key,
                                 x => Secret.IsMatch(x.Key) ? "<reproit:secret>" : x.Value,
                                 StringComparer.OrdinalIgnoreCase);

        private static object Body(byte[] bytes, Dictionary<string, string> headers)
        {
            if (bytes.Length == 0)
                return null;
            bool json =
                headers.Any(x => x.Key.Equals("Content-Type", StringComparison.OrdinalIgnoreCase) &&
                                 x.Value.Contains("json", StringComparison.OrdinalIgnoreCase));
            if (!json)
                return $"<reproit:body:length={bytes.Length}>";
            try
            {
                using JsonDocument doc = JsonDocument.Parse(bytes);
                return Redact(doc.RootElement);
            }
            catch
            {
                return "<reproit:invalid-json>";
            }
        }

        private static object Redact(JsonElement value)
        {
            if (value.ValueKind == JsonValueKind.Object)
                return value.EnumerateObject().ToDictionary(
                    x => x.Name,
                    x => Secret.IsMatch(x.Name) ? (object) "<reproit:secret>" : Redact(x.Value));
            if (value.ValueKind == JsonValueKind.Array)
                return value.EnumerateArray().Select(Redact).ToArray();
            return value.ValueKind switch { JsonValueKind.String => value.GetString(),
                                            JsonValueKind.Number => value.GetRawText(),
                                            JsonValueKind.True => true,
                                            JsonValueKind.False => false,
                                            _ => null };
        }

        private static ByteArrayContent CopyContent(
            byte[] bytes, System.Net.Http.Headers.HttpContentHeaders headers)
        {
            var content = new ByteArrayContent(bytes);
            foreach (var header in headers)
                content.Headers.TryAddWithoutValidation(header.Key, header.Value);
            return content;
        }

        private static JsonElement Property(JsonElement value, params string[] names)
        {
            foreach (string name in names)
                if (value.TryGetProperty(name, out JsonElement found))
                    return found;
            return default;
        }
        private static string Text(JsonElement value, params string[] names)
        {
            JsonElement found = Property(value, names);
            return found.ValueKind == JsonValueKind.String ? found.GetString() : null;
        }
        private static int Number(JsonElement value, params string[] names)
        {
            JsonElement found = Property(value, names);
            return found.TryGetInt32(out int number) ? number : 0;
        }
        private static bool Bool(JsonElement value, params string[] names)
        {
            JsonElement found = Property(value, names);
            return found.ValueKind == JsonValueKind.True;
        }

        private static void MergeCapabilities()
        {
            string path = Environment.GetEnvironmentVariable("REPROIT_CAPABILITIES_FILE");
            if (string.IsNullOrWhiteSpace(path))
                return;
            Dictionary<string, object> root;
            try
            {
                root = JsonSerializer.Deserialize<Dictionary<string, object>>(
                           File.ReadAllText(path)) ??
                       new();
            }
            catch
            {
                root = new();
            }
            root["http"] = new { status = "captured", detail = ".NET HttpClient causal handler" };
            root["http_replay"] =
                new { status = "captured", detail = ".NET HttpClient fail-closed replay" };
            File.WriteAllText(path, JsonSerializer.Serialize(root));
        }
    }
}
