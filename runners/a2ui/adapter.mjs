import {createHash} from 'node:crypto';

const MESSAGE_KEYS = ['createSurface', 'updateComponents', 'updateDataModel', 'deleteSurface'];

export function canonicalJson(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
}

export function sha256(value) {
  const bytes = typeof value === 'string' ? value : canonicalJson(value);
  return createHash('sha256').update(bytes).digest('hex');
}

export function parseJsonl(text) {
  const messages = [];
  for (const [index, raw] of String(text).split(/\r?\n/).entries()) {
    if (!raw.trim()) continue;
    try {
      messages.push(JSON.parse(raw));
    } catch (error) {
      throw new Error(`invalid JSONL at line ${index + 1}: ${error.message}`);
    }
  }
  return messages;
}

function requiredString(value, name) {
  if (typeof value !== 'string' || !value) throw new Error(`${name} must be a non-empty string`);
  return value;
}

function cloneJson(value, path = '$', seen = new Set()) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new TypeError(`${path} must contain only finite JSON numbers`);
    return value;
  }
  if (value === undefined) return undefined;
  if (typeof value !== 'object') throw new TypeError(`${path} contains a non-JSON ${typeof value} value`);
  if (seen.has(value)) throw new TypeError(`${path} contains a cycle`);
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null && !Array.isArray(value)) {
    throw new TypeError(`${path} contains a non-JSON object`);
  }
  seen.add(value);
  try {
    if (Array.isArray(value)) {
      const result = [];
      for (let index = 0; index < value.length; index++) {
        if (!Object.hasOwn(value, index)) throw new TypeError(`${path}[${index}] is a sparse array entry`);
        const item = value[index];
        if (item === undefined) throw new TypeError(`${path}[${index}] is undefined`);
        result.push(cloneJson(item, `${path}[${index}]`, seen));
      }
      return result;
    }
    const result = {};
    for (const key of Object.keys(value)) {
      if (value[key] === undefined) throw new TypeError(`${path}.${key} is undefined`);
      result[key] = cloneJson(value[key], `${path}.${key}`, seen);
    }
    return result;
  } finally {
    seen.delete(value);
  }
}

function clone(value) {
  return value === undefined ? undefined : cloneJson(value);
}

function pointerParts(path) {
  if (path === undefined || path === '' || path === '/') return [];
  if (typeof path !== 'string' || !path.startsWith('/')) throw new Error(`invalid JSON pointer: ${String(path)}`);
  return path.slice(1).split('/').map(part => part.replace(/~1/g, '/').replace(/~0/g, '~'));
}

function pointerGet(root, path) {
  let value = root;
  for (const part of pointerParts(path)) {
    if (value === null || typeof value !== 'object' || !(part in value)) return undefined;
    value = value[part];
  }
  return value;
}

function pointerSet(root, path, value) {
  const parts = pointerParts(path);
  if (!parts.length) return clone(value);
  let next = root && typeof root === 'object' ? clone(root) : {};
  const result = next;
  for (let index = 0; index < parts.length - 1; index++) {
    const part = parts[index];
    const child = next[part];
    next[part] = child && typeof child === 'object' ? clone(child) : {};
    next = next[part];
  }
  next[parts.at(-1)] = clone(value);
  return result;
}

function oracleDescriptor(oracle) {
  if (!oracle || typeof oracle !== 'object') throw new Error('oracle must be an object');
  const descriptor = clone(oracle);
  delete descriptor.identity;
  delete descriptor.actual;
  delete descriptor.detail;
  return descriptor;
}

export function oracleIdentity(oracle) {
  return `a2ui:${sha256(oracleDescriptor(oracle))}`;
}

