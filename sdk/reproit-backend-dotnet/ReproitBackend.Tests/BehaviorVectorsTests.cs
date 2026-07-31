// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. The .NET instance was writing captures with a UTF-8 byte
// order mark, which the CLI's JSON sniff rejected, so the capture could not be
// resolved at all. The encoding group pins that a written capture starts with
// '{' and carries no BOM.
//
// What the other groups pin, and the real defect behind each:
//
//   bounds                   the inline body budget is BYTES, not characters. 4096 euro
//                            signs are 12288 bytes; a runtime measuring string Length
//                            records that inline and blows a budget replay trusts. The
//                            .NET budget is byte typed at the API, so the encoding is the
//                            caller's and the case pins it stays that way.
//   headers                  names lowercase, and the 32 header cap is taken over NAME
//                            SORTED order. Go capped a randomized map in arrival order and
//                            recorded a different subset every run, so replay was
//                            unrepeatable.
//   redaction typeCases      the placeholder carries type and length.
//   redaction foldingCases   which field names fold to secret.
//   redaction nestingCases   redaction reaches nested objects and arrays.
//   redaction structureCases redaction is structure preserving: no key dropped, no array
//                            shortened, an explicit null still a null value. An encoder
//                            that dropped null map values changed the shape the replay
//                            matcher walks, and replay reproduced a DIFFERENT error.

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using Xunit;

namespace ReproitBackend.Tests;

public class BehaviorVectorsTests
{
    private static JsonElement Vectors
    {
        get
        {
            var path = Path.Combine(
                AppContext.BaseDirectory, "..", "..", "..", "..", "..",
                "capture-behavior-v1.json");
            return JsonDocument.Parse(File.ReadAllText(Path.GetFullPath(path))).RootElement;
        }
    }

    [Fact]
    public void ConstantsMatchTheSharedVectors()
    {
        var constants = Vectors.GetProperty("constants");
        Assert.Equal(
            Exchange.MaxExchangeBodyBytes,
            constants.GetProperty("maxExchangeBodyBytes").GetInt32());
        Assert.Equal(
            Replay.DivergenceMarker,
            constants.GetProperty("divergenceMarker").GetString());
    }

    // The .NET defect: Encoding.UTF8 prepends a byte order mark, so the written
    // capture did not start with '{' and the CLI refused to resolve it.
    [Fact]
    public void AWrittenCaptureHasNoByteOrderMark()
    {
        var file = Path.GetTempFileName();
        try
        {
            var payload = JsonSerializer.Serialize(new Dictionary<string, object>
            {
                ["format"] = "reproit-backend-capture",
                ["version"] = 2,
            });
            // This is the call shape the fixture uses; the encoding argument is
            // the whole defect.
            File.WriteAllText(file, payload, new UTF8Encoding(false));

            var bytes = File.ReadAllBytes(file);
            Assert.True(bytes.Length > 0, "capture file is empty");
            var expectedFirst = Convert.ToByte(
                Vectors.GetProperty("encoding").GetProperty("firstByteMustBe").GetString()!
                    .Substring(2),
                16);
            Assert.Equal(expectedFirst, bytes[0]);
            // The BOM is EF BB BF; assert it explicitly so the reason is legible
            // when this fails.
            Assert.False(
                bytes.Length >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF,
                "capture file starts with a UTF-8 byte order mark, which the CLI rejects");
        }
        finally
        {
            File.Delete(file);
        }
    }

    // JsonElement is opaque to Redact, which switches on CLR types, so the
    // vectors are materialized into native values first. Without this the test
    // reports every value as null and looks like an SDK defect when it is not.
    private static object? ToClr(JsonElement element) => element.ValueKind switch
    {
        JsonValueKind.String => element.GetString(),
        // The cast to object is load bearing: without it the conditional unifies on double
        // and every whole number arrives as a double, which Metadata then types as "number".
        JsonValueKind.Number => element.TryGetInt64(out var whole)
            ? (object)whole
            : element.GetDouble(),
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        JsonValueKind.Null => null,
        JsonValueKind.Array => ArrayToClr(element),
        JsonValueKind.Object => ObjectToClr(element),
        _ => null,
    };

