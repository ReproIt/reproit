// CI capture mode for reproit-backend-dotnet: the flaky-CI wedge.
//
// `Ci.TestAsync(suite, test, body)` wraps an xUnit test body with a trigger identity that is
// the TEST (suite + test id), not an inbound HTTP request. With `REPROIT_CI_CAPTURE=1` every
// wrapped test runs inside its own trace, so the explicit Instrument boundary (HttpClient
// handler, Ado.Wrap, Db.RunAsync) records dependency exchanges and the determinism envelope
// exactly as production capture does; a FAILING test emits a version-2
// `reproit-backend-capture` capsule to a bounded on-disk spool. With `REPROIT_REPLAY` set the
// SAME wrapper re-runs only the capsule's named test while the SDK serves the recorded
// exchanges in process, and reports the observed result as a structured stderr marker for
// `reproit check`. Without either env the wrapper runs the body untouched.
//
// The wire is the existing capture payload: the test identity rides in the `operation` field
// as `test:<suite>#<test>`, and the failed assertion is the existing
// `backend-authored-invariant` registry oracle (a test IS an authored invariant). No new
// protocol fields, no new oracle ids.
//
// Marker delivery under `dotnet test`: the VSTest host swallows raw test console output, so
// the stderr markers only reach the invoking process when the run adds
// `--logger "console;verbosity=detailed"` (which re-prints captured output verbatim, at
// column 0, on stdout) and redirects stdout to stderr (`1>&2`). The dotnet-flaky-ci-e2e gate
// pins that command shape; a plain `dotnet test` still spools capsules (the spool is a
// directory, not a stream) but reports nothing.
//
// Honest limit: replay pins the envelope and the recorded exchanges, which is the whole
// boundary this SDK can see. A race the boundary cannot see (scheduling, shared memory) is
// not reproduced by this capsule; `reproit check` reports such runs Inconclusive, never a
// fake reproduction.

using System.Security.Cryptography;

namespace ReproitBackend;

public static class Ci
{
    // Test-trigger identity prefix inside the existing `operation` field.
    public const string TestTriggerPrefix = "test:";
    // The registry oracle a failed test capsule carries: an authored invariant (the test's
    // own assertion) was violated. Existing id, not a new one.
    public const string TestFailureOracle = "backend-authored-invariant";
    // Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE.
    public const string ResultMarker = "REPROIT:CI-TEST ";
    public const string SpoolMarker = "REPROIT:CI-CAPSULE ";

    // Spool bounds. The cap covers the TOTAL bytes on disk; capsules beyond it are dropped
    // and counted (in-process stats plus the on-disk `dropped.count`), never silently.
    public const string DefaultSpoolDir = ".reproit/ci-spool";
    public const long DefaultSpoolMaxBytes = 16L * 1024 * 1024;
    private const long SpoolMaxFloorBytes = 4 * 1024;
    private const long SpoolMaxCeilBytes = 64L * 1024 * 1024;
    // Suite and test names share the operation field's 256-code-point bound.
    private const int MaxName = 120;
    private const int MaxErrorChars = 2048;

    private static long _traceSequence;
    private static long _spooledCapsules;
    private static long _droppedCapsules;
    private static long _failedCaptures;

    // Env seam so tests can state the environment they need instead of mutating the process,
    // same pattern as Capture.ReadEnvironment.
    internal static Func<string, string?> ReadEnvironment { get; set; } =
        Environment.GetEnvironmentVariable;

    // Marker sink: the REAL process stderr (Console.OpenStandardError), not Console.Error,
    // because test hosts redirect Console streams. Test-settable.
    internal static TextWriter Stderr { get; set; } = TextWriter.Synchronized(
        new StreamWriter(Console.OpenStandardError()) { AutoFlush = true });

    public static Dictionary<string, long> Stats() => new()
    {
        ["spooledCapsules"] = Interlocked.Read(ref _spooledCapsules),
        ["droppedCapsules"] = Interlocked.Read(ref _droppedCapsules),
        ["failedCaptures"] = Interlocked.Read(ref _failedCaptures),
    };

    // Run one wrapped test body under the mode the environment selects. Capture and replay
    // both rethrow the body's failure so the test framework's own verdict is untouched.
    public static Task TestAsync(string suite, string test, Func<Task> body)
    {
        if (ReplayPath() != null) return ReplayTestAsync(suite, test, body);
        if (ReadEnvironment("REPROIT_CI_CAPTURE") == "1")
        {
            return CaptureTestAsync(suite, test, body);
        }
        return body();
    }

    private static string? ReplayPath()
    {
        var value = ReadEnvironment("REPROIT_REPLAY");
        return string.IsNullOrEmpty(value) ? null : value;
    }

    private static string BoundedName(string value) =>
        Reproit.TruncateCodePoints(value.Trim(), MaxName);

    internal static string OperationFor(string suite, string test) =>
        TestTriggerPrefix + BoundedName(suite) + "#" + BoundedName(test);

    private static string BoundedError(Exception error)
    {
        var message = error.Message;
        return message.Length > MaxErrorChars ? message[..MaxErrorChars] : message;
    }

    // Synthesized trace context: the CI job stands where production stood.
    private static TraceContext CiContext()
    {
        string? commit = null;
        foreach (var candidate in new[]
        {
            ReadEnvironment("REPROIT_COMMIT"),
            ReadEnvironment("GITHUB_SHA"),
        })
        {
            if (Capture.ValidToken(candidate))
            {
                commit = candidate;
                break;
            }
        }
        var sequence = Interlocked.Increment(ref _traceSequence);
        return new TraceContext
        {
            TraceId = "ci-" + DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() + "-" + sequence,
            ActionIndex = 0,
            Build = commit,
            CaptureEnvelope = true,
        };
    }

