// Source-neutral, bounded causal recorder shared by semantic .NET adapters.
// It records evidence facts only and cannot grant process execution authority.

using System.Text.Json;
using System.Text.RegularExpressions;

namespace ReproitBackend;

public sealed class UniversalRecorderOptions
{
    public required string ProjectId { get; init; }
    public required string EmitterId { get; init; }
    public required string Component { get; init; }
    public string EmitterKind { get; init; } = "runtime-sdk";
    public string? Runtime { get; init; } = "dotnet";
    public string? BatchId { get; init; }
    public string? SessionId { get; init; }
    public string? BuildVersion { get; init; }
    public string? BuildCommit { get; init; }
    public string Consent { get; init; } = "application-telemetry";
    public string RetentionClass { get; init; } = "standard";
    public IReadOnlyList<Dictionary<string, object?>> Capabilities { get; init; } =
        Array.Empty<Dictionary<string, object?>>();
    public int MaxEvents { get; init; } = 1024;
    public int MaxArtifacts { get; init; } = 32;
}

public sealed class UniversalEventContext
{
    public ulong MonotonicNs { get; init; }
    public ulong? ProcessId { get; init; }
    public ulong? ThreadId { get; init; }
    public string? Actor { get; init; }
    public string? TraceId { get; init; }
    public string? SpanId { get; init; }
    public IReadOnlyList<string> CausalParentIds { get; init; } = Array.Empty<string>();
}

public static class CaptureValues
{
    public static Dictionary<string, object?> Structural(object? shape) => new()
    {
        ["representation"] = "structural",
        ["shape"] = BoundedJson(shape),
    };

    public static Dictionary<string, object?> Replayable(
        object? value,
        string redaction = "redacted-at-source")
    {
        if (redaction is not ("not-required" or
            "redacted-at-source" or "redacted-before-storage"))
        {
            throw new ArgumentException("replayable values must be safe before capture");
        }
        return new Dictionary<string, object?>
        {
            ["representation"] = "replayable",
            ["value"] = BoundedJson(value),
            ["redaction"] = redaction,
        };
    }

    public static Dictionary<string, object?> EnvironmentBound(string reference) => new()
    {
        ["representation"] = "environment-bound",
        ["reference"] = UniversalRecorder.BoundedText(reference, nameof(reference)),
    };

    private static object? BoundedJson(object? value)
    {
        var encoded = JsonSerializer.SerializeToUtf8Bytes(value);
        if (encoded.Length > 64 * 1024)
        {
            throw new ArgumentException("captured value exceeds 64 KiB");
        }
        return Json.Parse(System.Text.Encoding.UTF8.GetString(encoded));
    }
}

public sealed class UniversalRecorder
{
    public const int ProtocolVersion = 1;
    public const int MaximumEvents = 5000;
    public const int MaximumArtifacts = 256;

    private static readonly Regex TokenPattern =
        new("^[A-Za-z0-9._:-]{1,128}$", RegexOptions.Compiled);

    private readonly UniversalRecorderOptions _options;
    private readonly string _batchId;
    private readonly string _sessionId;
    private readonly List<Dictionary<string, object?>> _events = new();
    private readonly List<Dictionary<string, object?>> _artifacts = new();
    private readonly HashSet<string> _artifactIds = new();
    private readonly HashSet<string> _droppedEventIds = new();
    private ulong _sequence = 1;
    private ulong _lastMonotonicNs;
    private long _droppedEvents;
    private long _droppedArtifacts;
    private bool _finished;

    public UniversalRecorder(UniversalRecorderOptions options)
    {
        if (options.MaxEvents is < 2 or > MaximumEvents)
        {
            throw new ArgumentOutOfRangeException(nameof(options.MaxEvents));
        }
        if (options.MaxArtifacts is < 1 or > MaximumArtifacts)
        {
            throw new ArgumentOutOfRangeException(nameof(options.MaxArtifacts));
        }
        ProtocolToken(options.ProjectId, nameof(options.ProjectId));
        ProtocolToken(options.EmitterId, nameof(options.EmitterId));
        ProtocolToken(options.Component, nameof(options.Component));
        ProtocolToken(options.EmitterKind, nameof(options.EmitterKind));
        if (options.Runtime != null) ProtocolToken(options.Runtime, nameof(options.Runtime));
        _options = options;
        _batchId = options.BatchId == null
            ? RandomId("cb")
            : ProtocolToken(options.BatchId, nameof(options.BatchId));
        _sessionId = options.SessionId == null
            ? RandomId("session")
            : CorrelationToken(options.SessionId, nameof(options.SessionId));
    }

    public string? OperationStart(string name, UniversalEventContext? context = null) =>
        Record(new() { ["kind"] = "operation-start", ["name"] = BoundedText(name, "name") },
            context);

