/**
 * reproit-react-native, production telemetry SDK for React Native.
 *
 * Emits the SAME state-graph + error events from real users that the reproit
 * test runners emit, so the production graph aligns 1:1 with test-time graphs.
 * When a user hits an error, the event carries the graph PATH that produced it,
 * which the reproit cloud turns into a deterministic replay: a prod "cannot
 * reproduce" becomes a reproducible test.
 *
 * It mirrors the web SDK (`sdk/reproit-web.js`) and Flutter SDK
 * (`sdk/reproit_flutter`): same FNV-1a state signature, same event shapes,
 * same `{appId, sentAt, events}` batch POSTed to `<endpoint>/v1/events`, so
 * web, Flutter and RN telemetry land in one cloud graph.
 *
 * Usage (one init call in your app entry):
 *
 *   import { ReproIt } from 'reproit-react-native';
 *   ReproIt.init({ appId: 'example', endpoint: 'https://ingest.reproit.example',
 *                  apiKey: 'sk_...' });
 *
 * Optionally wrap your tree to capture taps + label nav transitions:
 *
 *   <ReproItProvider navigationRef={navRef}>
 *     <App />
 *   </ReproItProvider>
 */
import { autoContext, hashUid, type Context, type ContextValue } from './context';
import {
  collectFields,
  setAnchor,
  setValueNodeSelectors,
  snapshot,
  snapshotFromTree,
  type Snapshot,
} from './snapshot';
import type { Node } from './signature';
import { fingerprintFields, FP_VERSION } from './fingerprint';
import {
  DEFAULTS,
  type Batch,
  type EdgeEvent,
  type ErrorContext,
  type ErrorEvent,
  type PathStep,
  type ReproItConfig,
  type ReproItEvent,
  type ResolvedConfig,
} from './types';

export type {
  ReproItConfig,
  ReproItEvent,
  EdgeEvent,
  ErrorEvent,
  PathStep,
  Batch,
} from './types';
export { signatureOf, descriptorOf, valueClass, type Node } from './signature';
export { setValueNodeSelectors } from './snapshot';
export { ReproItProvider } from './provider';
export type { Context, ContextValue } from './context';
export { fingerprintValue, fingerprintFields, FP_VERSION } from './fingerprint';
export type { ValueFingerprint, FieldFingerprint } from './fingerprint';
export type { ErrorContext } from './types';

function resolveConfig(opts: ReproItConfig): ResolvedConfig {
  return {
    ...DEFAULTS,
    ...opts,
    endpoint: opts.endpoint ?? null,
    apiKey: opts.apiKey ?? null,
    onEvent: opts.onEvent ?? null,
  };
}

/** The telemetry singleton. */
class ReproItImpl {
  private cfg: ResolvedConfig | null = null;
  private on = false;
  private buf: ReproItEvent[] = [];
  private path: PathStep[] = [];
  // PII-safe context dimensions sent with each batch (the "which users" answer).
  private ctx: Context = {};
  private cur: string | null = null;
  private pendingAction: string | null = null;
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private priorGlobalHandler: ((e: unknown, isFatal?: boolean) => void) | null = null;

  /** Initialize telemetry. Safe to call once; later calls are ignored. */
  init(opts: ReproItConfig): ReproItImpl {
    if (this.on) return this;
    if (!opts || !opts.appId) {
      throw new Error('ReproIt.init: { appId } is required');
    }
    const cfg = resolveConfig(opts);
    // Session sampling: report only a fraction of sessions, decided once.
    if (cfg.sampleRate < 1.0 && Math.random() >= cfg.sampleRate) return this;
    this.cfg = cfg;
    this.on = true;

    // Layer-3 opt-in value-node selectors (docs/signature.md "Value-state"):
    // mark EXTRA nodes value-bearing even when their role is not a value-role.
    setValueNodeSelectors(cfg.valueNodes);

    // Tier-1 auto dimensions: zero-PII, high-signal for "works for me but not
    // for them" bugs (platform, OS version, locale, timezone, release flag).
    this.ctx = autoContext();

    this.installErrorHook();

    // First snapshot once the first frames have rendered + settled.
    this.settle(() => this.observe('load'));

    // Flush on a timer.
    this.flushTimer = setInterval(() => this.flush(), cfg.flushMs);
    if (this.flushTimer && typeof this.flushTimer === 'object' && 'unref' in this.flushTimer) {
      // Don't keep a Node test process alive (no-op on RN).
      (this.flushTimer as { unref?: () => void }).unref?.();
    }
    return this;
  }

