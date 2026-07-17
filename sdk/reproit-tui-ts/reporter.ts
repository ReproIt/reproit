// reporter.ts is the embeddable production half of the TypeScript TUI SDK. A
// JS/TS terminal-UI app (Ink, blessed, neo-blessed, ink-based CLIs, or a
// hand-rolled raw-mode dashboard) creates one Reporter, calls observe(...) with
// each rendered screen, and the SDK:
//
//  1. computes the SAME TUI screen signature the fuzz runner computes
//     (signature.ts, a byte-for-byte port of crates/tui-sig/src/lib.rs),
//  2. records a coverage EDGE whenever the structural signature changes (and uses
//     the content fingerprint as the Layer-1 effect token, exactly like the
//     runner and the Go/Rust SDKs),
//  3. batches events and POSTs them to the cloud as the SAME wire contract every
//     other reproit SDK uses: {appId, sentAt, ctx?, events},
//  4. installs process crash handlers (uncaughtException / unhandledRejection)
//     plus SIGINT/SIGTERM that flush a crash event carrying the crashing screen's
//     signature before the process dies, so a production crash is reported with
//     the exact state signature to replay locally.
//
// The event shape mirrors reproit-web.js: edge events carry from/action/to and a
// display-only label set; an error/crash event carries the current sig and the
// graph PATH that led to it (the seed of a deterministic repro), like the web
// SDK's error event.
//
// No em dashes anywhere, per project rules.

import { structuralSig, contentFingerprint, labelsOf } from './signature.ts';
import { ScreenContents } from './screen.ts';
import { installCausalFetch } from './causal.ts';
import type { Cursor, Row } from './screen.ts';

// ReproitEvent is one reported telemetry record. Mirrors the {kind, ...} event
// shape the other SDKs emit (reproit-web.js _emit / edge, reporter.go Event):
//   session: a session opened
//   edge:    structural signature changed (from/action/to + display labels)
//   error:   a record_error() call (current sig + graph path + message)
//   crash:   a process crash handler fired (current sig + message), flushed last
export interface ReproitEvent {
  kind: 'session' | 'edge' | 'error' | 'crash';
  t: number; // ms epoch
  from?: string; // edge: previous signature
  action?: string; // edge: the action that caused the move
  to?: string; // edge/crash: the signature
  sig?: string; // error/crash: the current signature
  path?: Array<{ sig: string; action: string }>; // error/crash: graph trail
  labels?: string[]; // edge: display-only word set (never the sig)
  message?: string; // error/crash: the message
  stack?: string[]; // error/crash: trimmed stack frames
}

// Batch is the wire contract POSTed to the endpoint: identical to every other
// reproit SDK ({appId, sentAt, ctx?, events}).
export interface Batch {
  appId: string;
  sentAt: number;
  ctx?: Record<string, unknown>;
  events: ReproitEvent[];
}

export interface ReporterConfig {
  appId: string; // application identifier (sent in every batch)
  endpoint?: string | null; // POST target; if null/"", events go to onEvent / are dropped
  ctx?: Record<string, unknown>; // optional static context attached to every batch
  onEvent?: (ev: ReproitEvent) => void; // optional local sink (testing / custom transport)
  flushAt?: number; // buffered-event count that triggers an auto flush (default 50)
  pathCap?: number; // how much of the graph trail to keep for repros (default 60)
  redactLabels?: boolean; // drop the human label set from edge events
  // fetchImpl lets a host inject fetch (Node >= 18 has a global fetch; this is an
  // escape hatch for older runtimes or for tests). Defaults to globalThis.fetch.
  fetchImpl?: typeof fetch;
}

// Reporter is the embeddable session/coverage/crash reporter.
export class Reporter {
  private cfg: Required<Omit<ReporterConfig, 'endpoint' | 'ctx' | 'onEvent' | 'fetchImpl'>> &
    Pick<ReporterConfig, 'endpoint' | 'ctx' | 'onEvent' | 'fetchImpl'>;
  private buf: ReproitEvent[] = [];
  private cur = ''; // current structural signature
  private curFP = ''; // current content fingerprint (Layer-1 effect token, ephemeral)
  private path: Array<{ sig: string; action: string }> = [];
  private crashInstalled = false;
  private handlers: Array<{ event: string; fn: (...a: unknown[]) => void }> = [];
  // App-declared invariants, idempotent by id. Inert in production; evaluated
  // only under the fuzzer (see reportInvariants).
  private invariants = new Map<string, () => unknown>();

