// Platform-agnostic core of the SDK. Holds the state-graph state machine, label
// reduction, batching and payload building. Has NO WPF / WinUI dependency so the
// behavior is unit-testable on any host; the Windows layer (ReproItClient) feeds
// it raw node data and supplies the wall clock + transport.
//
// Mirrors sdk/reproit-android/.../Engine.kt and sdk/reproit-web.js exactly: the
// edge/error state machine, the PII-safe context map (SetContext / SetContexts /
// Identify with the SHA-256 "uid" hash) and the {appId, sentAt, ctx?, events}
// batch envelope are byte-for-byte the same wire shape as the other SDKs and the
// cloud's ingest endpoint (POST /v1/events).

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Security.Cryptography;
using System.Text;

namespace ReproIt.Core
{
    /// <summary>The result of evaluating an app invariant: it either HELD or was
    /// VIOLATED with a message. Use <see cref="Ok"/> / <see cref="Fail"/> to build
    /// one. A predicate that throws is also a violation (message = the exception
    /// text), mirroring the web SDK's "throws => violated" contract.</summary>
    public sealed class InvariantResult
    {
        public bool Held { get; }
        public string Message { get; }

        private InvariantResult(bool held, string message)
        {
            Held = held;
            Message = message;
        }

        public static InvariantResult Ok()
        {
            return new InvariantResult(true, null);
        }

        public static InvariantResult Fail(string message = "")
        {
            return new InvariantResult(false, message ?? string.Empty);
        }
    }

    /// <summary>One observed accessibility node, as the platform layer reads it.</summary>
    public sealed class RawNode
    {
        /// <summary>AutomationProperties.Name ?? text, already non-null (empty if neither).</summary>
        public string Name { get; }

        /// <summary>True if the underlying element is invokable/clickable.</summary>
        public bool Tappable { get; }

        public RawNode(string name, bool tappable)
        {
            Name = name ?? string.Empty;
            Tappable = tappable;
        }
    }

    /// <summary>Reduced snapshot: a signature and capped unique labels.</summary>
    public sealed class Snapshot
    {
        public string Sig { get; }
        public List<string> Labels { get; }

        public Snapshot(string sig, List<string> labels)
        {
            Sig = sig;
            Labels = labels;
        }
    }

    /// <summary>One step in the graph trail kept for repros.</summary>
    public sealed class Step
    {
        public string Sig { get; }
        public string Action { get; }
        public string Label { get; }

        public Step(string sig, string action, string label = null)
        {
            Sig = sig;
            Action = action;
            Label = label;
        }

        public IDictionary<string, object> ToMap()
        {
            var m = new Dictionary<string, object>();
            m["sig"] = Sig;
            m["action"] = Action;
            if (!string.IsNullOrEmpty(Label))
            {
                m["label"] = Label;
            }
            return m;
        }
    }

    public sealed class Engine
    {
        private readonly ReproItConfig _cfg;
        private readonly Func<long> _now;
        private readonly Func<string, bool> _transport;
        private readonly Action<string> _log;

        private readonly List<IDictionary<string, object>> _queue = new List<IDictionary<string, object>>();
        private readonly List<Step> _path = new List<Step>();
        private readonly object _queueLock = new object();
        private readonly object _ctxLock = new object();
        private string _currentSig;
        private string _pendingAction;
        private string _pendingLabel;

        // PII-safe context dimensions sent with each batch (the "which users" answer).
        // Insertion-ordered and merged in place; included as the batch envelope's
        // "ctx" field (only when non-empty), exactly like the other SDKs.
        private readonly Dictionary<string, object> _context = new Dictionary<string, object>();

        // App-declared invariants, keyed by id (registration idempotent). Inert in
        // production; evaluated only under the fuzzer (see ReportInvariants).
        private readonly Dictionary<string, Func<InvariantResult>> _invariants =
            new Dictionary<string, Func<InvariantResult>>(StringComparer.Ordinal);

        public Engine(
            ReproItConfig cfg,
            Func<long> now = null,
            Func<string, bool> transport = null,
            Action<string> log = null)
        {
            _cfg = cfg;
            _now = now ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
            _transport = transport ?? (_ => true);
            _log = log;
        }