    public string? OperationEnd(
        string name,
        string outcome,
        UniversalEventContext? context = null) =>
        Record(new()
        {
            ["kind"] = "operation-end",
            ["name"] = BoundedText(name, "name"),
            ["outcome"] = ProtocolToken(outcome, "outcome"),
        }, context);

    public string? Trigger(
        string trigger,
        string subject,
        Dictionary<string, object?>? value = null,
        UniversalEventContext? context = null)
    {
        var evt = new Dictionary<string, object?>
        {
            ["kind"] = "trigger",
            ["trigger"] = ProtocolToken(trigger, "trigger"),
            ["subject"] = BoundedText(subject, "subject"),
        };
        if (value != null) evt["value"] = value;
        return Record(evt, context);
    }

    public string? State(
        string state,
        string operation,
        string subject,
        Dictionary<string, object?>? value = null,
        UniversalEventContext? context = null)
    {
        var evt = new Dictionary<string, object?>
        {
            ["kind"] = "state-access",
            ["state"] = ProtocolToken(state, "state"),
            ["operation"] = ProtocolToken(operation, "operation"),
            ["subject"] = BoundedText(subject, "subject"),
        };
        if (value != null) evt["value"] = value;
        return Record(evt, context);
    }

    public string? Dependency(
        string system,
        string operation,
        string subject,
        Dictionary<string, object?>? value = null,
        UniversalEventContext? context = null)
    {
        var evt = new Dictionary<string, object?>
        {
            ["kind"] = "dependency",
            ["system"] = ProtocolToken(system, "system"),
            ["operation"] = ProtocolToken(operation, "operation"),
            ["subject"] = BoundedText(subject, "subject"),
        };
        if (value != null) evt["value"] = value;
        return Record(evt, context);
    }

    public string? Effect(
        string effect,
        string subject,
        Dictionary<string, object?>? value = null,
        UniversalEventContext? context = null)
    {
        var evt = new Dictionary<string, object?>
        {
            ["kind"] = "effect",
            ["effect"] = ProtocolToken(effect, "effect"),
            ["subject"] = BoundedText(subject, "subject"),
        };
        if (value != null) evt["value"] = value;
        return Record(evt, context);
    }

    public string? Checkpoint(
        string name,
        Dictionary<string, object?> attributes,
        UniversalEventContext? context = null) =>
        Record(new()
        {
            ["kind"] = "checkpoint",
            ["name"] = BoundedText(name, "name"),
            ["attributes"] = attributes,
        }, context);

    public string? Failure(
        string observation,
        string summary,
        string? signature = null,
        string authority = "runtime-diagnosis",
        string? observationPoint = null,
        IReadOnlyList<string>? artifactIds = null,
        UniversalEventContext? context = null)
    {
        var failure = new Dictionary<string, object?>
        {
            ["observation"] = ProtocolToken(observation, "observation"),
            ["authority"] = ProtocolToken(authority, "authority"),
            ["summary"] = BoundedText(summary, "summary"),
            ["artifactIds"] = artifactIds?.ToList() ?? new List<string>(),
        };
        if (signature != null) failure["signature"] = BoundedText(signature, "signature");
        if (observationPoint != null)
        {
            failure["observationPoint"] = BoundedText(observationPoint, "observationPoint");
        }
        return Record(new() { ["kind"] = "observation", ["failure"] = failure }, context);
    }

    public bool AddArtifact(Dictionary<string, object?> artifact)
    {
        if (_finished) return false;
        try
        {
            var id = artifact["id"] as string;
            if (id == null || !Regex.IsMatch(id, "^sha256:[a-f0-9]{64}$")) return false;
            if (!_artifactIds.Add(id)) return false;
            if (_artifacts.Count == _options.MaxArtifacts)
            {
                _artifactIds.Remove(id);
                _droppedArtifacts++;
                return false;
            }
            _artifacts.Add(artifact);
            return true;
        }
        catch
        {
            return false;
        }
    }

