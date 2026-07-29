  function snapshot(cfg) {
    var root = document.body || document.documentElement;
    var tree = root ? domToNode(root, true) : { role: 'screen', children: [] };
    var anchor = anchorOf();
    var sig = signatureOf(anchor, tree);

    // display-only labels for `map --show` (never folded into the hash)
    var labels = [];
    var seen = {};
    var nodes = document.querySelectorAll('*');
    for (var i = 0; i < nodes.length; i++) {
      var el = nodes[i];
      if (!visible(el)) continue;
      var name = nameOf(el);
      if (name && name.length <= cfg.maxLabelLen && !seen[name]) {
        seen[name] = 1;
        labels.push(name);
      }
    }
    return { sig: sig, anchor: anchor, labels: labels.slice(0, cfg.maxLabels) };
  }

  // The production SDK is an ORACLE runner, not an error firehose: it reports
  // only findings we are confident about (zero/low false positive), so buckets in
  // the cloud stay high-signal. A genuine uncaught error IS the `crash` oracle and
  // is reported as such; but the environment/third-party noise every browser emits
  // through window.onerror is NOT the app crashing, carries no actionable info,
  // and is dropped AT THE SOURCE. Substring match on the lowercased message:
  var CRASH_NOISE = [
    'script error', // cross-origin, opaque: no stack, not ours
    'resizeobserver loop', // benign layout notification, not a crash
    'failed to fetch', // network flake, not a code defect
    'networkerror when attempting', // network flake
    'load failed', // network flake (Safari fetch)
    'aborterror', // a request the app itself aborted
    'the operation was aborted',
    'the user aborted a request',
  ];
  // Script URLs that are never the app's own code: browser extensions and
  // internal browser pages. An error sourced here is not a finding about the app.
  var NOISE_SOURCE = /^(chrome|moz|safari-web|webkit-masked)-extension:|^chrome:\/\//i;
  // True when an uncaught error is environment/third-party noise rather than the
  // app crashing, so the SDK drops it instead of shipping a low-signal bucket.
  function isCrashNoise(message, source) {
    var m = String(message == null ? '' : message)
      .toLowerCase()
      .trim();
    if (!m || m === 'script error.' || m === 'script error') return true;
    for (var i = 0; i < CRASH_NOISE.length; i++) {
      if (m.indexOf(CRASH_NOISE[i]) !== -1) return true;
    }
    if (source && NOISE_SOURCE.test(String(source))) return true;
    return false;
  }

  // Must match the CLI's structural message normalization. Volatile numeric
  // values and quoted user-facing labels cannot split one defect into multiple
  // identities or leak into the production join key.
  function structuralMessage(message) {
    var input = String(message == null ? '' : message);
    var out = '';
    for (var i = 0; i < input.length; i++) {
      var c = input[i];
      if (c === '"' || c === "'") {
        var quote = c;
        while (i + 1 < input.length && input[i + 1] !== quote) i++;
        if (i + 1 < input.length) i++;
        out += '<q>';
      } else if (c >= '0' && c <= '9') {
        out += '#';
        while (i + 1 < input.length && /[0-9.,]/.test(input[i + 1])) i++;
      } else {
        out += c;
      }
    }
    return out;
  }

  function crashIdentity(message) {
    return {
      oracle: 'crash',
      invariant: 'no-exception',
      kind: 'exception',
      message: structuralMessage(message),
      frame: '',
      trigger: '',
    };
  }

  function protocolBatch(appId, events, context, sentAt, batchSequence) {
    var batchId = 'sdk-' + sentAt + '-' + batchSequence;
    var frames = events.map(function (event, index) {
      var protocolEvent;
      if (event.kind === 'edge') {
        protocolEvent = {
          kind: 'graph-edge',
          from: event.from || '∅',
          action: event.action || 'auto',
          to: event.to || '?',
        };
      } else if (event.kind === 'error') {
        var identity = event.findingIdentity || crashIdentity(event.message);
        var findingContext = Object.assign({}, context || {}, event.context || {});
        protocolEvent = {
          kind: 'finding',
          signature: event.sig || '?',
          message: event.message || '',
          identity: identity,
          path: (event.path || []).map(function (step) {
            return {
              signature: step.sig || '?',
              action: step.action || 'auto',
              label: step.label || null,
            };
          }),
          context: findingContext,
        };
      } else {
        protocolEvent = { kind: 'stream-defect', reason: 'invalid-event' };
      }
      return {
        runId: batchId,
        sequence: index + 1,
        scope: { domain: 'shared' },
        event: protocolEvent,
      };
    });
    var batch = {
      version: 1,
      batchId: batchId,
      appId: appId,
      frames: frames,
      evidence: [],
    };
    if (context && context.build) batch.deployment = context.build;
    return batch;
  }

  // ---- the SDK ------------------------------------------------------------
  var ReproIt = {
    _cfg: null,
    _buf: [],
    _cur: null, // current state signature
    _path: [], // [{sig, action, label?}] graph trail for repros
    _pending: null, // last interaction's {action,label?}, awaiting a snapshot
    _timer: null,
    _on: false,
    _build: null, // developer-provided { version, commit } or null
    _batchSequence: 0,
    init: function (opts) {
      if (this._on) return this;
      var cfg = Object.assign({}, DEFAULTS, opts || {});
      // Automation-driven sessions (Playwright/Selenium, including reproit's own
      // replays) never feed production telemetry: a replayed crash would re-count
      // the very bucket it reproduces. Test rigs opt in via reportAutomation.
      if (navigator.webdriver && !cfg.reportAutomation) return this;
      // session sampling: report only a fraction of sessions
      if (Math.random() >= cfg.sampleRate) return this;
      this._cfg = cfg;
      this._on = true;
      // Developer-provided build identity, stamped under context.build so the
      // cloud can segment bugs by build (regressed in / resolved since). Only the
      // provided fields ride; null (omitted) when no build was supplied.
      this._build = normalizeBuild(cfg.build);
      // Layer-3 opt-in value-node selectors (docs/signature.md "Value-state").
      setValueNodeSelectors(cfg.valueNodes);

      var self = this;
      // 1. observe an initial state once the DOM settles
      this._settle(function () {
        self._observe('load');
      });

      // 2. navigations (SPA + classic)
      this._wrapHistory();
      // Navigation can be caused by the click we just captured. Preserve that
      // structural click instead of replacing it with a generic `nav`, or the
      // cloud replay loses the control that opened the destination screen.
      var observeNavigation = function () {
        self._settle(function () {
          self._observe(self._pending || 'nav');
        });
      };
      addEventListener('popstate', observeNavigation);
      addEventListener('hashchange', observeNavigation);

      // 3. interactions -> remember structural action + display label, then re-snapshot
      addEventListener(
        'click',
        function (e) {
          var t = e.target;
          while (t && t !== document && !interactive(t)) t = t.parentElement;
          var label = t && t !== document ? nameOf(t) || '' : '';
          var sel = t && t !== document ? actionSelectorOf(t) : null;
          self._pending = { action: sel ? 'tap:' + sel : 'tap:?', label: label || undefined };
          self._settle(function () {
            self._observe(self._pending);
          });
        },
        true,
      );

      // 4. crash oracle: a genuine uncaught error is the `crash` oracle firing,
      //    tagged so, carrying the graph PATH to it (the seed of a deterministic
      //    repro). Environment/third-party noise is dropped so only oracle-grade
      //    findings ship. General (non-oracle) error capture is a future opt-in.
      addEventListener('error', function (e) {
        var message = e.message || String(e);
        if (isCrashNoise(message, e.filename)) return;
        self._emit({
          kind: 'error',
          oracle: 'crash',
          findingIdentity: crashIdentity(message),
          sig: self._cur,
          path: self._errorPath(),
          message: message,
          stack:
            e.error && e.error.stack ? String(e.error.stack).split('\n').slice(0, 8) : undefined,
          source: e.filename,
          line: e.lineno,
          context: self._errorContext(),
        });
      });
      addEventListener('unhandledrejection', function (e) {
        var r = e.reason || {};
        var reason = r.message || String(r);
        if (isCrashNoise(reason, undefined)) return;
        self._emit({
          kind: 'error',
          oracle: 'crash',
          findingIdentity: crashIdentity('unhandledrejection: ' + reason),
          sig: self._cur,
          path: self._errorPath(),
          message: 'unhandledrejection: ' + reason,
          stack: r.stack ? String(r.stack).split('\n').slice(0, 8) : undefined,
          context: self._errorContext(),
        });
      });

      // Optional debug-build capture gesture. It is off by default so a shipped
      // production app never turns an arbitrary user key chord into a finding.
      if (cfg.testerCaptureShortcut) {
        addEventListener(
          'keydown',
          function (e) {
            if (e.altKey && e.shiftKey && String(e.key).toLowerCase() === 'b') {
              if (e.preventDefault) e.preventDefault();
              self.captureBug();
            }
          },
          true,
        );
      }

      // 5. flush on a timer and when the page goes away
      this._timer = setInterval(function () {
        self._flush();
      }, cfg.flushMs);
      addEventListener('pagehide', function () {
        self._flush(true);
      });
      addEventListener('visibilitychange', function () {
        if (document.visibilityState === 'hidden') self._flush(true);
      });
      return this;
    },

    // Zero-config start: the one-line quickstart. Begins telemetry with sensible
    // defaults and no required options, deriving appId from the page host when
    // one is not supplied, then delegating to init (which stays the full,
    // explicit entry point). ReproIt.start() is the copy-paste one-liner. A web page
    // has no build-mode distinction, so start() is active wherever it is loaded;
    // the existing webdriver/reportAutomation guard still keeps test-rig sessions
    // out of production telemetry. Pass any init option to override a default
    // (e.g. ReproIt.start({ endpoint, key })).
    start: function (opts) {
      var o = opts || {};
      if (o.appId == null) {
        var host = '';
        try {
          if (typeof location !== 'undefined' && location.hostname) host = location.hostname;
        } catch (e) {}
        o = Object.assign({}, o, { appId: host || 'app' });
      }
      return this.init(o);
    },

    // Register an app invariant: a predicate the app declares that must hold in
    // EVERY visited state (a running total never negative, the selected tab
    // always highlighted). `test` returns truthy when it holds, or falsy /
    // throws / an { ok:false, message } object when it is violated. reproit's
    // fuzzer evaluates every registered invariant on each state-settle and
    // reports the failures as `invariant` findings; in production the registry
    // is inert (a plain array push, no evaluation), so this is zero-overhead
    // until a run reproduces it. Registration is idempotent by id, so a hot
    // reload re-registering the same id replaces rather than duplicates it.
    // Stored on a stable global (window.__reproit_invariants) so reproit reads
    // it without coupling to this SDK's internals.
    invariant: function (id, test) {
      if (typeof id !== 'string' || typeof test !== 'function') return this;
      if (typeof window === 'undefined') return this;
      var reg = window.__reproit_invariants || (window.__reproit_invariants = []);
      for (var i = 0; i < reg.length; i++) {
        if (reg[i].id === id) {
          reg[i].test = test;
          return this;
        }
      }
      reg.push({ id: id, test: test });
      return this;
    },

    // Mark the current structural state as a tester-observed bug. This is not a
    // confirmed finding yet: Cloud keeps it in pending captures until the CLI
    // replays the path and reaches this exact state on a clean launch.
    captureBug: function () {
      if (!this._on || !this._cfg) return false;
      var snap;
      try {
        snap = snapshot(this._cfg);
      } catch (e) {
        return false;
      }
      this._cur = snap.sig;
      var path = this._errorPath();
      var last = path.length ? path[path.length - 1].action : 'load';
      this._emit({
        kind: 'error',
        oracle: 'tester-capture',
        findingIdentity: {
          oracle: 'tester-capture',
          invariant: 'tester-observed-failure',
          kind: 'structural-state',
          message: '',
          frame: '',
          trigger: last,
          boundary: snap.sig,
        },
        sig: snap.sig,
        path: path,
        message: 'Tester marked this structural state as incorrect',
        context: this._errorContext(),
      });
      this._flush();
      return true;
    },

    _settle: function (fn) {
      clearTimeout(this._settleT);
      this._settleT = setTimeout(fn, this._cfg.debounceMs);
    },

    _wrapHistory: function () {
      var self = this;
      ['pushState', 'replaceState'].forEach(function (m) {
        var orig = history[m];
        history[m] = function () {
          var r = orig.apply(this, arguments);
          self._settle(function () {
            self._observe(self._pending || 'nav');
          });
          return r;
        };
      });
    },

    // Observe the current screen; if its signature changed, record the edge.
    _observe: function (step) {
      if (!this._on) return;
      var snap = snapshot(this._cfg);
      if (snap.sig === this._cur) {
        // No structural change, but a same-sig INTERACTION still belongs in the
        // path: dropping it breaks replay fidelity when the tap mutates state
        // the signature ignores (e.g. "add to cart" only bumps a counter, yet
        // the later crash needs that item in the cart). Recorded as a self-loop
        // path step only; no edge event (the map has nothing new to learn).
        if (step && typeof step === 'object' && step.action) {
          var selfStep = { sig: snap.sig, action: step.action };
          if (!this._cfg.redactLabels && step.label) selfStep.label = step.label;
          this._path.push(selfStep);
          if (this._path.length > this._cfg.pathCap) this._path.shift();
          this._pending = null;
        }
        return;
      }
      var action = step && typeof step === 'object' ? step.action : step;
      var label = step && typeof step === 'object' ? step.label : undefined;
      var from = this._cur;
      this._cur = snap.sig;
      var pathStep = { sig: snap.sig, action: action };
      if (!this._cfg.redactLabels && label) pathStep.label = label;
      this._path.push(pathStep);
      if (this._path.length > this._cfg.pathCap) this._path.shift();
      var ev = {
        kind: 'edge',
        from: from,
        action: action || 'auto',
        to: snap.sig,
        labels: this._cfg.redactLabels ? undefined : snap.labels,
      };
      if (!this._cfg.redactLabels && label) ev.label = label;
      this._emit(ev);
      this._pending = null;
    },

    // The action path to an error, INCLUDING the in-flight action. A click that
    // throws synchronously (the crashing tap) sets `_pending` but crashes before
    // its debounced `_observe` records it, so the bare path stops one step short
    // of the bug. Append the pending action so the captured repro contains the
    // step that actually triggers the crash -- otherwise a replay reaches the
    // screen but never fires it.
    _errorPath: function () {
      var path = this._path.slice();
      if (this._pending) {
        // A user can click before the debounced initial observation runs. In
        // that case `_cur` is still null even though the DOM already has a
        // perfectly usable structural signature. Capture it synchronously so
        // Cloud never has to discard the crash-triggering action.
        var sig = this._cur;
        if (!sig && this._cfg) {
          try {
            sig = snapshot(this._cfg).sig;
            this._cur = sig;
          } catch (e) {}
        }
        var step = { sig: sig || '?', action: this._pending.action };
        if (!this._cfg.redactLabels && this._pending.label) step.label = this._pending.label;
        path.push(step);
      }
      return path;
    },

    // On-error context. Tier-3 input fingerprints ride here under
    // `context.fingerprint` (PII-safe FEATURES of on-screen fields, never the
    // raw values). Best-effort: failure to read the DOM never breaks reporting.
    _errorContext: function () {
      try {
        var fp = collectFieldFingerprints();
        if (fp.length) return { fingerprint: fp, fpVersion: FP_VERSION };
      } catch (e) {}
      return undefined;
    },

    _emit: function (ev) {
      ev.t = Date.now();
      if (this._cfg.onEvent) {
        try {
          this._cfg.onEvent(ev);
        } catch (e) {}
      }
      this._buf.push(ev);
      if (this._buf.length >= 50) this._flush();
    },

    _flush: function (useBeacon) {
      if (!this._buf.length) return;
      var context = environmentContext();
      if (
        this._cfg.context &&
        typeof this._cfg.context === 'object' &&
        !Array.isArray(this._cfg.context)
      ) {
        context = Object.assign(context, this._cfg.context);
      }
      // Stamp the developer-provided build identity as context.build (only the
      // provided fields); omitted entirely when no build was supplied.
      if (this._build) context.build = this._build;
      this._batchSequence += 1;
      var batch = protocolBatch(
        this._cfg.appId,
        this._buf,
        context,
        Date.now(),
        this._batchSequence,
      );
      this._buf = [];
      var cfg = this._cfg;
      if (!cfg.endpoint) {
        if (!cfg.onEvent && typeof console !== 'undefined') console.debug('[reproit]', batch);
        return;
      }
      var body = JSON.stringify(batch);
      // sendBeacon cannot carry an Authorization header, so a keyed config always
      // posts via fetch; `keepalive: true` gives it beacon-like unload survival.
      if (useBeacon && navigator.sendBeacon && !cfg.key) {
        navigator.sendBeacon(cfg.endpoint, body);
      } else {
        var headers = { 'Content-Type': 'application/json' };
        if (cfg.key) headers['Authorization'] = 'Bearer ' + cfg.key;
        fetch(cfg.endpoint, {
          method: 'POST',
          headers: headers,
          body: body,
          keepalive: true,
        }).catch(function () {});
      }
    },
  };

  // Expose the pure fingerprint helpers (load-bearing, host-testable).
  ReproIt.fingerprintValue = fingerprintValue;
  ReproIt.collectFieldFingerprints = collectFieldFingerprints;
  ReproIt.FP_VERSION = FP_VERSION;
  // The production error gate (host-testable): true for environment/third-party
  // noise the SDK must NOT report, so the crash oracle stays zero/low-FP.
  ReproIt.isCrashNoise = isCrashNoise;
  ReproIt.structuralMessage = structuralMessage;
  ReproIt.environmentContext = environmentContext;

  // Expose the CANONICAL signature core (load-bearing, parity-tested against
  // signature_vectors.json + the Rust oracle). signatureOf/descriptorOf take a
  // canonical Node tree; domToNode builds one from a live DOM root.
  ReproIt.signatureOf = signatureOf;
  ReproIt.descriptorOf = descriptorOf;
  ReproIt.domToNode = domToNode;
  ReproIt.anchorOf = anchorOf;
  // Layer-2 value-class bucketer + Layer-3 opt-in selector installer (load-
  // bearing, parity-tested against the oracle's value_class / V: section).
  ReproIt.valueClass = valueClass;
  ReproIt.setValueNodeSelectors = setValueNodeSelectors;
  // Developer-provided build identity normalizer (load-bearing, host-testable):
  // keeps only the provided {version, commit} string fields, else null.
  ReproIt.normalizeBuild = normalizeBuild;
  ReproIt.protocolBatch = protocolBatch;
  ReproIt._actionKeyOf = actionKeyOf;

  global.ReproIt = ReproIt;
  if (typeof module !== 'undefined' && module.exports) module.exports = ReproIt;
})(typeof window !== 'undefined' ? window : this);
