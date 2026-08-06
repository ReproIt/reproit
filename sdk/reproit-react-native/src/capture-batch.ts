/**
 * capture-batch-v1 emission for React Native.
 *
 * The legacy `/v1/events` batch stores a signature, a message, and an action
 * path: enough to group and prioritize a bug, not enough to re-execute it.
 * A complete failure capture ships here instead, on
 * `/v1/capture-batches`, carrying the trigger, every dependency exchange, the
 * determinism envelope, and the failure observation.
 *
 * `sdk/reproit-recorder-node` is the reference emitter but depends on
 * `node:crypto`, so this is a bounded port of the same wire contract rather
 * than a dependency.
 */

import type { ProductionExchange } from './exchange';

/** Protocol token charset (`validate_token` in reproit-protocol). */
const TOKEN = /^[A-Za-z0-9._:-]{1,128}$/;
/** Events per batch. The protocol allows more; a device ships far fewer. */
const MAX_BATCH_EVENTS = 256;

export type CapturedValue =
  | { representation: 'replayable'; value: unknown; redaction: 'redacted-at-source' }
  | { representation: 'structural'; shape: unknown };

export type CaptureEvent = {
  id: string;
  sequence: number;
  monotonicNs: number;
  causalParentIds: string[];
  traceId?: string;
  event: Record<string, unknown>;
};

export type CaptureBatch = {
  version: 1;
  batchId: string;
  projectId: string;
  sessionId: string;
  emitter: { id: string; kind: string; component: string; runtime: string };
  deployment?: { version?: string; commit?: string };
  observedAt: string;
  policy: { consent: string; retentionClass: string };
  capabilities: Array<{ capability: string; completeness: string; detail?: string }>;
  events: CaptureEvent[];
  artifacts: [];
};

export function replayable(value: unknown): CapturedValue {
  return { representation: 'replayable', value, redaction: 'redacted-at-source' };
}

export function structural(shape: unknown): CapturedValue {
  return { representation: 'structural', shape };
}

export function validToken(value: unknown): value is string {
  return typeof value === 'string' && TOKEN.test(value);
}

/** The failure a capture batch is built around. */
export type CaptureFailure = {
  /** Registry oracle id, e.g. `crash`. */
  oracle: string;
  summary: string;
  signature: string;
  observationPoint: string;
};

/** What the app was doing, and what its dependencies answered. */
export type CaptureOccurrence = {
  operation: string;
  trigger: unknown;
  exchanges: ProductionExchange[];
  failure: CaptureFailure;
  envelope: Record<string, unknown>;
};

/**
 * Build one capture batch for a single failed operation. One operation per
 * batch keeps unrelated failures from sharing an occurrence identity, which
 * is the same rule the backend SDKs follow.
 */
