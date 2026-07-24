// Canonical encoding tests: serde_json/BTreeMap semantics plus golden-bytes parity with the
// Node SDK's canonicalJson (the family's verified wire reference) on a shared fixture.

using System.Diagnostics;
using Xunit;

namespace ReproitBackend.Tests;

public class CanonicalJsonTests
{
    // A fixture that exercises key sorting, escaping, unicode, numbers, and nesting. Kept as
    // JSON text so both sides encode the exact same parsed value.
    private const string Fixture =
        "{\"zeta\":1,\"alpha\":{\"b\":[1,2.5,-3,0.125,1e+30,true,false,null],\"a\":\"x\"}," +
        "\"quote\":\"he said \\\"hi\\\"\\n\\t\\\\ done\",\"unicode\":\"café 日本" +
        "\",\"ctrl\":\"a\\u0001b\",\"empty\":{},\"list\":[],\"big\":9007199254740991," +
        "\"neg\":-42,\"frac\":\"2.0\"}";

    [Fact]
    public void SortsKeysRecursivelyAndEncodesCompactly()
    {
        var value = Json.Parse("{\"b\":{\"d\":1,\"c\":2},\"a\":[{\"z\":1,\"y\":2}]}");
        Assert.Equal("{\"a\":[{\"y\":2,\"z\":1}],\"b\":{\"c\":2,\"d\":1}}",
            Json.Canonical(value));
    }

    [Fact]
    public void MatchesTheNodeReferenceByteForByte()
    {
        var repoRoot = FindRepoRoot();
        var script =
            "const { canonicalJson } = require(process.argv[1]);" +
            "let raw = '';" +
            "process.stdin.on('data', (chunk) => (raw += chunk));" +
            "process.stdin.on('end', () => process.stdout.write(canonicalJson(JSON.parse(raw))));";
        var start = new ProcessStartInfo("node")
        {
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        start.ArgumentList.Add("-e");
        start.ArgumentList.Add(script);
        start.ArgumentList.Add(Path.Combine(repoRoot, "sdk/reproit-backend-node/index.js"));
        using var process = Process.Start(start)!;
        process.StandardInput.Write(Fixture);
        process.StandardInput.Close();
        var expected = process.StandardOutput.ReadToEnd();
        Assert.True(process.WaitForExit(15000), "node did not exit");
        Assert.Equal(0, process.ExitCode);
        Assert.Equal(expected, Json.Canonical(Json.Parse(Fixture)));
    }

    // The test runs from bin/<config>/net8.0; the repo root holds sdk/reproit-backend-node.
    // REPROIT_CLI_ROOT overrides for out-of-tree runs.
    private static string FindRepoRoot()
    {
        var overridden = Environment.GetEnvironmentVariable("REPROIT_CLI_ROOT");
        if (overridden != null) return overridden;
        var current = new DirectoryInfo(AppContext.BaseDirectory);
        for (var depth = 0; current != null && depth < 12; depth++)
        {
            if (File.Exists(Path.Combine(current.FullName,
                "sdk/reproit-backend-node/index.js")))
            {
                return current.FullName;
            }
            current = current.Parent;
        }
        throw new InvalidOperationException(
            "reproit-cli repo root not found; set REPROIT_CLI_ROOT");
    }
}