        public int QueueSize
        {
            get { lock (_queueLock) { return _queue.Count; } }
        }

        public string CurrentSignature()
        {
            return _currentSig;
        }

        /// <summary>Read-only snapshot of the current context dimensions (for tests / debug).</summary>
        public IDictionary<string, object> Context()
        {
            lock (_ctxLock) { return new Dictionary<string, object>(_context); }
        }

        /// <summary>Set a single PII-safe context dimension (e.g. role, plan, a count bucket).</summary>
        public void SetContext(string key, object value)
        {
            lock (_ctxLock) { _context[key] = value; }
        }

        /// <summary>Merge several context dimensions at once.</summary>
        public void SetContexts(IDictionary<string, object> values)
        {
            lock (_ctxLock)
            {
                foreach (var kv in values)
                {
                    _context[kv.Key] = kv.Value;
                }
            }
        }

        /// <summary>Attach a hashed user id (so the cloud can group "these N users hit
        /// it" without storing identity) plus optional context dimensions. The raw
        /// userId is never stored or sent; only a SHA-256 hex prefix is kept as "uid".</summary>
        public void Identify(string userId, IDictionary<string, object> context = null)
        {
            lock (_ctxLock)
            {
                _context["uid"] = HashUid(userId);
                if (context != null)
                {
                    foreach (var kv in context)
                    {
                        _context[kv.Key] = kv.Value;
                    }
                }
            }
        }

        private static string HashUid(string userId)
        {
            using (var sha = SHA256.Create())
            {
                byte[] digest = sha.ComputeHash(Encoding.UTF8.GetBytes(userId));
                var sb = new StringBuilder(16);
                // First 8 bytes -> 16 lowercase hex chars (matches the other SDKs' 16-char prefix).
                for (int i = 0; i < 8; i++)
                {
                    sb.Append(digest[i].ToString("x2", CultureInfo.InvariantCulture));
                }
                return sb.ToString();
            }
        }

        /// <summary>Accessible name reduction shared with the snapshot path. Trim, take
        /// the first line, drop empties and labels longer than MaxLabelLen.</summary>
        public string CleanLabel(string raw)
        {
            if (raw == null)
            {
                return null;
            }
            string trimmed = raw.Trim();
            int nl = trimmed.IndexOf('\n');
            string first = (nl >= 0 ? trimmed.Substring(0, nl) : trimmed).Trim();
            if (first.Length == 0 || first.Length > _cfg.MaxLabelLen)
            {
                return null;
            }
            return first;
        }

        /// <summary>Reduce a flat list of visible nodes (pre-order) plus the captured
        /// structural tree into a Snapshot. The signature is STRUCTURAL: the canonical
        /// descriptor of the tree prefixed by the screen anchor (route), byte-identical
        /// to the Rust oracle and the other SDKs (docs/signature.md). Localized text
        /// never enters the hash. The flat node list is used only for the display-only
        /// "labels" field; labels do NOT affect the signature.</summary>
        public Snapshot Reduce(IList<RawNode> nodes, Node tree, string anchor = null)
        {
            var seen = new List<string>();
            var seenSet = new HashSet<string>(StringComparer.Ordinal);
            foreach (var n in nodes)
            {
                string label = CleanLabel(n.Name);
                if (label == null)
                {
                    continue;
                }
                if (seenSet.Add(label))
                {
                    seen.Add(label);
                }
            }
            string sig = Signature.Of(anchor, tree);
            var capped = seen.Count > _cfg.MaxLabels ? seen.GetRange(0, _cfg.MaxLabels) : seen;
            return new Snapshot(sig, capped);
        }

        /// <summary>Record the action a tap implies; consumed by the next state change.</summary>
        public void NoteTap(string selector, string label)
        {
            _pendingAction = !string.IsNullOrEmpty(selector) ? "tap:" + selector : "tap:?";
            _pendingLabel = label;
        }

        /// <summary>Record an explicit navigation action.</summary>
        public void NoteNav()
        {
            _pendingAction = "nav";
            _pendingLabel = null;
        }

