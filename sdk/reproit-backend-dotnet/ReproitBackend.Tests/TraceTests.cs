// Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs, mirroring
// sdk/reproit-backend-node/test/trace.test.js.

using System.Text;
using Xunit;

namespace ReproitBackend.Tests;

public class TraceTests
{
    private static TraceContext Context() => new() { TraceId = "trace-a" };

    private static Dictionary<string, object?> Event(BackendTrace trace, int index) =>
        trace.Events()[index];

    private static Dictionary<string, object?> Sub(object? value, string key) =>
        (Dictionary<string, object?>)((Dictionary<string, object?>)value!)[key]!;

    internal static byte[] DecodeBase64Url(string header)
    {
        var padded = header.Replace('-', '+').Replace('_', '/');
        return Convert.FromBase64String(padded + new string('=', (4 - padded.Length % 4) % 4));
    }

    [Fact]
    public void EmitsBoundedCorrelatedRedactedEvents()
    {
        var headers = new Dictionary<string, string>
        {
            ["x-reproit-trace"] = "trace-a",
            ["x-reproit-actor"] = "alice",
            ["x-reproit-action"] = "7",
            ["x-reproit-build"] = "build-a",
            ["x-reproit-config-contract"] = "contract-a",
        };
        var parsed = Reproit.TraceContextFromHeaders(
            name => headers.TryGetValue(name, out var value) ? value : null)!;
        var trace = BackendTrace.Begin(parsed, "createProject", new BeginOptions
        {
            Tenant = "org-1",
            IdempotencyKey = "retry-secret",
            Input = new Dictionary<string, object?>
            {
                ["name"] = "demo",
                ["password"] = "abcdefgh",
            },
            Selections = new[] { Reproit.Selection("project.id", "projectId")! },
        });
        trace.Effect("write", new EffectOptions
        {
            Resource = "projects", Key = "1", Tenant = "org-1",
        });
        trace.Finish(new Dictionary<string, object?>
        {
            ["id"] = 1L,
            ["apiKey"] = "sk_live_secret",
            ["publishable_key"] = "pk_live_secret",
            ["private-key"] = "private-secret",
            ["access key"] = "access-secret",
            ["signingKey"] = "signing-secret",
            ["monkey"] = "harmless",
        }, 201, true, true);
        Assert.True(trace.Header().Length < Reproit.MaxHeaderBytes);
        Assert.Equal(7L, Event(trace, 0)["actionIndex"]);
        Assert.Equal("build-a", Event(trace, 0)["build"]);
        Assert.Equal("contract-a", Event(trace, 0)["configContract"]);
        Assert.Equal(8L, Sub(Sub(Event(trace, 0)["input"], "password"), "$reproit")["length"]);
        var identity = (string)Event(trace, 0)["idempotencyKey"]!;
        Assert.NotEqual("retry-secret", identity);
        Assert.Matches("^sha256:[0-9a-f]{24}$", identity);
        foreach (var field in new[] { "apiKey", "publishable_key", "private-key",
            "access key", "signingKey" })
        {
            Assert.Equal(true, Sub(Sub(Event(trace, 2)["output"], field), "$reproit")["redacted"]);
        }
        Assert.Equal("harmless", ((Dictionary<string, object?>)
            Event(trace, 2)["output"]!)["monkey"]);
        Assert.Equal(true, Event(trace, 2)["effectsComplete"]);
    }

    [Fact]
    public void StaysInactiveWithoutATraceHeader()
    {
        Assert.Null(Reproit.TraceContextFromHeaders(_ => null));
        Assert.Null(Reproit.TraceContextFromHeaders(
            name => name == "x-reproit-trace" ? "  " : null));
    }

    [Fact]
    public void HeaderIsUnpaddedBase64UrlOfTheCanonicalEventJson()
    {
        var trace = BackendTrace.Begin(Context(), "op", new BeginOptions
        {
            Input = new Dictionary<string, object?> { ["b"] = 1L, ["a"] = 2L },
        });
        trace.Finish(new Dictionary<string, object?> { ["ok"] = true }, 200, true, true);
        var header = trace.Header();
        Assert.DoesNotMatch("[+/=]", header);
        var raw = Encoding.UTF8.GetString(DecodeBase64Url(header));
        Assert.Equal(Json.Canonical(trace.Events()), raw);
        // Keys are sorted (serde_json BTreeMap order in the Rust adapter).
        Assert.True(raw.IndexOf("\"a\":2", StringComparison.Ordinal) <
            raw.IndexOf("\"b\":1", StringComparison.Ordinal));
    }