export function buildCaptureBatch(options: {
  appId: string;
  sessionId: string;
  batchId: string;
  occurrence: CaptureOccurrence;
  deployment?: { version?: string; commit?: string } | null;
  observedAt?: string;
}): CaptureBatch {
  const emitterId = 'reproit-react-native';
  const events: CaptureEvent[] = [];
  let sequence = 0;
  let parent: string | null = null;
  const traceId = options.sessionId;

  const push = (event: Record<string, unknown>, monotonicNs?: number): string => {
    sequence += 1;
    const id = `evt_${emitterId}_${sequence}`;
    events.push({
      id,
      sequence,
      monotonicNs: Number.isFinite(monotonicNs) ? (monotonicNs as number) : sequence,
      causalParentIds: parent === null ? [] : [parent],
      traceId,
      event,
    });
    parent = id;
    return id;
  };

  const occurrence = options.occurrence;
  push({ kind: 'operation-start', name: occurrence.operation });
  push({
    kind: 'trigger',
    // A device failure is triggered by what the user did, not by a request
    // arriving; `ui-action` is that variant in the protocol vocabulary.
    trigger: 'ui-action',
    subject: occurrence.operation,
    value:
      occurrence.trigger === undefined || occurrence.trigger === null
        ? structural({ type: 'unknown' })
        : replayable(occurrence.trigger),
  });
  // Determinism envelope: when and where the capture happened, plus the seed
  // that pins the REPLAY run. The seed does not reproduce the randomness the
  // app drew in production; it makes repeated replays agree.
  push({ kind: 'checkpoint', name: 'determinism-envelope', attributes: occurrence.envelope });

  for (const exchange of occurrence.exchanges.slice(0, MAX_BATCH_EVENTS - events.length - 2)) {
    // The raw exchange nests verbatim under a dependency carrier, exactly as
    // the backend SDKs nest theirs, so one projection inverts both.
    push(
      {
        kind: 'dependency',
        system: 'service',
        operation: 'call',
        subject: String(exchange.request.url ?? 'dependency').slice(0, 256),
        value: replayable({
          kind: 'effect',
          effect: 'call',
          resource: String(exchange.request.url ?? 'dependency').slice(0, 256),
          exchange,
          ...(exchange.at === undefined ? {} : { at: exchange.at }),
          ...(exchange.monoNs === undefined ? {} : { monoNs: exchange.monoNs }),
        }),
      },
      exchange.monoNs,
    );
  }

  push({ kind: 'operation-end', name: occurrence.operation, outcome: 'failed' });
  push({
    kind: 'observation',
    failure: {
      observation: 'exception',
      authority: 'runtime-diagnosis',
      summary: occurrence.failure.summary,
      signature: occurrence.failure.signature,
      observationPoint: occurrence.failure.observationPoint,
      artifactIds: [],
    },
  });

  const deployment =
    options.deployment && (options.deployment.version || options.deployment.commit)
      ? {
          ...(options.deployment.version ? { version: options.deployment.version } : {}),
          ...(options.deployment.commit ? { commit: options.deployment.commit } : {}),
        }
      : undefined;

  return {
    version: 1,
    batchId: options.batchId,
    projectId: options.appId,
    sessionId: options.sessionId,
    emitter: {
      id: emitterId,
      kind: 'runtime-sdk',
      component: 'mobile',
      runtime: 'react-native',
    },
    ...(deployment ? { deployment } : {}),
    observedAt: options.observedAt ?? new Date().toISOString(),
    policy: { consent: 'application-telemetry', retentionClass: 'standard' },
    capabilities: [
      {
        capability: 'user-interface',
        completeness: 'complete',
        detail: 'the trigger action and structural state path were recorded',
      },
      ...(occurrence.exchanges.length
        ? [{
            capability: 'network',
            completeness: 'complete',
            detail: 'outbound dependency exchanges recorded with responses',
          }]
        : []),
    ],
    events,
    artifacts: [],
  };
}

/**
 * The determinism envelope a React Native capture can honestly state. The
 * runtime has no process arch or image digest, so those fields are omitted
 * rather than guessed.
 */
export function buildEnvelope(options: {
  observedAtMs: number;
  platform?: string;
  osVersion?: string;
  locale?: string;
  timezone?: string;
  replaySeed: string;
  context?: object;
}): Record<string, unknown> {
  return {
    observedAtMs: options.observedAtMs,
    ...(options.timezone ? { tz: options.timezone } : {}),
    runtime: 'react-native',
    ...(options.platform ? { os: options.platform } : {}),
    ...(options.osVersion ? { osVersion: options.osVersion } : {}),
    ...(options.locale ? { locale: options.locale } : {}),
    replaySeed: options.replaySeed,
    ...(options.context ? { context: options.context } : {}),
  };
}

/**
 * A 16 hex character seed. `Math.random` is the only entropy React Native
 * guarantees without a native module; the seed pins replay determinism, so a
 * cryptographic source would buy nothing here.
 */
export function replaySeed(): string {
  let seed = '';
  while (seed.length < 16) {
    seed += Math.floor(Math.random() * 0x100000000)
      .toString(16)
      .padStart(8, '0');
  }
  return seed.slice(0, 16);
}
