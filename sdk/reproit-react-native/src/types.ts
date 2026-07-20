/** Event and config types. Field names + defaults mirror the web/Flutter SDKs. */

/** A graph edge (state transition) event. */
export interface EdgeEvent {
  kind: 'edge';
  /** Previous state signature; omitted on the initial `load`. */
  from?: string;
  /**
   * Structural replay action: `tap:key:<id>`, `tap:role:<role>#<idx>`,
   * `nav:<route>`, `load`, or `auto`.
   */
  action: string;
  /** Human-readable display label for the action, omitted when redactLabels. */
  label?: string;
  /** Destination state signature. */
  to: string;
  /** Visible accessible names of the destination (omitted when redactLabels). */
  labels?: string[];
  /** Wall-clock ms (Date.now()). */
  t: number;
}

/** One step of the graph trail kept for repros. */
export interface PathStep {
  sig: string;
  action: string;
  label?: string;
}

/**
 * Tier-3 on-error context. Rides the error event so the cloud can property-match
 * a replay fixture. `fingerprint` holds PII-safe FEATURES of on-screen text
 * fields (never raw values).
 */
export interface ErrorContext {
  fingerprint?: Array<{
    field: string;
    len: number;
    bytes: number;
    graphemes: number;
    charset: 'ascii' | 'numeric' | 'unicode';
    scripts: string[];
    hasEmoji: boolean;
    isEmpty: boolean;
    isRtl: boolean;
    hasCombiningMarks: boolean;
    hasZeroWidth: boolean;
    hasNewline: boolean;
    leadingTrailingWhitespace: boolean;
  }>;
  /** Fingerprint schema version stamped alongside the array (see FP_VERSION). */
  fpVersion?: number;
}

/** An uncaught-error event carrying the graph path that produced it. */
export interface ErrorEvent {
  kind: 'error';
  /** The oracle this finding fired: a genuine uncaught error IS the `crash` oracle. */
  oracle: 'crash' | 'tester-capture' | 'invariant';
  /** State signature where the error happened. */
  sig: string;
  /** The graph trail leading here. */
  path: PathStep[];
  message: string;
  /** Up to 8 stack lines. */
  stack?: string[];
  source?: string;
  line?: number;
  /** PII-safe on-error context (input fingerprints under `fingerprint`). */
  context?: ErrorContext;
  /** Stable structural identity used to verify an explicit tester capture. */
  findingIdentity?: {
    oracle: 'tester-capture' | 'invariant';
    invariant: string;
    kind: 'structural-state' | 'structural-contract';
    message: string;
    frame: '';
    trigger: string;
    boundary: string;
  };
  t: number;
}

export type ReproItEvent = EdgeEvent | ErrorEvent;

/**
 * An app invariant predicate (see {@link ReproIt.invariant}). Returns truthy when
 * the invariant HOLDS; a falsy value, a thrown error, or an `{ ok:false, message }`
 * object marks it VIOLATED (the message rides the finding). Mirrors the web SDK's
 * `invariant(id, test)` contract.
 */
export type InvariantResult = boolean | { ok: boolean; message?: string };
export type InvariantPredicate = () => InvariantResult;

export interface ProtocolPathStep {
  signature: string;
  action: string;
  label: string | null;
}

export interface ProtocolFindingIdentity {
  oracle: string;
  invariant: string;
  kind: string;
  message: string;
  frame: string;
  trigger: string;
  boundary?: string | null;
}

export type ProtocolEvent =
  | { kind: 'graph-edge'; from: string; action: string; to: string }
  | {
      kind: 'finding';
      signature: string;
      message: string;
      identity: ProtocolFindingIdentity;
      path: ProtocolPathStep[];
      context: Record<string, unknown>;
    }
  | { kind: 'stream-defect'; reason: 'invalid-event' };

export interface EventFrame {
  runId: string;
  sequence: number;
  scope: { domain: 'shared' };
  event: ProtocolEvent;
}

/** The strict versioned batch POSTed to `<endpoint>/v1/events`. */
export interface Batch {
  version: 1;
  batchId: string;
  appId: string;
  deployment?: { version?: string; commit?: string };
  frames: EventFrame[];
  evidence: [];
}

/**
 * Configuration for {@link init}. Names + defaults mirror the web SDK
 * (`sdk/reproit-web.js`) and Flutter SDK so behavior is consistent across
 * platforms.
 */
export interface ReproItConfig {
  /** Identifies the app in the cloud (the `appId` in every batch). Required. */
  appId: string;
  /**
   * Developer-provided build identity, stamped into every finding's context as
   * `context.build = { version, commit }` (only the provided fields). RN can't
   * auto-detect these without a native module, so the developer supplies them
   * from their build pipeline (app version from package.json/Info.plist/gradle,
   * git commit from CI). The cloud reads `context.build.version`/`.commit` to
   * segment bugs by build ("regressed in 1.4.2 / no hits since 1.4.5").
   * Omitted entirely when not set.
   */
  build?: { version?: string; commit?: string };
  /** `POST <endpoint>/v1/events`. If null/undefined, events go only to
   *  onEvent / the debug console. */
  endpoint?: string | null;
  /** Bearer token sent as `Authorization: Bearer <apiKey>` when set. */
  apiKey?: string | null;
  /** Dev hook / custom transport; called for every event. */
  onEvent?: ((event: ReproItEvent) => void) | null;
  /** Fraction of sessions that report (0..1). Decided once at init. */
  sampleRate?: number;
  /** Max distinct labels captured per state signature (matches the runner). */
  maxLabels?: number;
  /** Labels longer than this are ignored (matches the runner). */
  maxLabelLen?: number;
  /** Max length of the action trail kept for repro paths. */
  pathCap?: number;
  /** Batch flush interval, ms. */
  flushMs?: number;
  /** When true, only signatures are sent (no human-readable label text). */
  redactLabels?: boolean;
  /** Settle window: snapshot once the UI has been quiet this long, ms. */
  debounceMs?: number;
  /**
   * Layer-3 opt-in selectors (docs/signature.md "Value-state") marking EXTRA
   * value-bearing nodes even when their role is not a value-role. Same grammar as
   * `value_nodes:` in reproit.yaml: `key:<id>` | `role:<role>#<idx>`.
   */
  valueNodes?: string[];
}

/** Fully-resolved config (defaults applied). */
export type ResolvedConfig = Required<
  Omit<ReproItConfig, 'endpoint' | 'apiKey' | 'onEvent' | 'build'>
> & {
  endpoint: string | null;
  apiKey: string | null;
  onEvent: ((event: ReproItEvent) => void) | null;
  /** Developer-provided build identity, or null when not supplied. */
  build: { version?: string; commit?: string } | null;
};

export const DEFAULTS: Omit<ResolvedConfig, 'appId'> = {
  endpoint: null,
  apiKey: null,
  onEvent: null,
  build: null,
  sampleRate: 1.0,
  maxLabels: 24,
  maxLabelLen: 40,
  pathCap: 60,
  flushMs: 5000,
  redactLabels: false,
  debounceMs: 350,
  valueNodes: [],
};