  constructor(cfg: ReporterConfig) {
    this.cfg = {
      appId: cfg.appId ?? 'app',
      endpoint: cfg.endpoint ?? null,
      ctx: cfg.ctx,
      onEvent: cfg.onEvent,
      flushAt: cfg.flushAt && cfg.flushAt > 0 ? cfg.flushAt : 50,
      pathCap: cfg.pathCap && cfg.pathCap > 0 ? cfg.pathCap : 60,
      redactLabels: !!cfg.redactLabels,
      fetchImpl: cfg.fetchImpl,
    };
    this.emit({ kind: 'session', t: 0 });
    installCausalFetch(this.cfg.endpoint);
  }

  // observe records the current rendered screen. If its STRUCTURAL signature
  // differs from the last one, an edge event is recorded (the runner's coverage
  // edge). The CONTENT fingerprint is tracked too: a value-only change (same
  // skeleton, different on-screen number) is detected as an effect, exactly as
  // the runner does, but it is ephemeral and never becomes the canonical state
  // identity.
  //
  // `action` is the user/agent action that produced this screen (e.g. "key:Down",
  // "key:Enter"); pass "" for an unattributed observation (records as "auto").
  observe(screen: ScreenContents, action = ''): void {
    this.observeContents(screen.text, screen.cursorRow, screen.cursorCol, action);
  }

  // observeText is a convenience for the Ink path: hand the rendered frame string
  // (and optionally a cursor) directly. Equivalent to
  // observe(ScreenContents.fromText(text, cursor), action).
  observeText(text: string, cursor: Cursor = [0, 0], action = ''): void {
    this.observeContents(text, cursor[0], cursor[1], action);
  }

  // observeRows is a convenience for the cell-grid path: hand a row-major cell
  // grid. Equivalent to observe(ScreenContents.fromRows(rows, cursor), action).
  observeRows(rows: Row[], cursor: Cursor = [0, 0], action = ''): void {
    const sc = ScreenContents.fromRows(rows, cursor);
    this.observeContents(sc.text, sc.cursorRow, sc.cursorCol, action);
  }

  // observeContents is the low-level path: the exact vt100-style contents string
  // plus the 0-based cursor cell.
  observeContents(contents: string, cursorRow: number, cursorCol: number, action = ''): void {
    const sig = structuralSig(contents, cursorRow, cursorCol);
    const fp = contentFingerprint(contents, cursorRow, cursorCol);
    // App-invariant oracle (SDK-self-triggered): under the fuzzer, evaluate the
    // app's registered predicates against this state and report failures on the
    // channel the TUI backend scrapes. No-op in production.
    this.reportInvariants(sig);
    const from = this.cur;
    const sigChanged = sig !== this.cur;
    this.cur = sig;
    this.curFP = fp;

    if (!sigChanged) {
      // No structural change. A value-only effect is real but does not open a new
      // coverage edge; the runner records edges only on signature change, so we
      // match that and keep the cloud graph identical.
      return;
    }
    const act = action === '' ? 'auto' : action;
    this.path.push({ sig, action: act });
    if (this.path.length > (this.cfg.pathCap as number)) this.path.shift();
    const labels = this.cfg.redactLabels ? undefined : labelsOf(contents);
    this.emit({ kind: 'edge', t: 0, from, action: act, to: sig, labels });
  }

  // currentSig returns the last observed structural signature (the state to
  // replay). currentFingerprint returns the ephemeral Layer-1 effect token.
  currentSig(): string {
    return this.cur;
  }
  currentFingerprint(): string {
    return this.curFP;
  }

  // invariant registers an app invariant: a predicate the app declares that must
  // hold in EVERY visited state. `test` returns truthy when it HOLDS, or falsy /
  // throws / an object { ok: false, message } when it is VIOLATED. Under the
  // fuzzer the SDK evaluates every registered invariant on each observe and
  // reports the failures for the runner to turn into `invariant` findings; in
  // production the registry is INERT (evaluated only under the fuzzer), so it is
  // zero-overhead until a run reproduces it. Registration is idempotent by id, so
  // re-registering an id replaces it. Returns `this` for chaining.
  invariant(id: string, test: () => unknown): this {
    if (typeof id === 'string' && typeof test === 'function') {
      this.invariants.set(id, test);
    }
    return this;
  }