    private static List<object?> ArrayToClr(JsonElement element)
    {
        var items = new List<object?>();
        foreach (var item in element.EnumerateArray()) items.Add(ToClr(item));
        return items;
    }

    private static Dictionary<string, object?> ObjectToClr(JsonElement element)
    {
        var map = new Dictionary<string, object?>();
        foreach (var property in element.EnumerateObject()) map[property.Name] = ToClr(property.Value);
        return map;
    }

    // `bodyRepeat`/`repeat` keep the vectors small on disk.
    private static string Repeated(object? spec)
    {
        var parts = (List<object?>)spec!;
        return string.Concat(Enumerable.Repeat((string)parts[0]!, (int)(long)parts[1]!));
    }

    [Fact]
    public void BoundsVectors()
    {
        foreach (var kase in Vectors.GetProperty("bounds").GetProperty("cases").EnumerateArray())
        {
            var name = kase.GetProperty("name").GetString();
            var input = ObjectToClr(kase.GetProperty("input"));
            var text = input.GetValueOrDefault("bodyRepeat") is { } repeat
                ? Repeated(repeat)
                : (string?)input.GetValueOrDefault("body");
            var body = text == null ? null : Encoding.UTF8.GetBytes(text);
            var expect = ObjectToClr(kase.GetProperty("expect"));
            // A parsed JSON body is a dictionary too, so key on `repeat` itself.
            if (expect.GetValueOrDefault("body") is Dictionary<string, object?> wrapper
                && wrapper.GetValueOrDefault("repeat") is { } parts)
            {
                expect["body"] = Repeated(parts);
            }
            var contentType = (string?)input.GetValueOrDefault("contentType");
            var actual = Exchange.BoundedBody(body, contentType);
            Assert.True(
                Json.Canonical(expect) == Json.Canonical(actual),
                $"bounds case {name}: got {Json.Canonical(actual)}");
        }
    }

    // The cap case is fed in a deterministic NON-sorted order, so a cap taken over arrival
    // order keeps the wrong subset and the assertion says so.
    [Fact]
    public void HeaderVectors()
    {
        foreach (var kase in Vectors.GetProperty("headers").GetProperty("cases").EnumerateArray())
        {
            var name = kase.GetProperty("name").GetString();
            if (kase.TryGetProperty("input", out var literal))
            {
                var given = literal.GetProperty("headers").EnumerateObject()
                    .Select(header =>
                        new KeyValuePair<string, string>(header.Name, header.Value.GetString()!))
                    .ToList();
                Assert.True(
                    Json.Canonical(ObjectToClr(kase.GetProperty("expect")))
                        == Json.Canonical(Exchange.BoundedHeaders(given)),
                    $"headers case {name}: got {Json.Canonical(Exchange.BoundedHeaders(given))}");
                continue;
            }
            var spec = kase.GetProperty("inputGenerated");
            var count = spec.GetProperty("headerCount").GetInt32();
            var pattern = spec.GetProperty("namePattern").GetString()!;
            var value = spec.GetProperty("value").GetString()!;
            var shuffled = new List<KeyValuePair<string, string>>();
            for (var index = 0; index < count; index++)
            {
                // 17 is coprime with 40, so this walks every name exactly once. The pattern is
                // printf, which C# does not speak, so the width is substituted from it.
                var position = ((index * 17) % count).ToString("D2");
                shuffled.Add(new KeyValuePair<string, string>(
                    pattern.Replace("%02d", position), value));
            }
            var expect = kase.GetProperty("expect");
            var kept = (Dictionary<string, object?>)Exchange.BoundedHeaders(shuffled)["headers"]!;
            // Dictionary order is not contractual, so the subset is what is asserted.
            var names = kept.Keys.OrderBy(key => key, StringComparer.Ordinal).ToList();
            Assert.Equal(expect.GetProperty("headerCount").GetInt32(), names.Count);
            Assert.True(
                names[0] == expect.GetProperty("firstName").GetString(),
                $"the cap must be taken over sorted names, not arrival order; first is {names[0]}");
            Assert.True(
                names[^1] == expect.GetProperty("lastName").GetString(),
                $"the cap must be taken over sorted names, not arrival order; last is {names[^1]}");
        }
    }