    public Dictionary<string, object?>? Finish()
    {
        if (_finished) return null;
        _finished = true;
        RemoveDroppedParents();
        if (_droppedEvents > 0 || _droppedArtifacts > 0)
        {
            if (_events.Count == _options.MaxEvents)
            {
                DropOldest();
                RemoveDroppedParents();
            }
            _events.Add(new Dictionary<string, object?>
            {
                ["id"] = "evt_" + _options.EmitterId + "_" + _sequence,
                ["sequence"] = (long)_sequence,
                ["monotonicNs"] = (long)(_lastMonotonicNs + 1),
                ["causalParentIds"] = new List<string>(),
                ["event"] = new Dictionary<string, object?>
                {
                    ["kind"] = "defect",
                    ["defect"] = "dropped",
                    ["detail"] = _droppedEvents + " event(s) and " +
                        _droppedArtifacts + " artifact(s) exceeded recorder bounds",
                },
            });
        }
        var emitter = new Dictionary<string, object?>
        {
            ["id"] = _options.EmitterId,
            ["kind"] = _options.EmitterKind,
            ["component"] = _options.Component,
        };
        if (_options.Runtime != null) emitter["runtime"] = _options.Runtime;
        var batch = new Dictionary<string, object?>
        {
            ["version"] = (long)ProtocolVersion,
            ["batchId"] = _batchId,
            ["projectId"] = _options.ProjectId,
            ["sessionId"] = _sessionId,
            ["emitter"] = emitter,
            ["observedAt"] = DateTimeOffset.UtcNow.ToString("O"),
            ["policy"] = new Dictionary<string, object?>
            {
                ["consent"] = _options.Consent,
                ["retentionClass"] = _options.RetentionClass,
            },
            ["capabilities"] = _options.Capabilities,
            ["events"] = _events,
            ["artifacts"] = _artifacts,
        };
        if (_options.BuildVersion != null || _options.BuildCommit != null)
        {
            var deployment = new Dictionary<string, object?>();
            if (_options.BuildVersion != null) deployment["version"] = _options.BuildVersion;
            if (_options.BuildCommit != null) deployment["commit"] = _options.BuildCommit;
            batch["deployment"] = deployment;
        }
        return batch;
    }

    private string? Record(
        Dictionary<string, object?> evt,
        UniversalEventContext? context)
    {
        if (_finished) return null;
        try
        {
            context ??= new UniversalEventContext();
            var sequence = _sequence++;
            var monotonicNs = context.MonotonicNs == 0 ? sequence : context.MonotonicNs;
            _lastMonotonicNs = Math.Max(_lastMonotonicNs, monotonicNs);
            var id = "evt_" + _options.EmitterId + "_" + sequence;
            var parents = context.CausalParentIds.Take(32)
                .Select(parent => ProtocolToken(parent, "causal parent"))
                .ToList();
            var captured = new Dictionary<string, object?>
            {
                ["id"] = id,
                ["sequence"] = (long)sequence,
                ["monotonicNs"] = (long)monotonicNs,
                ["causalParentIds"] = parents,
                ["event"] = evt,
            };
            AddOptional(captured, "actor", context.Actor);
            AddOptional(captured, "traceId", context.TraceId);
            AddOptional(captured, "spanId", context.SpanId);
            if (context.ProcessId != null) captured["processId"] = (long)context.ProcessId.Value;
            if (context.ThreadId != null) captured["threadId"] = (long)context.ThreadId.Value;
            if (_events.Count == _options.MaxEvents) DropOldest();
            _events.Add(captured);
            return id;
        }
        catch
        {
            return null;
        }
    }

    private void DropOldest()
    {
        if (_events.Count == 0) return;
        _droppedEventIds.Add((string)_events[0]["id"]!);
        _events.RemoveAt(0);
        _droppedEvents++;
    }

    private void RemoveDroppedParents()
    {
        foreach (var evt in _events)
        {
            var parents = (List<string>)evt["causalParentIds"]!;
            parents.RemoveAll(_droppedEventIds.Contains);
        }
    }

    private static void AddOptional(
        Dictionary<string, object?> target,
        string name,
        string? value)
    {
        if (value == null) return;
        target[name] = CorrelationToken(value, name);
    }

    private static string CorrelationToken(string value, string name)
    {
        if (value.Length == 0) throw new ArgumentException(name + " must not be empty");
        if (TokenPattern.IsMatch(value)) return value;
        var digest = System.Security.Cryptography.SHA256.HashData(
            System.Text.Encoding.UTF8.GetBytes(value));
        return name.ToLowerInvariant() + ":" +
            Convert.ToHexString(digest)[..32].ToLowerInvariant();
    }

    internal static string ProtocolToken(string value, string name)
    {
        if (!TokenPattern.IsMatch(value))
        {
            throw new ArgumentException(name + " must be a bounded protocol token");
        }
        return value;
    }

    internal static string BoundedText(string value, string name)
    {
        if (string.IsNullOrEmpty(value) ||
            value.Contains('\0') ||
            System.Text.Encoding.UTF8.GetByteCount(value) > 16 * 1024)
        {
            throw new ArgumentException(name + " must be non-empty bounded text");
        }
        return value;
    }

    private static string RandomId(string prefix) =>
        prefix + "_" + Convert.ToHexString(
            System.Security.Cryptography.RandomNumberGenerator.GetBytes(8)).ToLowerInvariant();
}