  /** Flush queued events immediately. */
  flush(): void {
    if (!this.cfg || !this.buf.length) return;
    const cfg = this.cfg;
    const batch: Batch = { appId: cfg.appId, sentAt: Date.now(), events: this.buf };
    if (Object.keys(this.ctx).length) batch.ctx = this.ctx;
    this.buf = [];
    if (!cfg.endpoint) {
      if (!cfg.onEvent && typeof console !== 'undefined') {
        // eslint-disable-next-line no-console
        console.debug('[reproit]', batch);
      }
      return;
    }
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (cfg.apiKey) headers.Authorization = `Bearer ${cfg.apiKey}`;
    const body = JSON.stringify(batch);
    // fetch is available in React Native's global scope.
    const f = (globalThis as { fetch?: typeof fetch }).fetch;
    if (typeof f === 'function') {
      f(`${cfg.endpoint}/v1/events`, { method: 'POST', headers, body }).catch(() => {
        /* best-effort: drop on failure (matches web SDK) */
      });
    }
  }

  // ---- context API (mirrors the Flutter SDK) -------------------------------

  /**
   * Attach a hashed user id so the cloud can group "these N users hit it"
   * without storing identity, plus optional context dimensions. The raw
   * `userId` is hashed with SHA-256 (first 16 hex chars), never sent in the
   * clear, byte-identical to the Flutter SDK so the same user maps to the same
   * `uid` across platforms.
   */
  identify(userId: string, context?: Context): ReproItImpl {
    this.ctx.uid = hashUid(userId);
    if (context) Object.assign(this.ctx, context);
    return this;
  }

  /** Set a single PII-safe context dimension (e.g. role, plan, a count bucket). */
  setContext(key: string, value: ContextValue): ReproItImpl {
    this.ctx[key] = value;
    return this;
  }

  /** Merge several context dimensions at once. */
  setContexts(values: Context): ReproItImpl {
    Object.assign(this.ctx, values);
    return this;
  }

  /** The current context dimensions sent with each batch (read-only copy). */
  context(): Context {
    return { ...this.ctx };
  }

  // ---- capture hooks (called by ReproItProvider) ---------------------------

  /** @internal Called by the provider when a touch lands, to label the edge. */
  noteTapLabel(label: string | null): void {
    if (!this.on) return;
    this.pendingAction = label ? `tap:${label}` : 'tap:?';
    this.settle(() => this.observe(this.pendingAction ?? 'auto'));
  }

  /** @internal Called by the provider's navigation listener. */
  noteRoute(routeName: string | null): void {
    if (!this.on) return;
    // The route is BOTH the action label and the screen anchor: the anchor is a
    // prefix of the canonical descriptor (docs/signature.md "Anchor"), so the
    // same structure on two routes hashes to two distinct nodes.
    setAnchor(routeName ?? null);
    this.pendingAction =
      routeName && routeName.length ? `nav:${routeName}` : 'nav';
    this.settle(() => this.observe(this.pendingAction ?? 'nav'));
  }

  /**
   * Resolve the accessible name of the deepest tappable node under a screen
   * point, from the current fiber snapshot. Best-effort: RN does not give a
   * library a synchronous hit-test, so the provider hit-tests measured rects;
   * this is exposed for the provider to map a touch to a label.
   */
  tappableLabels(): string[] {
    if (!this.cfg) return [];
    return snapshot(this.cfg).tappables;
  }

  /**
   * Manually contribute a state snapshot from a canonical Node tree (and an
   * optional anchor). Documented escape hatch for screens the fiber walk can't
   * see (e.g. content rendered into a native module / WebView): the caller
   * supplies the structural tree itself, hashed exactly like the fiber-walk
   * path. The action defaults to 'auto'.
   */
  recordSnapshot(tree: Node, action = 'auto', anchor?: string | null): void {
    if (!this.on || !this.cfg) return;
    const snap = snapshotFromTree(tree, anchor);
    this.commit(snap, action);
  }

  // ---- internals -----------------------------------------------------------

  private settle(fn: () => void): void {
    if (!this.cfg) return;
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.settleTimer = setTimeout(fn, this.cfg.debounceMs);
  }

  /** Snapshot the current fiber tree; record an edge if the signature changed. */
  private observe(action: string): void {
    if (!this.on || !this.cfg) return;
    const snap = snapshot(this.cfg);
    if (!snap.any) return; // nothing rendered yet / tree unreachable
    this.commit(snap, action);
  }