  // reportInvariants evaluates every registered invariant and, ONLY under the
  // fuzzer (the REPROIT_INVARIANT_FILE env var the TUI backend sets is present
  // and names a file, which is also the fuzzer-detection gate), appends one
  // marker line
  //   REPROIT_INVARIANT {"sig":"<sig>","items":[{"id","message"}...]}
  // listing the VIOLATED invariants to that file. The TUI backend scrapes the
  // file and re-emits each as EXPLORE:INVARIANT. A file (not stderr) is the
  // channel because a TUI's stdout/stderr ARE its rendered frames in the PTY
  // (see crates/reproit/src/backends/tui.rs). Silent when the registry is empty
  // or every invariant held; inert in production (env var unset).
  private reportInvariants(sig: string): void {
    if (this.invariants.size === 0) return;
    const proc = (globalThis as { process?: NodeProcess }).process;
    const path = proc?.env?.REPROIT_INVARIANT_FILE;
    if (!path) return;
    const items: Array<{ id: string; message: string }> = [];
    for (const [id, test] of this.invariants) {
      const message = evalInvariant(test);
      if (message !== null) items.push({ id, message });
    }
    if (items.length === 0) return;
    const line = 'REPROIT_INVARIANT ' + JSON.stringify({ sig, items }) + '\n';
    // The fuzzer only runs in Node. Prefer a sync require (CJS / tsx), and fall
    // back to a cached dynamic import for Node ESM (node --test .ts), where no
    // global `require` exists. The dynamic write still lands well before the
    // runner's post-settle read of the marker file. Guarded so a non-Node bundle
    // (which never sets the env var and so never reaches here) never touches fs.
    try {
      const req = (globalThis as { require?: (m: string) => unknown }).require;
      if (typeof req === 'function') {
        (req('fs') as { appendFileSync(p: string, d: string): void }).appendFileSync(path, line);
        return;
      }
    } catch {
      // fall through to the dynamic import path
    }
    void import('node:fs')
      .then((fs) => {
        try {
          fs.appendFileSync(path, line);
        } catch {
          // best-effort
        }
      })
      .catch(() => {});
  }

  // recordError emits an error event carrying the current signature and the graph
  // PATH that led to it (the seed of a deterministic repro test), mirroring the
  // web SDK's error event. Does NOT exit; the app keeps running. err may be an
  // Error, a string, or anything stringifiable.
  recordError(err: unknown, action?: string): void {
    const { message, stack } = describe(err);
    // Include the crashing action in the PATH (not only the top-level `action`):
    // an action whose handler throws stops the path one step short of the bug, so
    // a path-based replay would never fire it. Mirrors the GUI SDKs' in-flight
    // append, keeping the repro path complete across every platform.
    const path = action ? [...this.path, { sig: this.cur, action }] : this.path.slice();
    this.emit({
      kind: 'error',
      t: 0,
      sig: this.cur,
      to: this.cur,
      action,
      path,
      message,
      stack,
    });
  }

  // reportCrash emits a crash event (current signature + graph path + message)
  // and flushes synchronously-as-possible. Used by the installed process crash
  // handlers; also callable directly from a caught fatal error.
  reportCrash(err: unknown): void {
    const { message, stack } = describe(err);
    this.emit({
      kind: 'crash',
      t: 0,
      sig: this.cur,
      to: this.cur,
      path: this.path.slice(),
      message,
      stack,
    });
    this.flush();
  }

  // installCrashHandler wires process-level crash + termination handlers that
  // report a crash event and flush before the process dies, so a production crash
  // is reported with the exact state signature to replay locally. Returns an
  // uninstall function (handy for tests / clean shutdown).
  //
  // It tolerates running outside Node (no `process`): if there is no process
  // EventEmitter it is a no-op. It NEVER swallows the crash: after reporting it
  // re-throws / re-raises so the app's own crash semantics (exit code, default
  // signal disposition) are preserved.
  installCrashHandler(): () => void {
    const proc: NodeProcess | undefined = (globalThis as { process?: NodeProcess }).process;
    if (this.crashInstalled || !proc || typeof proc.on !== 'function') {
      return () => {};
    }
    this.crashInstalled = true;

    const onUncaught = (err: unknown) => {
      this.reportCrash(err);
      // Restore default behavior: rethrow on next tick so the process exits
      // nonzero exactly as it would have, without our handler swallowing it.
      this.removeHandlers();
      throw err;
    };
    const onRejection = (reason: unknown) => {
      this.reportCrash(reason instanceof Error ? reason : 'unhandledRejection: ' + str(reason));
      // Do not throw here (would itself be uncaught); flush already ran.
    };
    const onSignal = (sig: string) => () => {
      this.reportCrash('signal: ' + sig);
      // re-raise with default disposition so the OS exit code is honest
      this.removeHandlers();
      try {
        proc.kill?.(proc.pid as number, sig as NodeJS.Signals);
      } catch {
        proc.exit?.(1);
      }
    };
    const onBeforeExit = () => this.flush();

    this.register(proc, 'uncaughtException', onUncaught as (...a: unknown[]) => void);
    this.register(proc, 'unhandledRejection', onRejection as (...a: unknown[]) => void);
    this.register(proc, 'SIGINT', onSignal('SIGINT'));
    this.register(proc, 'SIGTERM', onSignal('SIGTERM'));
    this.register(proc, 'beforeExit', onBeforeExit);

    return () => this.removeHandlers();
  }