export function capture({protocolVersion, protocolDocument, catalog, stream, renderer, clientDataSnapshots = [], actions = [], oracle, agent = {}, observation}) {
  requiredString(protocolVersion, 'protocolVersion');
  if (!protocolDocument || typeof protocolDocument !== 'object' || Array.isArray(protocolDocument)) throw new Error('protocolDocument must be an object');
  if (!catalog || typeof catalog !== 'object') throw new Error('catalog must be an object');
  requiredString(catalog.id, 'catalog.id');
  if (!catalog.document || typeof catalog.document !== 'object') throw new Error('catalog.document must be an object');
  if (!Array.isArray(stream)) throw new Error('stream must be an array');
  if (!renderer || typeof renderer !== 'object') throw new Error('renderer must be an object');
  requiredString(renderer.name, 'renderer.name');
  requiredString(renderer.version, 'renderer.version');
  requiredString(renderer.platform, 'renderer.platform');
  const identity = oracleIdentity(oracle);
  if (oracle.identity && oracle.identity !== identity) throw new Error('oracle.identity does not match its structural descriptor');
  const normalizedObservation = observation ? clone(observation) : undefined;
  if (normalizedObservation?.oracleIdentity && normalizedObservation.oracleIdentity !== identity) {
    throw new Error('observation.oracleIdentity does not match the capture oracle');
  }
  if (normalizedObservation) normalizedObservation.oracleIdentity = identity;
  const ordered = stream.map((message, index) => ({sequence: index, message: clone(message)}));
  const catalogHash = sha256(catalog.document);
  const protocolHash = sha256(protocolDocument);
  const streamHash = sha256(ordered.map(item => item.message));
  const capsule = {
    format: 'reproit-a2ui-capture',
    formatVersion: 1,
    protocolVersion,
    protocolSha256: protocolHash,
    protocolDocument: clone(protocolDocument),
    catalog: {id: catalog.id, sha256: catalogHash, document: clone(catalog.document)},
    stream: ordered,
    streamSha256: streamHash,
    renderer: clone(renderer),
    clientDataSnapshots: clone(clientDataSnapshots),
    actions: clone(actions),
    agent: clone(agent),
    oracle: {...clone(oracle), identity},
    observation: normalizedObservation,
  };
  capsule.evidenceSha256 = sha256({
    protocolSha256: capsule.protocolSha256,
    catalogSha256: capsule.catalog.sha256,
    streamSha256: capsule.streamSha256,
    renderer: capsule.renderer,
    clientDataSnapshots: capsule.clientDataSnapshots,
    actions: capsule.actions,
    agent: capsule.agent,
    oracleIdentity: capsule.oracle.identity,
    observation: capsule.observation,
  });
  return capsule;
}

export function validateCapture(capsule) {
  const errors = [];
  if (capsule?.format !== 'reproit-a2ui-capture' || capsule?.formatVersion !== 1) errors.push('unsupported capture format');
  if (capsule?.protocolSha256 !== sha256(capsule?.protocolDocument)) errors.push('protocol hash mismatch');
  if (capsule?.catalog?.sha256 !== sha256(capsule?.catalog?.document)) errors.push('catalog hash mismatch');
  const messages = Array.isArray(capsule?.stream) ? capsule.stream.map(item => item.message) : [];
  if (capsule?.streamSha256 !== sha256(messages)) errors.push('stream hash mismatch');
  try {
    if (capsule?.oracle?.identity !== oracleIdentity(capsule?.oracle)) errors.push('oracle identity mismatch');
  } catch (error) {
    errors.push(error.message);
  }
  const evidenceSha256 = sha256({
    protocolSha256: capsule?.protocolSha256,
    catalogSha256: capsule?.catalog?.sha256,
    streamSha256: capsule?.streamSha256,
    renderer: capsule?.renderer,
    clientDataSnapshots: capsule?.clientDataSnapshots,
    actions: capsule?.actions,
    agent: capsule?.agent,
    oracleIdentity: capsule?.oracle?.identity,
    observation: capsule?.observation,
  });
  if (capsule?.evidenceSha256 !== evidenceSha256) errors.push('capture evidence hash mismatch');
  if (capsule?.observation && capsule.observation.oracleIdentity !== capsule?.oracle?.identity) {
    errors.push('observation oracle identity mismatch');
  }
  for (const [index, item] of (capsule?.stream || []).entries()) {
    if (item.sequence !== index) errors.push(`stream sequence is not contiguous at index ${index}`);
  }
  return errors;
}