        /// <summary>Observe a reduced snapshot. If the signature changed (or this is the
        /// first observation), record an edge and advance the current state.</summary>
        /// <summary>Register an app invariant: a predicate the app declares that must
        /// hold in EVERY visited state. It returns <see cref="InvariantResult.Ok"/>
        /// when it holds, or <see cref="InvariantResult.Fail"/> (or throws) when it is
        /// violated. Under the fuzzer the SDK evaluates every registered invariant on
        /// each Observe and reports the failures for the runner to turn into
        /// `invariant` findings; in production the registry is INERT (evaluated only
        /// under the fuzzer), so it is zero-overhead until a run reproduces it.
        /// Registration is idempotent by id, so re-registering an id replaces it.</summary>
        public Engine Invariant(string id, Func<InvariantResult> predicate)
        {
            if (!string.IsNullOrEmpty(id) && predicate != null)
            {
                _invariants[id] = predicate;
            }
            return this;
        }

        public void Observe(Snapshot snap, string firstAction = "load")
        {
            // App-invariant oracle (SDK-self-triggered): under the fuzzer, evaluate
            // the app's registered predicates against this state and report failures
            // on stderr, which the UIA runner scrapes. No-op in production.
            ReportInvariants(snap.Sig);
            string cur = _currentSig;
            if (cur == null)
            {
                _currentSig = snap.Sig;
                EmitEdge(null, firstAction, snap, true);
                return;
            }
            if (snap.Sig == cur)
            {
                return;
            }
            string action = _pendingAction ?? "auto";
            string label = !_cfg.RedactLabels ? _pendingLabel : null;
            _pendingAction = null;
            _pendingLabel = null;
            EmitEdge(cur, action, snap, true, label);
            _currentSig = snap.Sig;
        }

        private void EmitEdge(string from, string action, Snapshot to, bool append, string label = null)
        {
            if (append)
            {
                _path.Add(new Step(from ?? string.Empty, action, label));
                if (_path.Count > _cfg.PathCap)
                {
                    _path.RemoveAt(0);
                }
            }
            var ev = new Dictionary<string, object>();
            ev["kind"] = "edge";
            if (from != null)
            {
                ev["from"] = from;
            }
            ev["action"] = action;
            if (!string.IsNullOrEmpty(label))
            {
                ev["label"] = label;
            }
            ev["to"] = to.Sig;
            if (!_cfg.RedactLabels)
            {
                ev["labels"] = to.Labels;
            }
            ev["t"] = _now();
            Enqueue(ev);
        }

        /// <summary>Evaluate one predicate. Returns null when it HELD, or a failure
        /// message string when it was VIOLATED (the returned message, or the thrown
        /// exception's text).</summary>
        private static string EvalInvariant(Func<InvariantResult> predicate)
        {
            try
            {
                var r = predicate();
                if (r == null || r.Held)
                {
                    return null;
                }
                return r.Message ?? string.Empty;
            }
            catch (Exception ex)
            {
                return string.IsNullOrEmpty(ex.Message) ? ex.GetType().Name : ex.Message;
            }
        }

        /// <summary>Evaluate every registered invariant and, ONLY under the fuzzer (the
        /// REPROIT_UNDER_FUZZER env var the UIA runner sets on the launched child, the
        /// fuzzer-detection gate), write one marker line
        ///   REPROIT_INVARIANT {"sig":"&lt;sig&gt;","items":[{"id","message"}...]}
        /// listing the VIOLATED invariants to stderr, which the runner scrapes and
        /// re-emits as EXPLORE:INVARIANT. Silent when the registry is empty or every
        /// invariant held (no empty-items line); inert in production (env var unset).</summary>
        private void ReportInvariants(string sig)
        {
            if (_invariants.Count == 0
                || string.IsNullOrEmpty(Environment.GetEnvironmentVariable("REPROIT_UNDER_FUZZER")))
            {
                return;
            }
            var items = new List<object>();
            foreach (var kv in _invariants)
            {
                string message = EvalInvariant(kv.Value);
                if (message != null)
                {
                    var it = new Dictionary<string, object>();
                    it["id"] = kv.Key;
                    it["message"] = message;
                    items.Add(it);
                }
            }
            if (items.Count == 0)
            {
                return;
            }
            var payload = new Dictionary<string, object>();
            payload["sig"] = sig ?? string.Empty;
            payload["items"] = items;
            Console.Error.WriteLine("REPROIT_INVARIANT " + Json.Encode(payload));
            Console.Error.Flush();
        }

