import { canonicalJson, capture, replay, shrink } from './adapter.mjs';

function asMessages(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

function oracleWithoutObservation(oracle) {
  const descriptor = { ...oracle };
  delete descriptor.actual;
  delete descriptor.detail;
  return descriptor;
}

export function exactRepairFeedback(capsule) {
  const result = replay(capsule);
  if (result.status !== 'fail') {
    throw new Error('exact repair feedback requires a valid failing capture');
  }
  const minimized = shrink(capsule);
  return {
    code: 'REPROIT_A2UI_FAILURE',
    oracle: oracleWithoutObservation(capsule.oracle),
    actual: result.actual,
    reproduction: {
      protocolVersion: capsule.protocolVersion,
      originalMessages: minimized.originalMessages,
      minimizedMessages: minimized.minimizedMessages,
      replayChecks: minimized.replays + 2,
      messages: minimized.capsule.stream.map((item) => item.message),
    },
  };
}

function recapture(capsule, messages) {
  return capture({
    protocolVersion: capsule.protocolVersion,
    protocolDocument: capsule.protocolDocument,
    catalog: { id: capsule.catalog.id, document: capsule.catalog.document },
    stream: messages,
    renderer: capsule.renderer,
    clientDataSnapshots: capsule.clientDataSnapshots,
    actions: capsule.actions,
    oracle: capsule.oracle,
    agent: capsule.agent,
  });
}

async function exactValidationErrors(capsule, validateMessages) {
  const validationErrors = await validateMessages(
    capsule.stream.map((item) => structuredClone(item.message)),
    {
      protocolVersion: capsule.protocolVersion,
      protocolDocument: structuredClone(capsule.protocolDocument),
      catalog: structuredClone(capsule.catalog),
    },
  );
  if (!Array.isArray(validationErrors)) {
    throw new TypeError('validateMessages must return an array of exact structural errors');
  }
  return validationErrors.map((error) =>
    typeof error === 'string' ? error : JSON.stringify(error),
  );
}

async function shrinkExactly(capsule, validateMessages) {
  const original = replay(capsule);
  if (original.status !== 'fail') throw new Error('exact shrink requires a valid failing capture');
  const originalValidation = await exactValidationErrors(capsule, validateMessages);
  if (originalValidation.length)
    throw new Error(`exact shrink received an invalid source: ${originalValidation.join('; ')}`);
  const identity = original.oracleIdentity;
  const originalActual = canonicalJson(original.actual);
  let stream = capsule.stream.map((item) => item.message);
  let granularity = 2;
  let replays = 0;
  while (stream.length >= 2) {
    const chunkSize = Math.ceil(stream.length / granularity);
    let reduced = false;
    for (let start = 0; start < stream.length; start += chunkSize) {
      const candidateMessages = stream.slice(0, start).concat(stream.slice(start + chunkSize));
      if (!candidateMessages.length) continue;
      const candidate = recapture(capsule, candidateMessages);
      replays++;
      if ((await exactValidationErrors(candidate, validateMessages)).length) continue;
      const result = replay(candidate);
      if (
        result.status === 'fail' &&
        result.oracleIdentity === identity &&
        canonicalJson(result.actual) === originalActual
      ) {
        stream = candidateMessages;
        reduced = true;
        granularity = Math.max(2, granularity - 1);
        break;
      }
    }
    if (!reduced) {
      if (granularity >= stream.length) break;
      granularity = Math.min(stream.length, granularity * 2);
    }
  }
  return {
    capsule: recapture(capsule, stream),
    originalMessages: capsule.stream.length,
    minimizedMessages: stream.length,
    replays,
    oracleIdentity: identity,
  };
}

async function exactValidatedRepairFeedback(capsule, validateMessages) {
  const result = replay(capsule);
  if (result.status !== 'fail')
    throw new Error('exact repair feedback requires a valid failing capture');
  const minimized = await shrinkExactly(capsule, validateMessages);
  return {
    code: 'REPROIT_A2UI_FAILURE',
    oracle: oracleWithoutObservation(capsule.oracle),
    actual: result.actual,
    reproduction: {
      protocolVersion: capsule.protocolVersion,
      originalMessages: minimized.originalMessages,
      minimizedMessages: minimized.minimizedMessages,
      replayChecks: minimized.replays + 2,
      messages: minimized.capsule.stream.map((item) => item.message),
    },
  };
}

export function createA2uiObserver(options) {
  if (!options || typeof options !== 'object')
    throw new TypeError('A2UI observer options are required');
  if (typeof options.validateMessages !== 'function') {
    throw new TypeError(
      'A2UI observer requires an exact protocol and catalog validateMessages ' + 'callback',
    );
  }
  const messages = [];
  let finished = false;
  return {
    message(message) {
      if (finished) throw new Error('A2UI observer already finished');
      messages.push(structuredClone(message));
    },
    async finish() {
      if (finished) throw new Error('A2UI observer already finished');
      finished = true;
      const evaluated = await evaluateMessages(messages, options);
      const capsule = evaluated.capsule;
      const replayResult = evaluated.replay;
      const evidence = {
        capsule,
        replay: replayResult,
        feedback:
          replayResult.status === 'fail'
            ? await exactValidatedRepairFeedback(capsule, options.validateMessages)
            : undefined,
      };
      await options.onResult?.(evidence);
      return evidence;
    },
  };
}

export class A2uiPreflightError extends Error {
  constructor(message, detail) {
    super(message);
    this.name = 'A2uiPreflightError';
    this.code = 'REPROIT_A2UI_PREFLIGHT_FAILED';
    this.detail = detail;
  }
}

async function collectMessages(source, maximum) {
  if (
    source == null ||
    (typeof source[Symbol.asyncIterator] !== 'function' &&
      typeof source[Symbol.iterator] !== 'function')
  ) {
    throw new TypeError('A2UI preflight source must be an iterable of decoded messages');
  }
  const messages = [];
  for await (const message of source) {
    if (messages.length >= maximum) throw new Error(`A2UI preflight exceeds ${maximum} messages`);
    messages.push(structuredClone(message));
  }
  return messages;
}

async function evaluateMessages(messages, options) {
  const capsule = capture({ ...options, stream: messages });
  const validationErrors = await exactValidationErrors(capsule, options.validateMessages);
  if (validationErrors.length) {
    return {
      capsule,
      replay: {
        classification: 'protocol_invalidity',
        status: 'invalid',
        oracleIdentity: capsule.oracle.identity,
        protocolErrors: validationErrors,
      },
    };
  }
  return { capsule, replay: replay(capsule) };
}

async function releaseVerified(evidence, release) {
  const verified = evidence.capsule.stream.map((item) => structuredClone(item.message));
  if (release) await release(verified, evidence);
  return { ...evidence, messages: verified };
}

/// Buffer a complete decoded A2UI stream, then release it atomically only after
/// structural replay passes. A failing stream may be replaced by a caller-owned
/// repair function, but every replacement is independently captured and replayed
/// against the original oracle before release.
export async function preflightA2ui(source, options) {
  if (!options || typeof options !== 'object')
    throw new TypeError('A2UI preflight options are required');
  if (typeof options.validateMessages !== 'function') {
    throw new TypeError(
      'A2UI preflight requires an exact protocol and catalog validateMessages ' + 'callback',
    );
  }
  const maxRepairs = options.maxRepairs ?? 2;
  const maxMessages = options.maxMessages ?? 10_000;
  if (!Number.isInteger(maxRepairs) || maxRepairs < 0 || maxRepairs > 10) {
    throw new RangeError('maxRepairs must be an integer from 0 to 10');
  }
  if (!Number.isInteger(maxMessages) || maxMessages < 1 || maxMessages > 100_000) {
    throw new RangeError('maxMessages must be an integer from 1 to 100000');
  }
  const original = await collectMessages(source, maxMessages);
  let current = await evaluateMessages(original, options);
  if (current.replay.status === 'pass') {
    return releaseVerified({ ...current, repairAttempts: 0 }, options.release);
  }
  if (current.replay.status !== 'fail') {
    throw new A2uiPreflightError('generated A2UI is structurally invalid', {
      repairAttempts: 0,
      lastReplay: current.replay,
    });
  }
  let feedback = await exactValidatedRepairFeedback(current.capsule, options.validateMessages);
  if (typeof options.repair !== 'function' || maxRepairs === 0) {
    throw new A2uiPreflightError(
      'generated A2UI failed preflight and no verified repair is available',
      {
        repairAttempts: 0,
        feedback,
        lastReplay: current.replay,
      },
    );
  }
  for (let attempt = 1; attempt <= maxRepairs; attempt++) {
    const candidateSource = await options.repair({
      attempt,
      feedback: structuredClone(feedback),
      messages: structuredClone(current.capsule.stream.map((item) => item.message)),
      oracleIdentity: current.capsule.oracle.identity,
    });
    let candidate;
    try {
      candidate = await collectMessages(candidateSource, maxMessages);
      current = await evaluateMessages(candidate, options);
    } catch (error) {
      if (attempt === maxRepairs) {
        throw new A2uiPreflightError('A2UI repair attempts were exhausted', {
          repairAttempts: attempt,
          feedback,
          candidateError: error.message,
        });
      }
      continue;
    }
    if (current.replay.status === 'pass') {
      return releaseVerified({ ...current, repairAttempts: attempt }, options.release);
    }
    if (current.replay.status === 'fail') {
      feedback = await exactValidatedRepairFeedback(current.capsule, options.validateMessages);
    }
    if (attempt === maxRepairs) {
      throw new A2uiPreflightError('A2UI repair attempts were exhausted', {
        repairAttempts: attempt,
        feedback,
        lastReplay: current.replay,
      });
    }
  }
  throw new Error('unreachable A2UI preflight state');
}

/// Observe an existing ADK, A2A, AG-UI, or GenUI async event stream without
/// changing its values or ordering. `extractMessages` is the only framework
/// seam: return zero, one, or many already-decoded A2UI envelopes for an event.
export function instrumentA2ui(source, options) {
  const observer = createA2uiObserver(options);
  let resolveResult;
  let rejectResult;
  const result = new Promise((resolve, reject) => {
    resolveResult = resolve;
    rejectResult = reject;
  });
  const events = (async function* () {
    let settled = false;
    try {
      for await (const event of source) {
        const extracted = options.extractMessages ? options.extractMessages(event) : event;
        for (const message of asMessages(extracted)) {
          observer.message(options.sanitizeMessage ? options.sanitizeMessage(message) : message);
        }
        yield event;
      }
      resolveResult(await observer.finish());
      settled = true;
    } catch (error) {
      rejectResult(error);
      settled = true;
      throw error;
    } finally {
      if (!settled)
        rejectResult(new Error('A2UI source was not fully consumed; capture is incomplete'));
    }
  })();
  return { events, result };
}