function replayStream(capsule) {
  const errors = validateCapture(capsule);
  const surfaces = new Map();
  for (const item of capsule.stream || []) {
    const message = item.message;
    if (!message || typeof message !== 'object' || Array.isArray(message)) {
      errors.push(`message ${item.sequence}: envelope must be an object`);
      continue;
    }
    if (message.version !== capsule.protocolVersion) errors.push(`message ${item.sequence}: version does not match capture`);
    const keys = MESSAGE_KEYS.filter(key => Object.hasOwn(message, key));
    if (keys.length !== 1) {
      errors.push(`message ${item.sequence}: envelope must contain exactly one A2UI message`);
      continue;
    }
    const key = keys[0];
    const unexpected = Object.keys(message).filter(name => name !== 'version' && name !== key);
    if (unexpected.length) errors.push(`message ${item.sequence}: unexpected envelope properties: ${unexpected.join(', ')}`);
    const payload = message[key];
    if (!payload || typeof payload !== 'object' || Array.isArray(payload)) {
      errors.push(`message ${item.sequence}: ${key} must be an object`);
      continue;
    }
    const surfaceId = payload.surfaceId;
    if (typeof surfaceId !== 'string' || !surfaceId) {
      errors.push(`message ${item.sequence}: ${key}.surfaceId must be a non-empty string`);
      continue;
    }
    if (key === 'createSurface') {
      if (surfaces.has(surfaceId)) {
        errors.push(`message ${item.sequence}: surface ${surfaceId} already exists`);
        continue;
      }
      if (typeof payload.catalogId !== 'string' || !payload.catalogId) errors.push(`message ${item.sequence}: createSurface.catalogId is required`);
      if (payload.catalogId !== capsule.catalog.id) errors.push(`message ${item.sequence}: catalogId does not match captured catalog`);
      surfaces.set(surfaceId, {catalogId: payload.catalogId, sendDataModel: payload.sendDataModel === true, components: new Map(), data: {}});
      continue;
    }
    const surface = surfaces.get(surfaceId);
    if (!surface) {
      errors.push(`message ${item.sequence}: ${key} precedes createSurface for ${surfaceId}`);
      continue;
    }
    if (key === 'deleteSurface') {
      surfaces.delete(surfaceId);
    } else if (key === 'updateComponents') {
      if (!Array.isArray(payload.components)) {
        errors.push(`message ${item.sequence}: updateComponents.components must be an array`);
        continue;
      }
      for (const component of payload.components) {
        if (!component || typeof component !== 'object' || typeof component.id !== 'string' || !component.id) {
          errors.push(`message ${item.sequence}: every component requires an id`);
          continue;
        }
        surface.components.set(component.id, clone(component));
      }
    } else if (key === 'updateDataModel') {
      try {
        surface.data = pointerSet(surface.data, payload.path, payload.value);
      } catch (error) {
        errors.push(`message ${item.sequence}: ${error.message}`);
      }
    }
  }
  for (const [surfaceId, surface] of surfaces) {
    if (surface.components.size && !surface.components.has('root')) errors.push(`surface ${surfaceId}: component graph has no root`);
    for (const component of surface.components.values()) {
      const references = [];
      if (typeof component.child === 'string') references.push(component.child);
      if (Array.isArray(component.children)) references.push(...component.children.filter(value => typeof value === 'string'));
      for (const reference of references) if (!surface.components.has(reference)) errors.push(`surface ${surfaceId}: component ${component.id} references missing component ${reference}`);
    }
  }
  const state = Object.fromEntries([...surfaces].sort(([a], [b]) => a.localeCompare(b)).map(([surfaceId, surface]) => [surfaceId, {
    catalogId: surface.catalogId,
    sendDataModel: surface.sendDataModel,
    components: Object.fromEntries([...surface.components].sort(([a], [b]) => a.localeCompare(b))),
    data: surface.data,
  }]));
  return {errors: [...new Set(errors)], state};
}

function evaluateOracle(capsule, state) {
  const oracle = capsule.oracle;
  const surface = state[oracle.surfaceId];
  let actual;
  switch (oracle.kind) {
    case 'protocol-valid':
      return {matches: true, actual: true};
    case 'component-present':
      actual = Boolean(surface?.components?.[oracle.componentId]);
      break;
    case 'component-property':
      actual = pointerGet(surface?.components?.[oracle.componentId], oracle.path);
      break;
    case 'data-model-value':
      actual = pointerGet(surface?.data, oracle.path);
      break;
    case 'action-context': {
      const action = (capsule.actions || []).find(item => item.surfaceId === oracle.surfaceId && item.action?.name === oracle.actionName);
      actual = pointerGet(action?.action?.context, oracle.path);
      break;
    }
    default:
      throw new Error(`unsupported structural oracle kind: ${oracle.kind}`);
  }
  return {matches: canonicalJson(actual) === canonicalJson(oracle.expected), actual: clone(actual)};
}

export function replay(capsule) {
  const {errors, state} = replayStream(capsule);
  if (errors.length) {
    return {classification: 'protocol_invalidity', status: 'invalid', oracleIdentity: capsule?.oracle?.identity, protocolErrors: errors, state, stateSha256: sha256(state)};
  }
  const evaluated = evaluateOracle(capsule, state);
  const status = evaluated.matches ? 'pass' : 'fail';
  return {
    classification: status === 'fail' ? 'app_ui_failure' : 'pass',
    status,
    oracleIdentity: capsule.oracle.identity,
    actual: evaluated.actual,
    state,
    stateSha256: sha256(state),
  };
}

