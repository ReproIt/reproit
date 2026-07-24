// C# port of sdk/test/event_batch_v1.js: mirror of reproit_protocol::EventBatch::validate
// scoped to the event kinds the production SDKs emit. Any batch this SDK builds must pass
// unchanged. Throws Xunit-friendly InvalidOperationException("<reason-code>") on the first
// defect, mirroring the protocol reason codes. Operates on the Json object model.

using System.Text;
using System.Text.RegularExpressions;

namespace ReproitBackend.Tests;

public static class EventBatchV1
{
    private const int MaxBatchFrames = 5000;
    private const int MaxBatchGraphs = 256;
    private const int MaxFrameBytes = 1024 * 1024;
    private const int MaxTokenBytes = 128;
    private const int MaxTextBytes = 16 * 1024;
    private const int MaxContextBytes = 64 * 1024;

    private static readonly Regex TokenPattern = new("^[A-Za-z0-9._:-]+$");
    private static readonly Regex LowerTokenPattern = new("^[a-z0-9_-]+$");
    private static readonly Regex ContractHashPattern = new("^[0-9a-f]{16}$");

    public static void ValidateEventBatch(object? batch)
    {
        var map = AsObject(batch, "malformed-frame");
        OnlyKeys(map, new[] { "version", "batchId", "appId", "deployment", "frames",
            "evidence" }, "invalid-event");
        if (!IsInteger(Get(map, "version"), out var version) || version != 1)
        {
            Fail("unsupported-version");
        }
        Token(Get(map, "batchId"));
        Token(Get(map, "appId"));
        if (map.TryGetValue("deployment", out var deployment) && deployment != null)
        {
            var fields = AsObject(deployment, "invalid-event");
            OnlyKeys(fields, new[] { "version", "commit" }, "invalid-event");
            var hasVersion = Get(fields, "version") != null;
            var hasCommit = Get(fields, "commit") != null;
            if (!hasVersion && !hasCommit) Fail("invalid-event");
            if (hasVersion) Token(Get(fields, "version"));
            if (hasCommit) Token(Get(fields, "commit"));
        }
        if (Get(map, "frames") is not List<object?> frames ||
            Get(map, "evidence") is not List<object?> evidence)
        {
            Fail("invalid-event");
            return;
        }
        if (frames.Count > MaxBatchFrames) Fail("batch-too-large");
        if (evidence.Count > MaxBatchGraphs) Fail("batch-too-large");
        if (frames.Count == 0 && evidence.Count == 0) Fail("invalid-event");
        long? lastSequence = null;
        foreach (var frame in frames)
        {
            ValidateFrame(frame);
            var sequence = (long)Get(AsObject(frame, "malformed-frame"), "sequence")!;
            if (lastSequence != null && sequence <= lastSequence) Fail("invalid-sequence");
            lastSequence = sequence;
        }
    }

    private static void ValidateFrame(object? frame)
    {
        var map = AsObject(frame, "malformed-frame");
        OnlyKeys(map, new[] { "runId", "sequence", "scope", "event" }, "malformed-frame");
        Token(Get(map, "runId"));
        if (!IsInteger(Get(map, "sequence"), out var sequence) || sequence < 0)
        {
            Fail("invalid-sequence");
        }
        ValidateScope(Get(map, "scope"));
        ValidateEvent(Get(map, "event"));
        if (Json.CanonicalUtf8(Get(map, "event")).Length > MaxFrameBytes)
        {
            Fail("frame-too-large");
        }
    }

    private static void ValidateScope(object? scope)
    {
        var map = AsObject(scope, "invalid-scope");
        var domain = Get(map, "domain") as string;
        if (domain == "shared" || domain == "backend")
        {
            OnlyKeys(map, new[] { "domain" }, "invalid-scope");
            return;
        }
        if (domain != "contract") Fail("invalid-scope");
        OnlyKeys(map, new[] { "domain", "contractHash" }, "invalid-scope");
        if (Get(map, "contractHash") is { } hash &&
            (hash is not string text || !ContractHashPattern.IsMatch(text)))
        {
            Fail("invalid-scope");
        }
    }