  private commit(snap: Snapshot, action: string): void {
    if (!this.cfg) return;
    if (snap.sig === this.cur) return; // no state change
    const from = this.cur;
    this.cur = snap.sig;
    this.path.push({ sig: snap.sig, action });
    if (this.path.length > this.cfg.pathCap) this.path.shift();
    const ev: EdgeEvent = {
      kind: 'edge',
      action: from === null ? 'load' : action || 'auto',
      to: snap.sig,
      t: Date.now(),
    };
    if (from !== null) ev.from = from;
    if (!this.cfg.redactLabels) ev.labels = snap.labels;
    this.emit(ev);
    this.pendingAction = null;
  }

  private installErrorHook(): void {
    const eu = (globalThis as {
      ErrorUtils?: {
        getGlobalHandler?: () => (e: unknown, isFatal?: boolean) => void;
        setGlobalHandler?: (h: (e: unknown, isFatal?: boolean) => void) => void;
      };
    }).ErrorUtils;
    if (eu && typeof eu.setGlobalHandler === 'function') {
      this.priorGlobalHandler = eu.getGlobalHandler?.() ?? null;
      eu.setGlobalHandler((e: unknown, isFatal?: boolean) => {
        this.recordError(e);
        // Preserve RN's red-box / default behavior.
        if (this.priorGlobalHandler) this.priorGlobalHandler(e, isFatal);
      });
    }
    // Unhandled promise rejections, where a tracker is available.
    const g = globalThis as {
      HermesInternal?: unknown;
      process?: { on?: (ev: string, cb: (r: unknown) => void) => void };
    };
    if (g.process && typeof g.process.on === 'function') {
      g.process.on('unhandledRejection', (reason: unknown) => {
        this.recordError(reason, 'unhandledRejection: ');
      });
    }
  }

  private recordError(e: unknown, prefix = ''): void {
    if (!this.on || !this.cfg) return;
    const err = e as { message?: string; stack?: string } | undefined;
    const message =
      prefix + (err?.message ? String(err.message) : String(e));
    const stackLines = err?.stack
      ? String(err.stack)
          .split('\n')
          .map((l) => l.trim())
          .filter((l) => l.length)
          .slice(0, 8)
      : undefined;
    let source: string | undefined;
    let line: number | undefined;
    if (stackLines && stackLines.length) {
      // best-effort: pull "(file.js:42:13)" out of the top frame
      const m = /(?:\(|@|\s)([^\s()@]+):(\d+):\d+/.exec(stackLines[0]);
      if (m) {
        source = m[1];
        line = Number.parseInt(m[2], 10);
      }
    }
    const ev: ErrorEvent = {
      kind: 'error',
      sig: this.cur ?? '',
      path: this.path.slice(),
      message,
      t: Date.now(),
    };
    if (stackLines) ev.stack = stackLines;
    if (source) ev.source = source;
    if (line !== undefined) ev.line = line;
    const ctx = this.errorContext();
    if (ctx) ev.context = ctx;
    this.emit(ev);
    this.flush(); // errors are worth shipping promptly
  }

  /**
   * Tier-3 on-error context: PII-safe fingerprints of on-screen text fields,
   * under `context.fingerprint`. Best-effort: never throws, returns undefined
   * when no fields are visible / the fiber tree is unreachable. Raw values are
   * fingerprinted to FEATURES and discarded; they never leave this process.
   */
  private errorContext(): ErrorContext | undefined {
    try {
      const fp = fingerprintFields(collectFields());
      if (fp.length) return { fingerprint: fp, fpVersion: FP_VERSION };
    } catch {
      /* fingerprinting must never break error reporting */
    }
    return undefined;
  }

  private emit(ev: ReproItEvent): void {
    if (!this.cfg) return;
    if (this.cfg.onEvent) {
      try {
        this.cfg.onEvent(ev);
      } catch {
        /* host callback must not break telemetry */
      }
    }
    this.buf.push(ev);
    if (this.buf.length >= 50) this.flush();
  }

  /** Tear down (mainly for tests). */
  dispose(): void {
    if (this.settleTimer) clearTimeout(this.settleTimer);
    if (this.flushTimer) clearInterval(this.flushTimer);
    this.settleTimer = null;
    this.flushTimer = null;
    this.on = false;
    this.cfg = null;
    this.buf = [];
    this.path = [];
    this.ctx = {};
    this.cur = null;
    this.pendingAction = null;
  }

  /** @internal test/inspection accessor. */
  _isOn(): boolean {
    return this.on;
  }
}

export const ReproIt = new ReproItImpl();
export default ReproIt;

// Re-export for advanced/manual use.
export { snapshot, snapshotFromTree };
