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
 *   ReproIt.init({ appId: 'example', endpoint: 'https://ingest.reproit.com',
 *                  apiKey: 'pk_live_...' });
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
  type SnapElement,
  type Snapshot,
} from './snapshot';
import type { Node } from './signature';
import { installCausalFetch, nativeCausalCapsule } from './causal';
import { fingerprintFields, FP_VERSION } from './fingerprint';
import { IndicatorRelations, type ReproItIndicatorContract } from './indicator-relation';
import { FocusVisibilityOracle, type FocusVisibilityContract } from './focus-visibility';
import {
  ActionEffectOracle,
  StatePreservationOracle,
  contractMarker,
  type ActionEffectContract,
  type ContractResult,
  type StateBoundary,
  type StatePreservationContract,
} from './structural-contracts';
import {
  DEFAULTS,
  type Batch,
  type EdgeEvent,
  type ErrorContext,
  type ErrorEvent,
  type InvariantPredicate,
  type PathStep,
  type ReproItConfig,
  type ReproItEvent,
  type ResolvedConfig,
} from './types';

export type { ReproItConfig, ReproItEvent, EdgeEvent, ErrorEvent, PathStep, Batch } from './types';
export { signatureOf, descriptorOf, valueClass, type Node } from './signature';
export { setValueNodeSelectors } from './snapshot';
export { ReproItProvider } from './provider';
export type { Context, ContextValue } from './context';
export { fingerprintValue, fingerprintFields, FP_VERSION } from './fingerprint';
export type { ValueFingerprint, FieldFingerprint } from './fingerprint';
export type { ErrorContext, InvariantResult, InvariantPredicate } from './types';
export type {
  ReproItRect,
  ReproItIndicatorGeometry,
  ReproItIndicatorContract,
} from './indicator-relation';
export type { FocusVisibilityObservation, FocusVisibilityContract } from './focus-visibility';
export type {
  ActionEffectContract,
  ActionEffectObservation,
  ContractResult,
  ContractStatus,
  StateBoundary,
  StatePreservationContract,
  StructuralObservation,
} from './structural-contracts';
export { installCausalFetch, redactCausal } from './causal';

function resolveConfig(opts: ReproItConfig): ResolvedConfig {
  return {
    ...DEFAULTS,
    ...opts,
    endpoint: opts.endpoint ?? null,
    apiKey: opts.apiKey ?? null,
    onEvent: opts.onEvent ?? null,
    build: normalizeBuild(opts.build),
  };
}

/**
 * Keep only the provided string fields of a developer-supplied build identity.
 * Returns null when neither `version` nor `commit` is a non-empty string, so no
 * build object is stamped into the context.
 */
function normalizeBuild(
  build: ReproItConfig['build'],
): { version?: string; commit?: string } | null {
  if (!build) return null;
  const out: { version?: string; commit?: string } = {};
  if (typeof build.version === 'string' && build.version.length) {
    out.version = build.version;
  }
  if (typeof build.commit === 'string' && build.commit.length) {
    out.commit = build.commit;
  }
  return out.version || out.commit ? out : null;
}

/** The telemetry singleton. */
class ReproItImpl {
  private cfg: ResolvedConfig | null = null;
  private on = false;
  private buf: ReproItEvent[] = [];
  private path: PathStep[] = [];
  // PII-safe context dimensions sent with each batch (the "which users" answer).
  // The scalar dimensions are `Context`; the developer-provided build identity
  // rides as a nested `{ version?, commit? }` object under `build` (the exact
  // shape the cloud reads at `context.build.version`/`.commit`).
  private ctx: Context & { build?: { version?: string; commit?: string } } = {};
  private cur: string | null = null;
  private pendingStep: { action: string; label?: string } | null = null;
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private priorGlobalHandler: ((e: unknown, isFatal?: boolean) => void) | null = null;
  // App-declared invariants (see `invariant`). A plain SDK-owned store, idempotent
  // by id; INERT in production (never read) and evaluated only when the SDK detects
  // it is running under the fuzzer, so registration is zero-overhead. Mirrors the
  // web SDK's `window.__reproit_invariants`.
  private invariants: Array<{ id: string; test: InvariantPredicate }> = [];
  private causalActionIndex = 0;
  private indicatorRelations = new IndicatorRelations();
  private relationRetryPending = false;
  private focusVisibility = new FocusVisibilityOracle();
  private statePreservation = new StatePreservationOracle();
  private actionEffects = new ActionEffectOracle();

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