        /// <summary>Record an error event carrying the current signature and graph path.
        /// stack is capped to 8 lines. Returns the event (useful for tests / for the
        /// caller to flush synchronously before a crash).</summary>
        public IDictionary<string, object> RecordError(
            string message,
            IList<string> stack,
            string source = "",
            int line = 0,
            IDictionary<string, object> context = null)
        {
            var ev = new Dictionary<string, object>();
            ev["kind"] = "error";
            // A genuine uncaught error IS the `crash` oracle firing; tag it so
            // the cloud can gate ingest on oracle-grade findings.
            ev["oracle"] = "crash";
            ev["sig"] = _currentSig ?? string.Empty;
            var pathOut = new List<object>();
            foreach (var s in _path)
            {
                pathOut.Add(s.ToMap());
            }
            // Include the in-flight action: a click whose handler throws
            // synchronously sets _pendingAction but crashes before its debounced
            // observe records it, so the bare path stops one step short of the bug.
            if (!string.IsNullOrEmpty(_pendingAction))
            {
                pathOut.Add(new Dictionary<string, object>
                {
                    ["sig"] = _currentSig ?? string.Empty,
                    ["action"] = _pendingAction,
                    ["label"] = !_cfg.RedactLabels ? _pendingLabel : null,
                });
            }
            ev["path"] = pathOut;
            ev["message"] = message;
            var cappedStack = new List<object>();
            int take = Math.Min(8, stack.Count);
            for (int i = 0; i < take; i++)
            {
                cappedStack.Add(stack[i]);
            }
            ev["stack"] = cappedStack;
            ev["source"] = source;
            ev["line"] = line;
            if (context != null && context.Count > 0)
            {
                ev["context"] = context;
            }
            ev["t"] = _now();
            Enqueue(ev);
            return ev;
        }

        private void Enqueue(IDictionary<string, object> ev)
        {
            try
            {
                _cfg.OnEvent?.Invoke(ev);
            }
            catch
            {
                // a faulty dev hook must never break telemetry.
            }
            if (_cfg.Endpoint == null)
            {
                if (_cfg.OnEvent == null)
                {
                    _log?.Invoke("reproit " + Json.Encode(ev));
                }
                return;
            }
            lock (_queueLock)
            {
                _queue.Add(ev);
            }
            if (QueueSize >= 50)
            {
                Flush();
            }
        }

        /// <summary>Build the batch body for the given queued events (without draining).</summary>
        public string BuildBatch(IList<IDictionary<string, object>> events)
        {
            var envelope = new Dictionary<string, object>();
            envelope["appId"] = _cfg.AppId;
            envelope["sentAt"] = _now();
            // Include "ctx" only when non-empty, placed BEFORE "events" to match the
            // other SDKs' envelope order.
            Dictionary<string, object> ctx;
            lock (_ctxLock)
            {
                ctx = _context.Count == 0 ? null : new Dictionary<string, object>(_context);
            }
            if (ctx != null)
            {
                envelope["ctx"] = ctx;
            }
            var evList = new List<object>();
            foreach (var e in events)
            {
                evList.Add(e);
            }
            envelope["events"] = evList;
            return Json.Encode(envelope);
        }

        /// <summary>Drain the queue and ship it via the transport. On failure the batch
        /// is re-queued ahead of newer events for one retry (mirrors the other SDKs).</summary>
        public void Flush()
        {
            if (_cfg.Endpoint == null)
            {
                lock (_queueLock) { _queue.Clear(); }
                return;
            }
            List<IDictionary<string, object>> batch;
            lock (_queueLock)
            {
                if (_queue.Count == 0)
                {
                    return;
                }
                batch = new List<IDictionary<string, object>>(_queue);
                _queue.Clear();
            }
            string body = BuildBatch(batch);
            bool ok;
            try
            {
                ok = _transport(body);
            }
            catch
            {
                ok = false;
            }
            if (!ok)
            {
                lock (_queueLock)
                {
                    _queue.InsertRange(0, batch);
                }
            }
        }
    }
}
