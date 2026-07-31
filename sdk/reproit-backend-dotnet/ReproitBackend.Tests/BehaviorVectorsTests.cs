// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. The .NET instance was writing captures with a UTF-8 byte
// order mark, which the CLI's JSON sniff rejected, so the capture could not be
// resolved at all. The encoding group pins that a written capture starts with
// '{' and carries no BOM.

using System;
using System.Collections.Generic;
using System.IO;
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
        JsonValueKind.Number => element.TryGetInt64(out var whole)
            ? whole
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
