// The .NET side of sdk/test/backend_replay_parity_test.js: reads the shared capsule on
// stdin, replays the same two probes the Node reference and Python port replay, and prints
// the observable bytes (served SSE exchange, 599 divergence body, REPROIT:DIVERGENCE marker
// line) as JSON on the last stdout line, where the test byte-compares them.

using System.Text;
using ReproitBackend;

var payloadText = Console.In.ReadToEnd();
var path = Path.Combine(Path.GetTempPath(), "reproit-dotnet-parity-" +
    Guid.NewGuid().ToString("n") + ".json");
File.WriteAllText(path, payloadText, new UTF8Encoding(false));
var session = Replay.Load(path);
File.Delete(path);
if (session == null)
{
    Console.Error.WriteLine("stdin is not a reproit-backend-capture payload");
    return 1;
}

var served = session.ServeHttp(new Dictionary<string, object?>
{
    ["method"] = "GET",
    ["url"] = "http://llm.internal/stream",
});

static Dictionary<string, object?> Message(string role, string content) => new()
{
    ["role"] = role,
    ["content"] = content,
};

var original = Console.Error;
var held = new StringWriter();
Console.SetError(held);
Replay.Served diverged;
try
{
    diverged = session.ServeHttp(new Dictionary<string, object?>
    {
        ["method"] = "POST",
        ["url"] = "http://llm.internal/v1/chat",
        ["body"] = new Dictionary<string, object?>
        {
            ["messages"] = new List<object?>
            {
                Message("user", "hello"),
                Message("assistant", "hi"),
                Message("user", "DIFFERENT QUESTION"),
            },
        },
    });
}
finally
{
    Console.SetError(original);
}
var marker = held.ToString().Split('\n')
    .FirstOrDefault(line => line.StartsWith(Replay.DivergenceMarker, StringComparison.Ordinal));
if (marker == null)
{
    Console.Error.WriteLine("the diverged probe produced no REPROIT:DIVERGENCE line");
    return 1;
}

Console.WriteLine(Json.Compact(new Dictionary<string, object?>
{
    ["serve"] = new Dictionary<string, object?>
    {
        ["status"] = (long)served.Status,
        ["bodyText"] = served.BodyText,
        ["chunks"] = (served.Chunks ?? new List<byte[]>())
            .Select(chunk => (object?)Encoding.UTF8.GetString(chunk)).ToList(),
    },
    ["divergedBody"] = diverged.BodyText,
    ["marker"] = marker,
}));
return 0;