  private register(proc: NodeProcess, event: string, fn: (...a: unknown[]) => void): void {
    proc.on(event, fn);
    this.handlers.push({ event, fn });
  }
  private removeHandlers(): void {
    const proc: NodeProcess | undefined = (globalThis as { process?: NodeProcess }).process;
    if (proc && typeof proc.removeListener === 'function') {
      for (const h of this.handlers) proc.removeListener(h.event, h.fn);
    }
    this.handlers = [];
    this.crashInstalled = false;
  }

  // emit appends an event, stamps it, fans out to onEvent, and auto-flushes at
  // the threshold. A bad onEvent sink must never break reporting.
  private emit(ev: ReproitEvent): void {
    ev.t = Date.now();
    if (this.cfg.onEvent) {
      try {
        this.cfg.onEvent(ev);
      } catch {
        // a bad sink must never break reporting
      }
    }
    this.buf.push(ev);
    if (this.buf.length >= (this.cfg.flushAt as number)) this.flush();
  }

  // flush sends the buffered events to the endpoint as one batch and clears the
  // buffer. Best-effort: a transport failure never throws.
  flush(): void {
    if (this.buf.length === 0) return;
    const events = this.buf;
    this.buf = [];
    const batch: Batch = {
      appId: this.cfg.appId,
      sentAt: Date.now(),
      ctx: this.cfg.ctx,
      events,
    };
    if (!this.cfg.endpoint) {
      // no endpoint: onEvent already saw each event; nothing to POST.
      return;
    }
    const body = JSON.stringify(batch);
    const f = this.cfg.fetchImpl ?? (globalThis as { fetch?: typeof fetch }).fetch;
    if (typeof f !== 'function') return; // no transport available
    try {
      // keepalive lets the POST survive a process that is about to exit.
      const p = f(this.cfg.endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
        keepalive: true,
      });
      // swallow async transport errors; reporting must never throw into the app.
      if (p && typeof (p as Promise<unknown>).catch === 'function') {
        (p as Promise<unknown>).catch(() => {});
      }
    } catch {
      // best-effort
    }
  }
}

// describe extracts a message + trimmed stack from any thrown value.
function describe(err: unknown): { message: string; stack?: string[] } {
  if (err instanceof Error) {
    const stack = err.stack ? String(err.stack).split('\n').slice(0, 8) : undefined;
    return { message: err.message || err.name, stack };
  }
  return { message: str(err) };
}

function str(v: unknown): string {
  if (typeof v === 'string') return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// Minimal structural type for the Node process EventEmitter surface we touch, so
// this file type-checks without @types/node and runs under Node's type-stripping.
interface NodeProcess {
  on(event: string, fn: (...args: unknown[]) => void): unknown;
  removeListener(event: string, fn: (...args: unknown[]) => void): unknown;
  kill?(pid: number, signal?: string): unknown;
  exit?(code?: number): never;
  pid?: number;
  env?: Record<string, string | undefined>;
}

// evalInvariant runs one predicate. Returns null when it HOLDS, or a failure
// message string when it is VIOLATED. Mirrors the web SDK contract: truthy holds;
// falsy / throws / { ok: false, message } is a violation (the thrown text, the
// object's message, or "" for a bare falsy).
function evalInvariant(test: () => unknown): string | null {
  let result: unknown;
  try {
    result = test();
  } catch (err) {
    return describe(err).message;
  }
  if (result && typeof result === 'object' && 'ok' in result) {
    const obj = result as { ok?: unknown; message?: unknown };
    return obj.ok === false ? str(obj.message ?? '') : null;
  }
  return result ? null : '';
}