export function shrink(capsule) {
  const original = replay(capsule);
  if (original.status !== 'fail') throw new Error('shrink requires a valid failing capture');
  const identity = original.oracleIdentity;
  // A missing target and a wrong value can share the same structural selector,
  // but they are different violations. Preserve the observed value as well as
  // the oracle descriptor so deletion cannot "shrink" a mismatch into absence.
  const originalActual = canonicalJson(original.actual);
  let stream = capsule.stream.map(item => item.message);
  let granularity = 2;
  let replays = 0;
  while (stream.length >= 2) {
    const chunkSize = Math.ceil(stream.length / granularity);
    let reduced = false;
    for (let start = 0; start < stream.length; start += chunkSize) {
      const candidateMessages = stream.slice(0, start).concat(stream.slice(start + chunkSize));
      if (!candidateMessages.length) continue;
      const candidate = capture({
        protocolVersion: capsule.protocolVersion,
        protocolDocument: capsule.protocolDocument,
        catalog: {id: capsule.catalog.id, document: capsule.catalog.document},
        stream: candidateMessages,
        renderer: capsule.renderer,
        clientDataSnapshots: capsule.clientDataSnapshots,
        actions: capsule.actions,
        oracle: capsule.oracle,
        agent: capsule.agent,
      });
      const result = replay(candidate);
      replays++;
      if (result.status === 'fail' && result.oracleIdentity === identity && canonicalJson(result.actual) === originalActual) {
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
  const minimized = capture({
    protocolVersion: capsule.protocolVersion,
    protocolDocument: capsule.protocolDocument,
    catalog: {id: capsule.catalog.id, document: capsule.catalog.document},
    stream,
    renderer: capsule.renderer,
    clientDataSnapshots: capsule.clientDataSnapshots,
    actions: capsule.actions,
    oracle: capsule.oracle,
    agent: capsule.agent,
  });
  return {capsule: minimized, originalMessages: capsule.stream.length, minimizedMessages: stream.length, replays, oracleIdentity: identity};
}

function observationFor(capsule, result) {
  const observation = capsule.observation || {};
  return {
    status: observation.status || result.status,
    oracleIdentity: observation.oracleIdentity || result.oracleIdentity,
    structuralSha256: observation.structuralSha256 || result.stateSha256,
  };
}

export function rendererMatrix(capsules) {
  const runs = capsules.map(capsule => {
    const result = replay(capsule);
    return {renderer: capsule.renderer, streamSha256: capsule.streamSha256, agent: capsule.agent || {}, result, observation: observationFor(capsule, result)};
  });
  const protocolInvalidity = runs.filter(run => run.result.status === 'invalid');
  const agentGroups = new Map();
  for (const run of runs) {
    if (!run.agent.inputSha256) continue;
    const hashes = agentGroups.get(run.agent.inputSha256) || new Set();
    hashes.add(run.streamSha256);
    agentGroups.set(run.agent.inputSha256, hashes);
  }
  const agentNondeterminism = [...agentGroups].filter(([, hashes]) => hashes.size > 1).map(([inputSha256, hashes]) => ({inputSha256, streamSha256: [...hashes]}));
  const streamGroups = new Map();
  for (const run of runs.filter(item => item.result.status !== 'invalid')) {
    const key = `${run.streamSha256}|${run.observation.oracleIdentity}`;
    const group = streamGroups.get(key) || [];
    group.push(run);
    streamGroups.set(key, group);
  }
  const rendererDivergence = [];
  const appUiFailure = [];
  for (const group of streamGroups.values()) {
    if (group.length < 2) continue;
    const signatures = new Set(group.map(run => canonicalJson(run.observation)));
    if (signatures.size > 1) {
      rendererDivergence.push({streamSha256: group[0].streamSha256, renderers: group.map(run => ({renderer: run.renderer, observation: run.observation}))});
    } else if (group.every(run => run.observation.status === 'fail')) {
      appUiFailure.push({streamSha256: group[0].streamSha256, oracleIdentity: group[0].observation.oracleIdentity, renderers: group.map(run => run.renderer)});
    }
  }
  return {
    status: protocolInvalidity.length || agentNondeterminism.length || rendererDivergence.length || appUiFailure.length ? 'fail' : 'pass',
    protocolInvalidity: protocolInvalidity.map(run => ({renderer: run.renderer, errors: run.result.protocolErrors})),
    agentNondeterminism,
    rendererDivergence,
    appUiFailure,
    runs,
  };
}