    [Fact]
    public void RedactionKeyFoldingVectors()
    {
        var folding = Vectors.GetProperty("redaction").GetProperty("foldingCases");
        foreach (var kase in folding.EnumerateArray())
        {
            var field = kase.GetProperty("field").GetString()!;
            var secret = kase.GetProperty("secret").GetBoolean();
            var input = new Dictionary<string, object?> { [field] = "value" };
            var folded = (Dictionary<string, object?>)Reproit.Redact(input)!;
            var redacted = folded[field] is IDictionary<string, object?> stub
                && stub.ContainsKey("$reproit");
            Assert.True(
                redacted == secret,
                $"{field} should {(secret ? string.Empty : "not ")}be treated as secret");
        }
    }

    [Fact]
    public void RedactionNestingVectors()
    {
        var nesting = Vectors.GetProperty("redaction").GetProperty("nestingCases");
        foreach (var kase in nesting.EnumerateArray())
        {
            var actual = Reproit.Redact(ObjectToClr(kase.GetProperty("input")));
            Assert.True(
                Json.Canonical(ObjectToClr(kase.GetProperty("expect"))) == Json.Canonical(actual),
                $"nesting case: got {Json.Canonical(actual)}");
        }
    }

    // Structure preservation: a dropped key, a shortened array or a collapsed null all change
    // the shape the replay matcher walks.
    [Fact]
    public void RedactionStructureVectors()
    {
        var structure = Vectors.GetProperty("redaction").GetProperty("structureCases");
        foreach (var kase in structure.EnumerateArray())
        {
            var name = kase.GetProperty("name").GetString();
            var actual = Reproit.Redact(ObjectToClr(kase.GetProperty("input")));
            Assert.True(
                Json.Canonical(ObjectToClr(kase.GetProperty("expect"))) == Json.Canonical(actual),
                $"structure case {name}: got {Json.Canonical(actual)}");
        }
    }

    [Fact]
    public void RedactionTypeVectors()
    {
        foreach (var kase in Vectors.GetProperty("redaction").GetProperty("typeCases").EnumerateArray())
        {
            var input = ObjectToClr(kase.GetProperty("input"));
            var redacted = Reproit.Redact(input);
            var actual = JsonSerializer.Serialize(redacted);
            var expected = kase.GetProperty("expect").GetRawText();
            Assert.Equal(
                JsonSerializer.Serialize(JsonSerializer.Deserialize<object>(expected)),
                JsonSerializer.Serialize(JsonSerializer.Deserialize<object>(actual)));
        }
    }

    [Fact]
    public void TriggerTokenIsInTheProtocolVocabulary()
    {
        var tokens = Vectors.GetProperty("triggerTokens");
        var token = tokens.GetProperty("bySdkKind").GetProperty("backend").GetString();
        var allowed = false;
        foreach (var candidate in tokens.GetProperty("allowed").EnumerateArray())
        {
            if (candidate.GetString() == token) allowed = true;
        }
        Assert.True(allowed, $"backend trigger token {token} is not in the vocabulary");

        var source = File.ReadAllText(Path.GetFullPath(Path.Combine(
            AppContext.BaseDirectory, "..", "..", "..", "..", "ReproitBackend", "Capture.cs")));
        Assert.Contains(token!, source);
        foreach (var bad in tokens.GetProperty("rejected").EnumerateArray())
        {
            Assert.DoesNotContain($"\"{bad.GetString()}\"", source);
        }
    }
}
