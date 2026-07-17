import { canonicalJson, sha256 } from './adapter.mjs';
import { instrumentA2ui, preflightA2ui } from './integration.mjs';

const A2UI_MESSAGE_KEYS = new Set([
  'createSurface',
  'updateComponents',
  'updateDataModel',
  'deleteSurface',
]);

const A2UI_MIME_TYPES = new Set(['application/a2ui+json', 'application/json+a2ui']);

const BASIC_CATALOG_SHA256 = '04870eba93a828a1959df8da3ae03a5d004839d4e7bd94e477c336f6e213e94a';
const SERVER_TO_CLIENT_SHA256 = 'a5feade7635f1e9ed199f37d88c8bdc985cd6f0d57d0933b28417b7d3ae0f6e1';

export const ADK_A2UI_SUPPORT = Object.freeze({
  upstreamRepository: 'https://github.com/google/A2UI',
  upstreamCommit: '96abfdc60de0657c6322028d10c1cc7bc25c237c',
  sample: 'samples/agent/adk/restaurant_finder',
  googleAdk: '>=1.28.1',
  a2aSdk: '>=0.3.0',
  a2aJsSdk: '0.3.13',
  a2uiWebCore: '0.10.4',
  protocolVersion: 'v0.9',
  mimeTypes: Object.freeze([...A2UI_MIME_TYPES]),
  basicCatalogSha256: BASIC_CATALOG_SHA256,
  protocolDocumentSha256: SERVER_TO_CLIENT_SHA256,
});

const BASIC_CATALOG_ID = 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json';

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function looksLikeA2uiMessage(value) {
  return isRecord(value) && Object.keys(value).some((key) => A2UI_MESSAGE_KEYS.has(key));
}

function eventParts(event) {
  if (!isRecord(event)) throw new TypeError('A2A event must be an object');
  if (event.kind === 'message') {
    if (!Array.isArray(event.parts)) throw new Error('A2A message.parts must be an array');
    return event.parts;
  }
  if (event.kind === 'status-update') {
    const message = event.status?.message;
    if (message === undefined || message === null) return [];
    if (!isRecord(message) || !Array.isArray(message.parts)) {
      throw new Error('A2A status-update status.message.parts must be an array');
    }
    return message.parts;
  }
  if (Object.hasOwn(event, 'parts') || event.status?.message?.parts !== undefined) {
    throw new Error(`unsupported A2A event envelope: ${String(event.kind)}`);
  }
  return [];
}

function partMimeType(part) {
  const direct = part.mimeType;
  const metadata = part.metadata?.mimeType;
  if (direct !== undefined && metadata !== undefined && direct !== metadata) {
    throw new Error('A2A part has conflicting mimeType metadata');
  }
  return direct ?? metadata;
}

/** Extract decoded A2UI messages from the raw A2A events emitted by the pinned
 * official ADK sample. Unknown lookalike envelopes and unmarked A2UI data fail
 * closed instead of being guessed.
 */
export function extractAdkA2uiMessages(event) {
  const messages = [];
  for (const [index, part] of eventParts(event).entries()) {
    if (!isRecord(part)) throw new Error(`A2A part ${index} must be an object`);
    const mimeType = partMimeType(part);
    const isA2ui = A2UI_MIME_TYPES.has(mimeType);
    if (isA2ui && part.kind !== 'data') {
      throw new Error(`A2A part ${index} declares A2UI MIME type but is not a data part`);
    }
    if (part.kind !== 'data') continue;
    if (isA2ui) {
      if (!isRecord(part.data))
        throw new Error(`A2A A2UI data part ${index} must contain one object`);
      messages.push(part.data);
      continue;
    }
    if (looksLikeA2uiMessage(part.data)) {
      throw new Error(`A2A data part ${index} looks like A2UI but has no supported A2UI MIME type`);
    }
  }
  return messages;
}

function createCumulativeExtractor() {
  const createBySurface = new Map();
  return (event) =>
    extractAdkA2uiMessages(event).filter((message) => {
      if (!isRecord(message.createSurface)) return true;
      const surfaceId = message.createSurface.surfaceId;
      if (typeof surfaceId !== 'string' || !surfaceId) return true;
      const canonical = canonicalJson(message);
      if (createBySurface.get(surfaceId) === canonical) return false;
      if (!createBySurface.has(surfaceId)) createBySurface.set(surfaceId, canonical);
      return true;
    });
}

/**
 * Validate with the official v0.9 web SDK when it is present in the host ADK
 * client. Absence or an incompatible SDK is an infrastructure error and throws,
 * so callers cannot accidentally treat an unvalidated stream as safe.
 */