    // Developer-provided build identity, stamped under `context.build` so the
    // cloud can segment bugs by build (regressed in / resolved since). Only the
    // provided fields ride; omitted entirely when no build was supplied.
    if (cfg.build) this.ctx.build = cfg.build;

    this.installErrorHook();
    if (this.underFuzzer()) {
      installCausalFetch({
        actionIndex: () => this.causalActionIndex,
        capsule: nativeCausalCapsule(),
        excludePrefix: cfg.endpoint,
      });
    }

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

  /**
   * Zero-config start: the one-line quickstart. Begins telemetry with sensible
   * defaults and no required arguments, then delegates to {@link init} (which
   * stays the full, explicit entry point). Enabled only in a debug/dev build
   * (React Native's `__DEV__` global); a no-op in release unless
   * `enableInRelease` is set, so shipping this one line does nothing in
   * production by default. `appId` defaults to `'app'` when not supplied (RN has
   * no synchronous bundle id without a native module); pass `appId`, or any
   * other config field, to override. Additive and backward-compatible: use
   * {@link init} directly when you want telemetry in every build.
   */
  start(opts: Partial<ReproItConfig> & { enableInRelease?: boolean } = {}): ReproItImpl {
    const dev = (globalThis as { __DEV__?: boolean }).__DEV__;
    // Only skip when explicitly a release build (`__DEV__ === false`); an
    // undefined flag (plain Node/tests, non-RN host) is treated as dev.
    if (dev === false && !opts.enableInRelease) return this;
    const cfg = { ...opts };
    delete cfg.enableInRelease;
    return this.init({ appId: 'app', ...cfg } as ReproItConfig);
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

  /** Capture the current structural state as a tester-observed bug. */
  captureBug(): boolean {
    if (!this.on || !this.cfg) return false;
    const snap = snapshot(this.cfg);
    if (!this.cur) {
      this.cur = snap.sig;
      this.path.push({ sig: snap.sig, action: 'load' });
    } else if (snap.sig !== this.cur) {
      const step = this.pendingStep ?? { action: 'auto' };
      this.path.push({
        sig: snap.sig,
        action: step.action,
        ...(step.label && !this.cfg.redactLabels ? { label: step.label } : {}),
      });
      this.cur = snap.sig;
      this.pendingStep = null;
    }
    if (this.path.length > this.cfg.pathCap) {
      this.path.splice(0, this.path.length - this.cfg.pathCap);
    }
    const trigger = this.path.length ? this.path[this.path.length - 1].action : 'load';
    const ev: ErrorEvent = {
      kind: 'error',
      oracle: 'tester-capture',
      sig: snap.sig,
      path: this.path.slice(),
      message: 'Tester observed a bug in this state',
      findingIdentity: {
        oracle: 'tester-capture',
        invariant: 'tester-observed-failure',
        kind: 'structural-state',
        message: '',
        frame: '',
        trigger,
        boundary: snap.sig,
      },
      t: Date.now(),
    };
    const ctx = this.errorContext();
    if (ctx) ev.context = ctx;
    this.emit(ev);
    this.flush();
    return true;
  }

  private captureContractBug(result: ContractResult): boolean {
    if (!this.on || !this.cfg || result.status !== 'PROVEN') return false;
    const snap = snapshot(this.cfg);
    if (!this.cur) {
      this.cur = snap.sig;
      this.path.push({ sig: snap.sig, action: 'load' });
    } else if (snap.sig !== this.cur) {
      const step = this.pendingStep ?? { action: 'auto' };
      this.path.push({
        sig: snap.sig,
        action: step.action,
        ...(step.label && !this.cfg.redactLabels ? { label: step.label } : {}),
      });
      this.cur = snap.sig;
      this.pendingStep = null;
    }
    if (this.path.length > this.cfg.pathCap)
      this.path.splice(0, this.path.length - this.cfg.pathCap);
    const trigger = this.path.length ? this.path[this.path.length - 1].action : 'load';
    const ev: ErrorEvent = {
      kind: 'error',
      oracle: 'invariant',
      sig: snap.sig,
      path: this.path.slice(),
      message: result.message ?? result.id,
      findingIdentity: {
        oracle: 'invariant',
        invariant: result.id,
        kind: 'structural-contract',
        message: result.message ?? result.id,
        frame: '',
        trigger,
        boundary: snap.sig,
      },
      t: Date.now(),
    };
    const ctx = this.errorContext();
    if (ctx) ev.context = ctx;
    this.emit(ev);
    this.flush();
    return true;
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
  context(): Context & { build?: { version?: string; commit?: string } } {
    return { ...this.ctx };
  }

  /**
   * Register an app invariant: a predicate the app declares that must hold in
   * EVERY visited state (a running total never negative, the selected tab always
   * highlighted). `predicate` returns truthy when it holds, or falsy / throws /
   * an `{ ok:false, message }` object when it is violated. Registration is
   * idempotent by id (a hot reload re-registering the same id replaces it), and
   * INERT in production: the predicate is stored but only evaluated when the SDK
   * detects it is running under the reproit fuzzer (see `underFuzzer`), so this
   * is zero-overhead until a run reproduces it. Under the fuzzer, a violated
   * invariant is logged as a `REPROIT_INVARIANT` marker on the JS console (which
   * lands in logcat / syslog) for the mobile runner to scrape. Mirrors the web
   * SDK's `ReproIt.invariant`.
   */
  invariant(id: string, predicate: InvariantPredicate): ReproItImpl {
    if (typeof id !== 'string' || typeof predicate !== 'function') return this;
    for (let i = 0; i < this.invariants.length; i++) {
      if (this.invariants[i].id === id) {
        this.invariants[i].test = predicate;
        return this;
      }
    }
    this.invariants.push({ id, test: predicate });
    return this;
  }

  /** Declare an explicit indicator owner using global screen rectangles, normally
   * populated from `measureInWindow`. Ambiguous or moving geometry abstains. */
  indicator(id: string, contract: ReproItIndicatorContract): ReproItImpl {
    this.indicatorRelations.register(id, contract);
    return this;
  }

  /** Register the focused editable, exact usable viewport, and its owning
   * ScrollView reveal operation. Without a safe reveal provider this abstains. */
  focusedInput(id: string, contract: FocusVisibilityContract): ReproItImpl {
    this.focusVisibility.register(id, contract);
    return this;
  }

  /** Declare structural state that must survive an explicit platform boundary. */
  preserveState(id: string, contract: StatePreservationContract): ReproItImpl {
    this.statePreservation.register(id, contract);
    return this;
  }

  /** Report an authoritative lifecycle boundary. Platform adapters call before
   * and after only after the UI has settled; unsupported boundaries abstain. */
  stateBoundary(kind: StateBoundary, phase: 'before' | 'after'): ContractResult[] {
    const results = this.statePreservation.boundary(kind, phase);
    this.publishContractResults(results);
    return results;
  }

  /** Declare the exact route and local-state effects of an action. */
  actionEffect(id: string, contract: ActionEffectContract): ReproItImpl {
    this.actionEffects.register(id, contract);
    return this;
  }

  actionBegin(id: string): ContractResult[] {
    const results = this.actionEffects.begin(id);
    this.publishContractResults(results);
    return results;
  }

  actionEnd(id: string): ContractResult[] {
    const results = this.actionEffects.end(id);
    this.publishContractResults(results);
    return results;
  }

  private publishContractResults(results: ContractResult[]): void {
    const marker = contractMarker(results);
    if (!marker) return;
    if (this.underFuzzer()) {
      if (typeof console !== 'undefined') console.log(marker);
    } else if (this.on) {
      for (const result of results) this.captureContractBug(result);
    }
  }

  // ---- capture hooks (called by ReproItProvider) ---------------------------

  /** @internal Called by the provider when a touch lands, to record the edge. */
  noteTapTarget(target: Pick<SnapElement, 'sel' | 'label'> | null): void {
    if (!this.on) return;
    this.causalActionIndex++;
    this.pendingStep = {
      action: target?.sel ? `tap:${target.sel}` : 'tap:?',
      label: target?.label || undefined,
    };
    this.settle(() => this.observe(this.pendingStep ?? { action: 'auto' }));
  }

  /** @internal Called by the provider's navigation listener. */
  noteRoute(routeName: string | null): void {
    if (!this.on) return;
    this.causalActionIndex++;
    // The route is BOTH the action label and the screen anchor: the anchor is a
    // prefix of the canonical descriptor (docs/signature.md "Anchor"), so the
    // same structure on two routes hashes to two distinct nodes.
    setAnchor(routeName ?? null);
    this.pendingStep = {
      action: routeName && routeName.length ? `nav:${routeName}` : 'nav',
    };
    this.settle(() => this.observe(this.pendingStep ?? { action: 'nav' }));
  }

  /**
   * Resolve the accessible name of the deepest tappable node under a screen
   * point, from the current fiber snapshot. Best-effort: RN does not give a
   * library a synchronous hit-test, so the provider hit-tests measured rects;
   * this is exposed for the provider to map a touch to an action.
   */
  tappableTargets(): SnapElement[] {
    if (!this.cfg) return [];
    return snapshot(this.cfg).elements;
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
    this.checkInvariants();
    this.checkIndicatorRelations();
  }

  // ---- internals -----------------------------------------------------------

  private settle(fn: () => void): void {
    if (!this.cfg) return;
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.settleTimer = setTimeout(fn, this.cfg.debounceMs);
  }

  /** Snapshot the current fiber tree; record an edge if the signature changed. */
  private observe(step: string | { action: string; label?: string }): void {
    if (!this.on || !this.cfg) return;
    const snap = snapshot(this.cfg);
    if (!snap.any) return; // nothing rendered yet / tree unreachable
    this.commit(snap, step);
    // Self-triggered oracle: the native fuzzer drives this app and cannot call
    // the app's predicates, so the SDK evaluates its OWN registered invariants
    // on each settled state and emits a marker for the violations. Runs only
    // under the fuzzer; a no-op (and zero-cost) in production.
    this.checkInvariants();
    this.checkIndicatorRelations();
  }

  private checkIndicatorRelations(): void {
    if (!this.underFuzzer()) return;
    const marker = this.indicatorRelations.marker();
    if (marker && typeof console !== 'undefined') console.log(marker);
    const focusMarker = this.focusVisibility.marker();
    if (focusMarker && typeof console !== 'undefined') console.log(focusMarker);
    if (this.relationRetryPending) return;
    this.relationRetryPending = true;
    setTimeout(() => {
      const confirmed = this.indicatorRelations.marker();
      if (confirmed && typeof console !== 'undefined') console.log(confirmed);
      const focusConfirmed = this.focusVisibility.marker();
      if (focusConfirmed && typeof console !== 'undefined') console.log(focusConfirmed);
      const delay = this.cfg?.debounceMs ?? 350;
      setTimeout(() => {
        this.relationRetryPending = false;
        const finalFocus = this.focusVisibility.marker();
        if (finalFocus && typeof console !== 'undefined') console.log(finalFocus);
      }, delay);
    }, this.cfg?.debounceMs ?? 350);
  }

  /**
   * Whether this app is running under the reproit fuzzer. RN has no
   * `navigator.webdriver` equivalent (the JS VM is native-hosted, not a browser),
   * and Appium cannot set a JS global in the RN VM, so the reproit E2E build
   * opts in by setting a stable, SDK-owned global (`global.__reproit_fuzz`) or a
   * bundled `process.env.REPROIT_FUZZ` in its app entry. Off => the invariant
   * registry is never evaluated.
   */
  private underFuzzer(): boolean {
    const g = globalThis as { __reproit_fuzz?: unknown };
    const flag = g.__reproit_fuzz;
    if (flag === true || flag === 1 || flag === '1') return true;
    const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process
      ?.env;
    if (env && (env.REPROIT_FUZZ === '1' || env.REPROIT_FUZZ === 'true')) return true;
    return false;
  }

  /**
   * Evaluate every registered invariant on the settled state `sig`; when running
   * under the fuzzer and one or more are violated, emit ONE `REPROIT_INVARIANT`
   * marker on the JS console (logcat on Android, syslog on iOS) for the mobile
   * runner to map into an `EXPLORE:INVARIANT` line. The emitted sig is left empty
   * ("") so the runner substitutes the sig it is currently on. Each predicate is
   * isolated in try/catch so one throwing predicate cannot suppress the others.
   * Silent when no invariant was registered or all held.
   */
  private checkInvariants(): void {
    if (!this.on || !this.invariants.length) return;
    if (!this.underFuzzer()) return;
    const items: Array<{ id: string; message: string }> = [];
    for (let i = 0; i < this.invariants.length; i++) {
      const it = this.invariants[i];
      if (!it || typeof it.test !== 'function') continue;
      let ok = true;
      let message = '';
      try {
        const r = it.test();
        if (r && typeof r === 'object') {
          ok = !!r.ok;
          message = r.message ? String(r.message) : '';
        } else {
          ok = !!r;
        }
      } catch (e) {
        ok = false;
        const err = e as { message?: string } | undefined;
        message = err && err.message ? String(err.message) : String(e);
      }
      if (!ok) items.push({ id: String(it.id), message });
    }
    if (!items.length) return;
    if (typeof console !== 'undefined' && typeof console.log === 'function') {
      // eslint-disable-next-line no-console
      console.log('REPROIT_INVARIANT ' + JSON.stringify({ sig: '', items }));
    }
  }

  private commit(snap: Snapshot, step: string | { action: string; label?: string }): void {
    if (!this.cfg) return;
    if (snap.sig === this.cur) return; // no state change
    const action = typeof step === 'string' ? step : step.action;
    const label = typeof step === 'string' ? undefined : step.label;
    const from = this.cur;
    this.cur = snap.sig;
    this.path.push({
      sig: snap.sig,
      action,
      ...(label && !this.cfg.redactLabels ? { label } : {}),
    });
    if (this.path.length > this.cfg.pathCap) this.path.shift();
    const ev: EdgeEvent = {
      kind: 'edge',
      action: from === null ? 'load' : action || 'auto',
      to: snap.sig,
      t: Date.now(),
    };
    if (from !== null) ev.from = from;
    if (!this.cfg.redactLabels) ev.labels = snap.labels;
    if (label && !this.cfg.redactLabels) ev.label = label;
    this.emit(ev);
    this.pendingStep = null;
  }

  private installErrorHook(): void {
    const eu = (
      globalThis as {
        ErrorUtils?: {
          getGlobalHandler?: () => (e: unknown, isFatal?: boolean) => void;
          setGlobalHandler?: (h: (e: unknown, isFatal?: boolean) => void) => void;
        };
      }
    ).ErrorUtils;
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
    const message = prefix + (err?.message ? String(err.message) : String(e));
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
    // Include the in-flight action: a press whose handler throws synchronously
    // (the crashing tap) sets `pendingStep` but crashes before its debounced
    // observe records it, so the bare path stops one step short of the bug.
    const errPath = this.path.slice();
    if (this.pendingStep) {
      errPath.push({
        sig: this.cur ?? '',
        action: this.pendingStep.action,
        ...(this.pendingStep.label && !this.cfg.redactLabels
          ? { label: this.pendingStep.label }
          : {}),
      });
    }
    const ev: ErrorEvent = {
      kind: 'error',
      // A genuine uncaught error IS the `crash` oracle firing; tag it so the
      // cloud can gate ingest on oracle-grade findings.
      oracle: 'crash',
      sig: this.cur ?? '',
      path: errPath,
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
    this.pendingStep = null;
    this.invariants = [];
    this.indicatorRelations.clear();
    this.relationRetryPending = false;
    this.focusVisibility.clear();
    this.statePreservation.clear();
    this.actionEffects.clear();
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