    private static void ValidateEvent(object? value)
    {
        var map = AsObject(value, "invalid-event");
        switch (Get(map, "kind") as string)
        {
            case "backend":
                OnlyKeys(map, new[] { "kind", "evidence" }, "invalid-event");
                ValueBytes(Get(map, "evidence"), MaxContextBytes);
                return;
            case "graph-edge":
                OnlyKeys(map, new[] { "kind", "from", "action", "to" }, "invalid-event");
                Text(Get(map, "from"), MaxTextBytes);
                Text(Get(map, "action"), MaxTextBytes);
                Text(Get(map, "to"), MaxTextBytes);
                return;
            case "finding":
                OnlyKeys(map, new[] { "kind", "signature", "message", "identity", "path",
                    "context" }, "invalid-event");
                Text(Get(map, "signature"), MaxTextBytes);
                Text(Get(map, "message"), MaxTextBytes);
                ValidateIdentity(Get(map, "identity"));
                if (Get(map, "path") is not List<object?> path || path.Count > 256)
                {
                    Fail("invalid-event");
                    return;
                }
                foreach (var step in path)
                {
                    var fields = AsObject(step, "invalid-event");
                    OnlyKeys(fields, new[] { "signature", "action", "label" },
                        "invalid-event");
                    Text(Get(fields, "signature"), MaxTextBytes);
                    Text(Get(fields, "action"), MaxTextBytes);
                    OptionalText(Get(fields, "label"), MaxTextBytes);
                }
                var context = AsObject(Get(map, "context"), "invalid-event");
                ValueBytes(context, MaxContextBytes);
                return;
            default:
                Fail("invalid-event");
                return;
        }
    }

    private static void ValidateIdentity(object? identity)
    {
        var map = AsObject(identity, "invalid-event");
        OnlyKeys(map, new[] { "oracle", "invariant", "kind", "message", "frame", "trigger",
            "boundary" }, "invalid-event");
        LowerToken(Get(map, "oracle"));
        foreach (var field in new[] { "invariant", "kind", "message", "frame", "trigger" })
        {
            Text(Get(map, field), MaxTextBytes);
        }
        OptionalText(Get(map, "boundary"), MaxTextBytes);
    }

    private static Dictionary<string, object?> AsObject(object? value, string reason)
    {
        if (value is Dictionary<string, object?> map) return map;
        Fail(reason);
        throw new InvalidOperationException("unreachable");
    }

    private static object? Get(Dictionary<string, object?> map, string key) =>
        map.TryGetValue(key, out var value) ? value : null;

    private static bool IsInteger(object? value, out long result)
    {
        if (value is long integer)
        {
            result = integer;
            return true;
        }
        result = 0;
        return false;
    }

    private static void OnlyKeys(
        Dictionary<string, object?> map, string[] allowed, string reason)
    {
        foreach (var key in map.Keys)
        {
            if (!allowed.Contains(key)) Fail(reason);
        }
    }

    private static void Token(object? value)
    {
        if (value is not string text || text.Length == 0 ||
            Encoding.UTF8.GetByteCount(text) > MaxTokenBytes || !TokenPattern.IsMatch(text))
        {
            Fail("invalid-event");
        }
    }

    private static void LowerToken(object? value)
    {
        Token(value);
        if (!LowerTokenPattern.IsMatch((string)value!)) Fail("invalid-event");
    }

    private static void Text(object? value, int maxBytes)
    {
        if (value is not string text || Encoding.UTF8.GetByteCount(text) > maxBytes)
        {
            Fail("invalid-event");
        }
    }

    private static void OptionalText(object? value, int maxBytes)
    {
        if (value != null) Text(value, maxBytes);
    }

    private static void ValueBytes(object? value, int maxBytes)
    {
        if (Json.CanonicalUtf8(value).Length > maxBytes) Fail("invalid-event");
    }

    private static void Fail(string reason) => throw new InvalidOperationException(reason);
}