    private static async Task CaptureTestAsync(string suite, string test, Func<Task> body)
    {
        var operation = OperationFor(suite, test);
        var trace = BackendTrace.Begin(CiContext(), operation, new BeginOptions
        {
            Input = new Dictionary<string, object?>
            {
                ["suite"] = BoundedName(suite),
                ["test"] = BoundedName(test),
            },
        });
        try
        {
            await Instrument.ScopeAsync(trace, body).ConfigureAwait(false);
        }
        catch (Exception error)
        {
            FinishAndSpool(trace, operation, error);
            throw;
        }
        try
        {
            trace.Finish(null, null, true, false);
        }
        catch (TraceException)
        {
            // An over-long passing trace has nothing to spool anyway.
        }
    }

    // Same envelope shape production capture records; the seed pins the REPLAY run's
    // randomness, it does not reproduce the test run's.
    private static Dictionary<string, object?> EnvelopeFor(BackendTrace trace)
    {
        var first = trace.Events().FirstOrDefault();
        return Capture.DeterminismEnvelope(first?.GetValueOrDefault("at") as long?);
    }

    private static void FinishAndSpool(BackendTrace trace, string operation, Exception error)
    {
        try
        {
            trace.Finish(
                new Dictionary<string, object?> { ["error"] = BoundedError(error) },
                null, false, false);
            var body = Json.CanonicalUtf8(new Dictionary<string, object?>
            {
                ["format"] = Capture.CaptureFormat,
                ["version"] = 2L,
                ["operation"] = operation,
                ["oracle"] = TestFailureOracle,
                ["envelope"] = EnvelopeFor(trace),
                ["events"] = trace.Events(),
            });
            Spool(body, operation, SpoolDir(), SpoolMaxBytes());
        }
        catch (Exception)
        {
            // Capture must never mask the test's own failure.
            Interlocked.Increment(ref _failedCaptures);
        }
    }

    internal static string SpoolDir()
    {
        var dir = ReadEnvironment("REPROIT_CI_SPOOL");
        return string.IsNullOrEmpty(dir) ? DefaultSpoolDir : dir;
    }

    internal static long SpoolMaxBytes()
    {
        var raw = ReadEnvironment("REPROIT_CI_SPOOL_MAX");
        if (raw == null || !long.TryParse(raw, out var parsed)) return DefaultSpoolMaxBytes;
        return Math.Min(SpoolMaxCeilBytes, Math.Max(SpoolMaxFloorBytes, parsed));
    }

    private static void RecordDrop(string dir)
    {
        var counter = Path.Combine(dir, "dropped.count");
        var dropped = 0;
        try
        {
            dropped = int.TryParse(File.ReadAllText(counter).Trim(), out var parsed)
                ? parsed : 0;
        }
        catch (IOException)
        {
            // First drop: the counter does not exist yet.
        }
        File.WriteAllText(counter, (dropped + 1) + "\n");
    }

    // Write one capsule inside the byte cap; over-cap capsules are dropped and counted.
    // Returns the file path or null.
    internal static string? Spool(byte[] body, string operation, string dir, long maxBytes)
    {
        Directory.CreateDirectory(dir);
        long used = 0;
        foreach (var entry in Directory.GetFiles(dir, "*.json"))
        {
            try
            {
                used += new FileInfo(entry).Length;
            }
            catch (IOException)
            {
                // A concurrently removed entry counts as zero.
            }
        }
        if (used + body.Length > maxBytes)
        {
            Interlocked.Increment(ref _droppedCapsules);
            RecordDrop(dir);
            return null;
        }
        var digest = Convert.ToHexString(SHA256.HashData(body)).ToLowerInvariant()[..12];
        var file = Path.Combine(dir, "capsule-" + digest + ".json");
        File.WriteAllBytes(file, body);
        Interlocked.Increment(ref _spooledCapsules);
        Stderr.WriteLine(SpoolMarker + Json.Compact(new Dictionary<string, object?>
        {
            ["file"] = file,
            ["operation"] = operation,
        }));
        return file;
    }

    // The capsule names exactly one test. `dotnet test --filter FullyQualifiedName=...` is
    // the selection mechanism (xUnit v2 has no dynamic skip), so a wrapped body whose
    // identity is not the target simply does not run.
    private static string ReplayTarget()
    {
        var payload = Json.Parse(File.ReadAllText(ReplayPath()!))
            as Dictionary<string, object?>;
        var operation = payload?.GetValueOrDefault("operation") as string;
        if (operation == null || !operation.StartsWith(TestTriggerPrefix, StringComparison.Ordinal))
        {
            throw new InvalidOperationException(
                "REPROIT_REPLAY capsule does not carry a test trigger identity");
        }
        return operation;
    }

    private static void ReportResult(string operation, string status, Exception? error)
    {
        var detail = new Dictionary<string, object?>
        {
            ["operation"] = operation,
            ["status"] = status,
        };
        if (error != null) detail["failure"] = BoundedError(error);
        Stderr.WriteLine(ResultMarker + Json.Compact(detail));
    }

    private static async Task ReplayTestAsync(string suite, string test, Func<Task> body)
    {
        var target = ReplayTarget();
        var operation = OperationFor(suite, test);
        if (operation != target) return;
        try
        {
            await body().ConfigureAwait(false);
        }
        catch (Exception error)
        {
            ReportResult(operation, "failed", error);
            throw;
        }
        ReportResult(operation, "passed", null);
    }
}