export function validateWithOfficialWebSdk(sdk, messages, context) {
  if (context?.protocolVersion !== ADK_A2UI_SUPPORT.protocolVersion) {
    return [`unsupported ADK A2UI protocol version: ${String(context?.protocolVersion)}`];
  }
  if (!sdk.A2uiMessageListSchema || typeof sdk.A2uiMessageListSchema.safeParse !== 'function') {
    throw new Error(
      'installed @a2ui/web_core/v0_9 does not expose A2uiMessageListSchema.' + 'safeParse',
    );
  }
  const errors = [];
  if (
    !isRecord(context?.protocolDocument) ||
    sha256(context.protocolDocument) !== SERVER_TO_CLIENT_SHA256
  ) {
    errors.push(
      'automatic ADK validation requires the exact pinned v0.9 ' +
        'server_to_client protocol document; supply an official validator for a ' +
        'custom document',
    );
  }
  const result = sdk.A2uiMessageListSchema.safeParse(messages);
  if (!result.success) {
    errors.push(
      ...result.error.issues.map((issue) => ({
        path: issue.path.join('.'),
        code: issue.code,
        message: issue.message,
      })),
    );
  }
  const catalog = context?.catalog;
  if (
    catalog?.id !== BASIC_CATALOG_ID ||
    catalog?.document?.catalogId !== BASIC_CATALOG_ID ||
    sha256(catalog?.document) !== BASIC_CATALOG_SHA256
  ) {
    errors.push(
      'automatic ADK validation supports only the exact official v0.9 basic ' +
        'catalog; supply its official validator for a custom catalog',
    );
    return errors;
  }
  if (!isRecord(catalog.document.components)) {
    errors.push('official v0.9 basic catalog document is missing components');
    return errors;
  }
  if (!Array.isArray(sdk.BASIC_COMPONENTS)) {
    throw new Error('installed @a2ui/web_core/v0_9 does not expose BASIC_COMPONENTS');
  }
  const componentSchemas = new Map(sdk.BASIC_COMPONENTS.map((api) => [api.name, api.schema]));
  for (const [messageIndex, message] of messages.entries()) {
    for (const [componentIndex, component] of (
      message.updateComponents?.components ?? []
    ).entries()) {
      const type = component.component;
      const schema = componentSchemas.get(type);
      const path = `${messageIndex}.updateComponents.components.${componentIndex}`;
      if (!Object.hasOwn(catalog.document.components, type) || !schema) {
        errors.push({
          path,
          code: 'unknown_catalog_component',
          message: `component ${String(type)} is not in the pinned basic catalog`,
        });
        continue;
      }
      const { id: _id, component: _component, ...properties } = component;
      const componentResult = schema.safeParse(properties);
      if (!componentResult.success) {
        errors.push(
          ...componentResult.error.issues.map((issue) => ({
            path: [path, ...issue.path].join('.'),
            code: issue.code,
            message: issue.message,
          })),
        );
      }
    }
  }
  return errors;
}

export async function validateOfficialAdkA2uiMessages(messages, context) {
  if (context?.protocolVersion !== ADK_A2UI_SUPPORT.protocolVersion) {
    return [`unsupported ADK A2UI protocol version: ${String(context?.protocolVersion)}`];
  }
  const documentErrors = [];
  if (
    !isRecord(context?.protocolDocument) ||
    sha256(context.protocolDocument) !== SERVER_TO_CLIENT_SHA256
  ) {
    documentErrors.push(
      'automatic ADK validation requires the exact pinned v0.9 ' +
        'server_to_client protocol document; supply an official validator for a ' +
        'custom document',
    );
  }
  if (
    context?.catalog?.id !== BASIC_CATALOG_ID ||
    context?.catalog?.document?.catalogId !== BASIC_CATALOG_ID ||
    sha256(context?.catalog?.document) !== BASIC_CATALOG_SHA256
  ) {
    documentErrors.push(
      'automatic ADK validation supports only the exact official v0.9 basic ' +
        'catalog; supply its official validator for a custom catalog',
    );
  }
  if (documentErrors.length) return documentErrors;
  let sdk;
  try {
    sdk = await import('@a2ui/web_core/v0_9');
  } catch (error) {
    throw new Error(`official @a2ui/web_core v0.9 validator is unavailable: ${error.message}`);
  }
  return validateWithOfficialWebSdk(sdk, messages, context);
}

/** Observation-only integration for an official ADK -> A2A stream. Events are
 * passed through before the complete capture can be validated. Use
 * preflightAdkA2ui when delivery must be withheld until verification.
 */
export function instrumentAdkA2ui(source, options) {
  if (!options || typeof options !== 'object') throw new TypeError('ADK A2UI options are required');
  if (options.protocolVersion !== ADK_A2UI_SUPPORT.protocolVersion) {
    throw new Error(`ADK adapter supports only ${ADK_A2UI_SUPPORT.protocolVersion}`);
  }
  return instrumentA2ui(source, {
    ...options,
    extractMessages: createCumulativeExtractor(),
    validateMessages: options.validateMessages ?? validateOfficialAdkA2uiMessages,
  });
}

/** Buffer the complete official ADK -> A2A event stream, extract its decoded
 * A2UI messages, and deliver only the A2UI batch that passes hardened preflight.
 * Raw A2A events are never yielded or delivered by this API.
 */
export async function preflightAdkA2ui(source, options) {
  if (!options || typeof options !== 'object') throw new TypeError('ADK A2UI options are required');
  if (typeof options.deliver !== 'function')
    throw new TypeError('ADK A2UI preflight requires a deliver callback');
  if (options.protocolVersion !== ADK_A2UI_SUPPORT.protocolVersion) {
    throw new Error(`ADK adapter supports only ${ADK_A2UI_SUPPORT.protocolVersion}`);
  }
  const maxEvents = options.maxEvents ?? 10_000;
  const maxMessages = options.maxMessages ?? 10_000;
  if (!Number.isInteger(maxEvents) || maxEvents < 1 || maxEvents > 100_000) {
    throw new RangeError('maxEvents must be an integer from 1 to 100000');
  }
  if (!Number.isInteger(maxMessages) || maxMessages < 1 || maxMessages > 100_000) {
    throw new RangeError('maxMessages must be an integer from 1 to 100000');
  }
  const extract = createCumulativeExtractor();
  const messages = [];
  let events = 0;
  for await (const event of source) {
    if (events++ >= maxEvents) throw new Error(`ADK A2UI preflight exceeds ${maxEvents} events`);
    for (const message of extract(event)) {
      if (messages.length >= maxMessages)
        throw new Error(`A2UI preflight exceeds ${maxMessages} messages`);
      messages.push(structuredClone(message));
    }
  }
  return preflightA2ui(messages, {
    ...options,
    validateMessages: options.validateMessages ?? validateOfficialAdkA2uiMessages,
    release: options.deliver,
  });
}