    [Fact]
    public void RejectsEffectsAfterReturnAndASecondReturn()
    {
        var trace = BackendTrace.Begin(Context(), "op");
        trace.Finish(null, 200, true, false);
        Assert.Equal("AlreadyFinished",
            Assert.Throws<TraceException>(() => trace.Effect("read")).Code);
        Assert.Equal("AlreadyFinished",
            Assert.Throws<TraceException>(() => trace.Finish(null, 200, true, false)).Code);
    }

    [Fact]
    public void HeaderBeforeFinishIsRejectedOversizedHeaderIsRejected()
    {
        var open = BackendTrace.Begin(Context(), "op");
        Assert.Equal("AlreadyFinished",
            Assert.Throws<TraceException>(() => open.Header()).Code);
        var big = BackendTrace.Begin(Context(), "op");
        big.Finish(new Dictionary<string, object?>
        {
            ["blob"] = new string('x', Reproit.MaxHeaderBytes),
        }, 200, true, true);
        Assert.Equal("HeaderTooLarge",
            Assert.Throws<TraceException>(() => big.Header()).Code);
    }

    [Fact]
    public void EventCountIsCappedAt256()
    {
        var trace = BackendTrace.Begin(Context(), "op");
        for (var index = 1; index < Reproit.MaxEvents; index++)
        {
            trace.Effect("emit", new EffectOptions { Event = "tick" });
        }
        Assert.Equal("TooManyEvents",
            Assert.Throws<TraceException>(() => trace.Effect("emit")).Code);
        Assert.Equal("TooManyEvents",
            Assert.Throws<TraceException>(() => trace.Finish(null, 200, true, false)).Code);
    }

    [Fact]
    public void TypedEffectsOnlyBoundedIdentifiersOnly()
    {
        var trace = BackendTrace.Begin(Context(), "op");
        Assert.Equal("InvalidOperation",
            Assert.Throws<TraceException>(() => trace.Effect("mutate")).Code);
        Assert.Equal("InvalidOperation",
            Assert.Throws<TraceException>(() => BackendTrace.Begin(Context(), "")).Code);
        Assert.Equal("InvalidOperation",
            Assert.Throws<TraceException>(
                () => BackendTrace.Begin(Context(), new string('x', 257))).Code);
    }

    [Fact]
    public void EffectDetailKeepsOnlyBeforeAfterPayloadAfterRedaction()
    {
        var trace = BackendTrace.Begin(Context(), "op");
        trace.Effect("write", new EffectOptions
        {
            Resource = "users",
            Detail = new Dictionary<string, object?>
            {
                ["before"] = new Dictionary<string, object?> { ["email"] = "a@b.c" },
                ["after"] = new Dictionary<string, object?> { ["name"] = "z" },
                ["extra"] = "dropped",
            },
        });
        var effect = Event(trace, 1);
        Assert.Equal(true, Sub(Sub(effect["before"], "email"), "$reproit")["redacted"]);
        Assert.Equal("z", ((Dictionary<string, object?>)effect["after"]!)["name"]);
        Assert.False(effect.ContainsKey("extra"));
    }

    [Fact]
    public void CanonicalHttpInputLowercasesHeadersAndPreservesRepeatedValues()
    {
        var input = Reproit.HttpInput(
            body: new Dictionary<string, object?> { ["name"] = "demo" },
            path: new Dictionary<string, object?> { ["project"] = "p1" },
            query: new Dictionary<string, object?>
            {
                ["tag"] = new List<object?> { "a", "b" },
            },
            headers: new Dictionary<string, object?> { ["X-Mode"] = "safe" });
        Assert.Equal("safe", ((Dictionary<string, object?>)input["headers"]!)["x-mode"]);
        Assert.Equal(new List<object?> { "a", "b" },
            ((Dictionary<string, object?>)input["query"]!)["tag"]);
        Assert.Empty(Reproit.HttpInput(
            path: new Dictionary<string, object?>(),
            query: new Dictionary<string, object?>(),
            headers: new Dictionary<string, object?>()));
    }

    [Fact]
    public void SelectionsValidateTheirPaths()
    {
        Assert.NotNull(Reproit.Selection("project.id", "projectId"));
        Assert.NotNull(Reproit.Selection("items[].id", "rows[].id", "Widget"));
        Assert.Null(Reproit.Selection("1bad", "ok"));
        Assert.Null(Reproit.Selection("ok", "ok", "Bad.Condition"));
    }
}
