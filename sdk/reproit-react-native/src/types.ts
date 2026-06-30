/** Event and config types. Field names + defaults mirror the web/Flutter SDKs. */

/** A graph edge (state transition) event. */
export interface EdgeEvent {
  kind: 'edge';
  /** Previous state signature; omitted on the initial `load`. */
  from?: string;
  /** `tap:<label>` | `nav:<route>` | `load` | `auto`. */
  action: string;
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
  t: number;
}

export type ReproItEvent = EdgeEvent | ErrorEvent;

/** A batch as POSTed to `<endpoint>/v1/events`. */
export interface Batch {
  appId: string;
  sentAt: number;
  /**
   * PII-safe context dimensions for the batch (omitted when empty). Scalar
   * dimensions plus an optional developer-provided `build` identity (the cloud
   * reads `context.build.version`/`.commit` to segment bugs by build).
   */
  ctx?: Record<string, string | number | boolean | null> & {
    build?: { version?: string; commit?: string };
  };
  events: ReproItEvent[];
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
   * Developer-provided build identity, stamped into every event's context as
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
