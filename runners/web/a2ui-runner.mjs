#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { chromium } from 'playwright';
import { A2uiMessageListSchema, BASIC_COMPONENTS } from '@a2ui/web_core/v0_9';
import { zodToJsonSchema } from 'zod-to-json-schema';

const CATALOG_ID = 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json';
const MESSAGE_KEYS = ['createSurface', 'updateComponents', 'updateDataModel', 'deleteSurface'];
const INPUT_COMPONENTS = new Set([
  'TextField',
  'CheckBox',
  'ChoicePicker',
  'Slider',
  'DateTimeInput',
]);
const COMPONENT_MARKER = 'data-reproit-a2ui-component-id';
const SCOPE_MARKER = 'data-reproit-a2ui-scope';
const here = dirname(fileURLToPath(import.meta.url));

const componentSchemas = new Map(BASIC_COMPONENTS.map((api) => [api.name, api.schema]));
const messageListJsonSchema = zodToJsonSchema(A2uiMessageListSchema);
const messageJsonSchemas = new Map(
  (messageListJsonSchema.items?.anyOf ?? []).flatMap((schema) => {
    const operation = MESSAGE_KEYS.find((key) => schema.properties?.[key]);
    return operation ? [[operation, schema]] : [];
  }),
);
const componentJsonSchemas = new Map(
  BASIC_COMPONENTS.map((api) => {
    const properties = zodToJsonSchema(api.schema);
    return [
      api.name,
      {
        ...properties,
        properties: {
          id: { type: 'string', description: 'Stable component ID.' },
          component: { const: api.name },
          ...(properties.properties ?? {}),
        },
        required: [...new Set(['id', 'component', ...(properties.required ?? [])])],
        additionalProperties: false,
      },
    ];
  }),
);

export const A2UI_REPAIR_CONTRACT = Object.freeze({
  protocolVersion: 'v0.9',
  catalogId: CATALOG_ID,
  allowedComponents: BASIC_COMPONENTS.map((api) => api.name),
  streamRules: [
    'Return a JSON array of complete A2UI messages.',
    'Every message must use version v0.9 and contain exactly one operation.',
    'Component properties belong directly on the component object.',
    'Referenced children are component IDs, never inline component objects.',
    'Preserve IDs and unrelated messages unless the finding requires ' + 'changing them.',
  ],
  prohibitedProperties: ['ariaLabel', 'componentProperties'],
  validation: {
    command: 'reproit --json scan <stream.json>',
    success: 'exit code 0 with an empty findings array',
  },
});

function canonical(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  return `{${Object.keys(value)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
    .join(',')}}`;
}

function sha256(value) {
  return createHash('sha256')
    .update(typeof value === 'string' ? value : canonical(value))
    .digest('hex');
}

function parseArgs(args) {
  const config = { runs: 3, seed: 1 };
  config.command = args.shift();
  config.target = args.shift();
  while (args.length) {
    const flag = args.shift();
    if (flag === '--output') config.output = args.shift();
    else if (flag === '--runs') config.runs = Number(args.shift());
    else if (flag === '--seed') config.seed = Number(args.shift());
    else if (flag === '--expect') config.expect = args.shift();
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!['scan', 'fuzz', 'replay'].includes(config.command) || !config.target) {
    throw new Error(
      'usage: a2ui-runner.mjs scan|fuzz|replay <stream-or-finding.json> ' +
        '[--output report.json] [--runs N] [--seed N]',
    );
  }
  if (!Number.isInteger(config.runs) || config.runs < 1 || config.runs > 100)
    throw new Error('--runs must be 1..100');
  if (!Number.isSafeInteger(config.seed) || config.seed < 0)
    throw new Error('--seed must be a non-negative integer');
  return config;
}

export function parseA2uiText(text) {
  const trimmed = text.trim();
  if (!trimmed) throw new Error('A2UI target is empty');
  try {
    const document = JSON.parse(trimmed);
    const messages = Array.isArray(document) ? document : document.messages;
    if (!Array.isArray(messages))
      throw new Error('JSON target must be an array or contain a messages array');
    return { messages, document };
  } catch (jsonError) {
    const messages = trimmed
      .split(/\r?\n/)
      .filter(Boolean)
      .map((line, index) => {
        try {
          return JSON.parse(line);
        } catch (error) {
          throw new Error(`invalid JSONL at line ${index + 1}: ${error.message}`);
        }
      });
    return { messages, document: { messages }, jsonError: jsonError.message };
  }
}

export function validateMessages(messages) {
  const errors = [];
  const list = A2uiMessageListSchema.safeParse(messages);
  if (!list.success) {
    errors.push(
      ...list.error.issues.map((issue) => ({
        kind: 'protocol-invalid',
        path: issue.path.join('.'),
        reason: issue.message,
      })),
    );
  }
  for (const [messageIndex, message] of messages.entries()) {
    if (!message || typeof message !== 'object' || Array.isArray(message)) continue;
    const keys = MESSAGE_KEYS.filter((key) => Object.hasOwn(message, key));
    if (keys.length !== 1)
      errors.push({
        kind: 'protocol-invalid',
        path: String(messageIndex),
        reason: 'message must contain exactly one A2UI operation',
      });
    if (message.version !== 'v0.9')
      errors.push({
        kind: 'protocol-invalid',
        path: `${messageIndex}.version`,
        reason: 'only A2UI v0.9 is supported',
      });
    if (
      message.createSurface?.catalogId !== undefined &&
      message.createSurface.catalogId !== CATALOG_ID
    ) {
      errors.push({
        kind: 'protocol-invalid',
        path: `${messageIndex}.createSurface.catalogId`,
        reason: 'automatic scan supports the official v0.9 basic catalog',
      });
    }
    const components = message.updateComponents?.components;
    if (!Array.isArray(components)) continue;
    for (const [componentIndex, component] of components.entries()) {
      const schema = componentSchemas.get(component.component);
      const path = `${messageIndex}.updateComponents.components.${componentIndex}`;
      if (!schema) {
        errors.push({
          kind: 'protocol-invalid',
          path,
          reason: `unknown basic-catalog component ${String(component.component)}`,
        });
        continue;
      }
      const { id: _id, component: _component, ...properties } = component;
      const parsed = schema.safeParse(properties);
      if (!parsed.success)
        errors.push(
          ...parsed.error.issues.map((issue) => ({
            kind: 'protocol-invalid',
            path: [path, ...issue.path].join('.'),
            reason: issue.message,
          })),
        );
    }
  }
  const liveSurfaces = new Set();
  for (const [messageIndex, message] of messages.entries()) {
    const create = message?.createSurface;
    if (create && typeof create.surfaceId === 'string') {
      if (liveSurfaces.has(create.surfaceId))
        errors.push({
          kind: 'protocol-invalid',
          path: `${messageIndex}.createSurface.surfaceId`,
          reason: `surface ${create.surfaceId} is created while it is already live`,
          proofStatus: 'VIOLATION',
        });
      else liveSurfaces.add(create.surfaceId);
      continue;
    }
    const update = message?.updateComponents ?? message?.updateDataModel;
    if (update && typeof update.surfaceId === 'string' && !liveSurfaces.has(update.surfaceId)) {
      const operation = message.updateComponents ? 'updateComponents' : 'updateDataModel';
      errors.push({
        kind: 'protocol-invalid',
        path: `${messageIndex}.${operation}.surfaceId`,
        reason:
          `${operation} targets surface ${update.surfaceId} before createSurface ` +
          'or after deleteSurface',
        proofStatus: 'VIOLATION',
      });
      continue;
    }
    const deleted = message?.deleteSurface?.surfaceId;
    if (typeof deleted === 'string') liveSurfaces.delete(deleted);
  }
  if (list.success) errors.push(...negotiatedConformance(messages).errors);
  return errors;
}

function pointerParts(path) {
  if (path === undefined || path === '' || path === '/') return [];
  if (typeof path !== 'string' || !path.startsWith('/')) return undefined;
  return path
    .slice(1)
    .split('/')
    .map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'));
}

function pointerGet(root, path) {
  const parts = pointerParts(path);
  if (!parts) return undefined;
  let value = root;
  for (const part of parts) {
    if (value === null || typeof value !== 'object' || !Object.hasOwn(value, part))
      return undefined;
    value = value[part];
  }
  return value;
}

function pointerSet(root, path, value) {
  const parts = pointerParts(path);
  if (!parts) return root;
  if (!parts.length) return structuredClone(value);
  const result = root && typeof root === 'object' ? structuredClone(root) : {};
  let cursor = result;
  for (const part of parts.slice(0, -1)) {
    const child = cursor[part];
    cursor[part] = child && typeof child === 'object' ? structuredClone(child) : {};
    cursor = cursor[part];
  }
  cursor[parts.at(-1)] = structuredClone(value);
  return result;
}

function finalSurfaces(messages) {
  const surfaces = new Map();
  for (const message of messages) {
    const create = message.createSurface;
    if (create) {
      surfaces.set(create.surfaceId, {
        catalogId: create.catalogId,
        theme: structuredClone(create.theme ?? {}),
        sendDataModel: create.sendDataModel ?? false,
        components: new Map(),
        data: {},
      });
      continue;
    }
    const update = message.updateComponents;
    if (update) {
      const surface = surfaces.get(update.surfaceId);
      if (!surface) continue;
      for (const component of update.components ?? [])
        surface.components.set(component.id, structuredClone(component));
      continue;
    }
    const data = message.updateDataModel;
    if (data) {
      const surface = surfaces.get(data.surfaceId);
      if (surface) surface.data = pointerSet(surface.data, data.path, data.value);
      continue;
    }
    if (message.deleteSurface) surfaces.delete(message.deleteSurface.surfaceId);
  }
  return surfaces;
}

function canonicalStateFromMessages(messages) {
  return [...finalSurfaces(messages)]
    .map(([id, surface]) => ({
      id,
      catalogId: surface.catalogId,
      theme: structuredClone(surface.theme),
      sendDataModel: surface.sendDataModel,
      data: structuredClone(surface.data),
      components: [...surface.components.values()]
        .map((component) => {
          const { id: componentId, component: type, ...properties } = component;
          return { id: componentId, type, ...structuredClone(properties) };
        })
        .sort((left, right) => left.id.localeCompare(right.id)),
    }))
    .sort((left, right) => left.id.localeCompare(right.id));
}

function normalizedRuntimeState(state) {
  return structuredClone(state ?? [])
    .map((surface) => ({
      ...surface,
      components: [...(surface.components ?? [])].sort((left, right) =>
        left.id.localeCompare(right.id),
      ),
    }))
    .sort((left, right) => left.id.localeCompare(right.id));
}

function firstDifference(expected, actual, path = '') {
  if (canonical(expected) === canonical(actual)) return undefined;
  if (
    expected === null ||
    actual === null ||
    typeof expected !== 'object' ||
    typeof actual !== 'object'
  ) {
    return { path: path || '/', expected, actual };
  }
  if (Array.isArray(expected) !== Array.isArray(actual))
    return { path: path || '/', expected, actual };
  const keys =
    Array.isArray(expected) && Array.isArray(actual)
      ? Array.from({ length: Math.max(expected.length, actual.length) }, (_, index) =>
          String(index),
        )
      : [...new Set([...Object.keys(expected), ...Object.keys(actual)])].sort();
  for (const key of keys) {
    if (!Object.hasOwn(expected, key) || !Object.hasOwn(actual, key)) {
      return { path: `${path}/${key}`, expected: expected[key], actual: actual[key] };
    }
    const child = firstDifference(expected[key], actual[key], `${path}/${key}`);
    if (child) return child;
  }
  return { path: path || '/', expected, actual };
}

function bindingLeaves(value, path = []) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
  const keys = Object.keys(value);
  if (
    keys.length === 1 &&
    keys[0] === 'path' &&
    typeof value.path === 'string' &&
    value.path.length
  ) {
    return [
      {
        bindingPath: value.path,
        contextPath: `/${path
          .map((part) => String(part).replace(/~/g, '~0').replace(/\//g, '~1'))
          .join('/')}`,
      },
    ];
  }
  return Object.entries(value).flatMap(([key, child]) => bindingLeaves(child, [...path, key]));
}

function exactBinding(value) {
  return value &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1 &&
    typeof value.path === 'string'
    ? value.path
    : undefined;
}

function absoluteBindingPath(scopePath, bindingPath) {
  if (bindingPath.startsWith('/')) return bindingPath;
  const base = scopePath === '/' ? '' : scopePath.replace(/\/$/, '');
  return `${base}/${bindingPath}`;
}

function staticChildIds(component) {
  const result = [];
  if (typeof component.child === 'string') result.push(component.child);
  if (Array.isArray(component.children))
    result.push(...component.children.filter((child) => typeof child === 'string'));
  return result;
}

function descendants(components, rootId) {
  const seen = new Set();
  const visit = (id) => {
    if (seen.has(id)) return;
    seen.add(id);
    const component = components.get(id);
    if (component) for (const child of staticChildIds(component)) visit(child);
  };
  visit(rootId);
  return seen;
}

function componentScopes(surface) {
  const dynamic = new Map();
  for (const component of surface.components.values()) {
    const template =
      component.component === 'List' && component.children && !Array.isArray(component.children)
        ? component.children
        : undefined;
    if (
      !template ||
      typeof template.componentId !== 'string' ||
      typeof template.path !== 'string' ||
      !template.path.startsWith('/')
    )
      continue;
    const items = pointerGet(surface.data, template.path);
    if (!Array.isArray(items)) continue;
    const members = descendants(surface.components, template.componentId);
    for (const member of members) {
      const scopes = dynamic.get(member) ?? [];
      for (let index = 0; index < items.length; index++)
        scopes.push(`${template.path.replace(/\/$/, '')}/${index}`);
      dynamic.set(member, scopes);
    }
  }
  return dynamic;
}

const TYPED_DYNAMIC_PROPERTIES = Object.freeze({
  Text: { text: 'string' },
  Image: { url: 'string' },
  Video: { url: 'string' },
  AudioPlayer: { url: 'string' },
  TextField: { label: 'string', value: 'string' },
  CheckBox: { label: 'string', value: 'boolean' },
  ChoicePicker: { label: 'string', value: 'string-array' },
  Slider: { label: 'string', value: 'number' },
  DateTimeInput: { label: 'string', value: 'string' },
});

function matchesProofType(value, type) {
  if (type === 'string-array')
    return Array.isArray(value) && value.every((item) => typeof item === 'string');
  return typeof value === type;
}

export function negotiatedConformance(messages) {
  const claims = [];
  const errors = [];
  for (const [surfaceId, surface] of finalSurfaces(messages)) {
    const dynamicScopes = componentScopes(surface);
    for (const component of surface.components.values()) {
      const jsonSchema = componentJsonSchemas.get(component.component);
      for (const [property, propertySchema] of Object.entries(jsonSchema?.properties ?? {})) {
        if (
          property === 'id' ||
          property === 'component' ||
          Object.hasOwn(component, property) ||
          !Object.hasOwn(propertySchema, 'default')
        )
          continue;
        claims.push({
          subject: `${surfaceId}/${component.id}.${property}`,
          status: 'SATISFIED',
          reason: `official schema default is ${canonical(propertySchema.default)}`,
        });
      }
      const propertyTypes = TYPED_DYNAMIC_PROPERTIES[component.component] ?? {};
      const record = componentRecords(
        messages,
        (candidate) => candidate.id === component.id && candidate.component === component.component,
      ).at(-1);
      for (const [property, expectedType] of Object.entries(propertyTypes)) {
        const value = component[property];
        if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
        const subject = `${surfaceId}/${component.id}.${property}`;
        if ('call' in value) {
          claims.push({
            subject,
            status: 'ABSTAIN',
            reason: 'function result depends on client catalog behavior',
          });
          continue;
        }
        const bindingPath = exactBinding(value);
        if (bindingPath === undefined) continue;
        const scoped = dynamicScopes.get(component.id) ?? [];
        const scopePaths = bindingPath.startsWith('/')
          ? scoped.length
            ? []
            : ['/']
          : scoped.length
            ? scoped
            : ['/'];
        if (!scopePaths.length) {
          claims.push({
            subject,
            status: 'ABSTAIN',
            reason: 'absolute binding is repeated by a dynamic template',
          });
          continue;
        }
        for (const scopePath of scopePaths) {
          const resolvedPath = absoluteBindingPath(scopePath, bindingPath);
          const actual = pointerGet(surface.data, resolvedPath);
          const scopedSubject = scopePath === '/' ? subject : `${subject}@${scopePath}`;
          if (actual === undefined) {
            claims.push({
              subject: scopedSubject,
              status: 'ABSTAIN',
              reason: `binding ${resolvedPath} has no final value`,
            });
          } else if (matchesProofType(actual, expectedType)) {
            claims.push({
              subject: scopedSubject,
              status: 'SATISFIED',
              reason: `binding resolves to ${expectedType}`,
            });
          } else {
            const actualType = Array.isArray(actual) ? 'array' : typeof actual;
            const reason =
              `${component.component}.${property} binding ${resolvedPath} resolves to ` +
              `${actualType}, expected ${expectedType}`;
            claims.push({ subject: scopedSubject, status: 'VIOLATION', reason });
            errors.push({
              kind: 'protocol-invalid',
              path: `${record?.path ?? component.id}.${property}.path`,
              reason,
              proofStatus: 'VIOLATION',
              oracle: {
                surfaceId,
                componentId: component.id,
                property,
                resolvedPath,
                expectedType,
              },
              actual,
            });
          }
        }
      }
      if (component.component === 'Button' && component.action?.functionCall) {
        claims.push({
          subject: `${surfaceId}/${component.id}.action`,
          status: 'ABSTAIN',
          reason: 'local function action is catalog-defined external behavior',
        });
      } else if (component.component === 'Button' && component.action?.event) {
        claims.push({
          subject: `${surfaceId}/${component.id}.action`,
          status: 'SATISFIED',
          reason: 'event action matches the official action schema',
        });
      }
    }
  }
  return {
    status: claims.some((claim) => claim.status === 'VIOLATION')
      ? 'VIOLATION'
      : claims.some((claim) => claim.status === 'ABSTAIN')
        ? 'ABSTAIN'
        : 'SATISFIED',
    claims,
    errors,
  };
}

function deterministicControl(component, initialValue, descriptor) {
  if (component.checks?.length) return undefined;
  switch (component.component) {
    case 'TextField': {
      if (
        component.variant === 'number' ||
        component.validationRegexp !== undefined ||
        typeof initialValue !== 'string'
      )
        return undefined;
      return {
        initialValue,
        renderedInitialValue: initialValue,
        sentinel: `reproit+${sha256(descriptor).slice(0, 16)}@example.test`,
      };
    }
    case 'CheckBox':
      return typeof initialValue === 'boolean'
        ? { initialValue, renderedInitialValue: initialValue, sentinel: !initialValue }
        : undefined;
    case 'ChoicePicker': {
      if (!Array.isArray(initialValue) || !initialValue.every((value) => typeof value === 'string'))
        return undefined;
      const options = component.options?.map((option) => option?.value);
      if (
        !options?.length ||
        options.some((value) => typeof value !== 'string') ||
        new Set(options).size !== options.length
      )
        return undefined;
      if (initialValue.some((value) => !options.includes(value))) return undefined;
      let optionIndex;
      let sentinel;
      if ((component.variant ?? 'mutuallyExclusive') === 'mutuallyExclusive') {
        optionIndex = options.findIndex(
          (value) => initialValue.length !== 1 || value !== initialValue[0],
        );
        if (optionIndex < 0) return undefined;
        sentinel = [options[optionIndex]];
      } else {
        optionIndex = 0;
        sentinel = initialValue.includes(options[0])
          ? initialValue.filter((value) => value !== options[0])
          : [...initialValue, options[0]];
      }
      return {
        initialValue: structuredClone(initialValue),
        renderedInitialValue: structuredClone(initialValue),
        sentinel,
        optionIndex,
        options,
      };
    }
    case 'Slider': {
      const min = component.min ?? 0;
      const max = component.max;
      const step = component.step ?? 1;
      if (
        ![initialValue, min, max, step].every(Number.isFinite) ||
        step <= 0 ||
        min >= max ||
        initialValue < min ||
        initialValue > max
      )
        return undefined;
      const candidates = [min, Math.min(max, min + step), max].filter(
        (value) => value !== initialValue && value >= min && value <= max,
      );
      if (!candidates.length) return undefined;
      return { initialValue, renderedInitialValue: initialValue, sentinel: candidates[0] };
    }
    case 'DateTimeInput': {
      if (
        typeof initialValue !== 'string' ||
        component.min !== undefined ||
        component.max !== undefined
      )
        return undefined;
      const mode =
        component.enableDate && component.enableTime
          ? 'datetime-local'
          : component.enableDate
            ? 'date'
            : component.enableTime
              ? 'time'
              : undefined;
      if (!mode) return undefined;
      const normalize = (value) =>
        mode === 'date'
          ? value.split('T')[0]?.slice(0, 10)
          : mode === 'time'
            ? (value.includes('T') ? value.split('T')[1] : value).slice(0, 5)
            : `${value.split('T')[0]?.slice(0, 10)}T${(value.split('T')[1] ?? '').slice(0, 5)}`;
      const sentinel =
        mode === 'date' ? '2031-02-03' : mode === 'time' ? '13:37' : '2031-02-03T13:37';
      const renderedInitialValue = normalize(initialValue);
      if (!renderedInitialValue || renderedInitialValue === sentinel) return undefined;
      return { initialValue, renderedInitialValue, sentinel, inputMode: mode };
    }
  }
}

export function boundActionContracts(messages) {
  const contracts = [];
  for (const [surfaceId, surface] of finalSurfaces(messages)) {
    const dynamicScopes = componentScopes(surface);
    const controls = [...surface.components.values()].filter(
      (component) =>
        INPUT_COMPONENTS.has(component.component) && exactBinding(component.value) !== undefined,
    );
    const buttons = [...surface.components.values()].filter((component) => {
      const event = component.component === 'Button' && component.action?.event;
      return (
        event &&
        typeof event.name === 'string' &&
        event.name &&
        event.context &&
        typeof event.context === 'object'
      );
    });
    for (const control of controls) {
      const bindingPath = exactBinding(control.value);
      const scoped = dynamicScopes.get(control.id) ?? [];
      const scopePaths = bindingPath.startsWith('/')
        ? scoped.length
          ? []
          : ['/']
        : scoped.length
          ? scoped
          : ['/'];
      for (const scopePath of scopePaths) {
        const resolvedBindingPath = absoluteBindingPath(scopePath, bindingPath);
        const initialValue = pointerGet(surface.data, resolvedBindingPath);
        for (const button of buttons) {
          const buttonScopes = dynamicScopes.get(button.id) ?? [];
          if (scopePath !== '/' && !buttonScopes.includes(scopePath)) continue;
          if (scopePath === '/' && buttonScopes.length) continue;
          for (const binding of bindingLeaves(button.action.event.context)) {
            if (absoluteBindingPath(scopePath, binding.bindingPath) !== resolvedBindingPath)
              continue;
            const descriptor = {
              surfaceId,
              controlId: control.id,
              controlType: control.component,
              bindingPath,
              resolvedBindingPath,
              scopePath,
              buttonId: button.id,
              actionName: button.action.event.name,
              contextPath: binding.contextPath,
            };
            const typed = deterministicControl(control, initialValue, descriptor);
            if (typed) contracts.push({ ...descriptor, ...typed });
          }
        }
      }
    }
  }
  return contracts.sort((a, b) => canonical(a).localeCompare(canonical(b)));
}

function componentRecords(messages, predicate = () => true) {
  const records = [];
  for (const [messageIndex, message] of messages.entries()) {
    const components = message?.updateComponents?.components;
    if (!Array.isArray(components)) continue;
    for (const [componentIndex, component] of components.entries()) {
      if (!component || typeof component !== 'object' || !predicate(component)) continue;
      records.push({
        path: `${messageIndex}.updateComponents.components.${componentIndex}`,
        messageIndex,
        componentIndex,
        id: component.id,
        type: component.component,
        value: component,
      });
    }
  }
  return records;
}

function recordForPath(messages, path) {
  const match = /^(\d+)\.updateComponents\.components\.(\d+)(?:\.|$)/.exec(path ?? '');
  if (!match) return undefined;
  const messageIndex = Number(match[1]);
  const componentIndex = Number(match[2]);
  const value = messages[messageIndex]?.updateComponents?.components?.[componentIndex];
  if (!value || typeof value !== 'object') return undefined;
  return {
    path: `${messageIndex}.updateComponents.components.${componentIndex}`,
    messageIndex,
    componentIndex,
    id: value.id,
    type: value.component,
    value,
  };
}

function schemaContext(record) {
  const schema = componentJsonSchemas.get(record?.type);
  if (!schema) return undefined;
  return {
    path: record.path,
    id: record.id,
    type: record.type,
    allowedProperties: Object.keys(schema.properties ?? {}),
    requiredProperties: schema.required ?? [],
    schema,
  };
}

function messageContextForPath(messages, path) {
  const match = /^(\d+)(?:\.([^\.]+))?/.exec(path ?? '');
  if (!match) return undefined;
  const index = Number(match[1]);
  const value = messages[index];
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const operation = MESSAGE_KEYS.find((key) => Object.hasOwn(value, key));
  const schema = messageJsonSchemas.get(operation);
  const operationSchema = schema?.properties?.[operation];
  return {
    path: String(index),
    operation,
    operationPath: operation ? `${index}.${operation}` : String(index),
    allowedProperties: Object.keys(schema?.properties ?? {}),
    requiredProperties: schema?.required ?? [],
    operationAllowedProperties: Object.keys(operationSchema?.properties ?? {}),
    operationRequiredProperties: operationSchema?.required ?? [],
    schema,
  };
}

function legacyWrappedComponent(record) {
  const wrapped = record?.value?.component;
  if (!wrapped || typeof wrapped !== 'object' || Array.isArray(wrapped)) return undefined;
  const entries = Object.entries(wrapped);
  if (entries.length !== 1) return undefined;
  const [type, properties] = entries[0];
  if (
    !componentSchemas.has(type) ||
    !properties ||
    typeof properties !== 'object' ||
    Array.isArray(properties)
  )
    return undefined;
  const value = { id: record.id, component: type, ...structuredClone(properties) };
  return {
    detectedShape: 'legacy-wrapped-component',
    originalType: type,
    replacement: value,
    normalizedRecord: { ...record, type, value },
  };
}

function protocolRepairContext(messages, item) {
  const record = recordForPath(messages, item.path);
  const legacy = legacyWrappedComponent(record);
  const component = schemaContext(legacy?.normalizedRecord ?? record);
  const message = messageContextForPath(messages, item.path);
  return {
    objective: legacy
      ? 'Convert this legacy wrapped component to the flat A2UI v0.9 ' +
        'basic-catalog component shape.'
      : 'Make the smallest schema-valid edit that removes this exact finding.',
    repairability: 'message-edit',
    editScope: component?.path ?? message?.operationPath ?? item.path,
    component,
    message,
    oracle: item.oracle ? structuredClone(item.oracle) : undefined,
    detectedShape: legacy?.detectedShape,
    validPatchExamples: legacy
      ? [
          {
            path: record.path,
            operation: 'replace-component',
            value: legacy.replacement,
          },
        ]
      : component
        ? [
            {
              path: component.path,
              operation: 'replace-component',
              valueMustMatch: 'repairContext.component.schema',
            },
          ]
        : message?.operation
          ? [
              {
                path: message.operationPath,
                operation: 'replace-operation',
                valueMustMatch:
                  'the operation schema referenced by repairContext.message.schemaRef',
              },
            ]
          : [],
    revalidateAfterEdit: true,
  };
}

function accessibilityRepairContext(messages, item) {
  const records =
    item.kind === 'unlabeled-button'
      ? componentRecords(messages, (component) => component.component === 'Button')
      : componentRecords(messages, (component) => INPUT_COMPONENTS.has(component.component));
  const observationIndex = item.inputIndex ?? item.buttonIndex ?? 0;
  const selected = records[observationIndex] ?? records[0];
  const rendererOwnedTextField =
    item.kind === 'unlabeled-input' && item.renderer === 'lit' && selected?.type === 'TextField';
  if (rendererOwnedTextField) {
    return {
      objective: 'Preserve this schema-valid stream and repair or upgrade the Lit ' + 'renderer.',
      repairability: 'renderer-change-required',
      owner: '@a2ui/lit',
      editScope: 'renderer implementation, not the A2UI message stream',
      component: schemaContext(selected),
      candidateComponents: records.map((record) => ({
        path: record.path,
        id: record.id,
        type: record.type,
      })),
      validPatchExamples: [],
      notes: [
        'The official label and accessibility.label properties are schema-valid ' +
          'but do not give this Lit-rendered TextField an accessible name.',
        'Do not invent ariaLabel or another message property.',
        'A message-only repair has not been verified. Keep the minimized ' +
          'reproduction for the renderer fix.',
      ],
      revalidateAfterEdit: true,
    };
  }
  return {
    objective:
      'Give the rendered control an accessible name using the official ' +
      'basic-catalog accessibility object.',
    repairability: 'message-edit',
    editScope: selected?.path ?? 'the corresponding visible control component',
    component: schemaContext(selected),
    candidateComponents: records.map((record) => ({
      path: record.path,
      id: record.id,
      type: record.type,
    })),
    validPatchExamples: selected
      ? [
          {
            path: selected.path,
            operation: 'merge-component-properties',
            value: { accessibility: { label: 'Descriptive accessible name' } },
          },
        ]
      : [],
    notes: [
      'Use accessibility.label. Do not invent ariaLabel.',
      'Keep the visible label or child component unless the requested UI ' +
        'requires changing it.',
    ],
    revalidateAfterEdit: true,
  };
}

function attachRepairContext(messages, item) {
  if (item.kind === 'protocol-invalid') {
    return { ...item, repairContext: protocolRepairContext(messages, item) };
  }
  if (item.kind === 'unlabeled-input' || item.kind === 'unlabeled-button') {
    return { ...item, repairContext: accessibilityRepairContext(messages, item) };
  }
  if (item.kind === 'stream-convergence' || item.kind === 'default-conformance') {
    return {
      ...item,
      repairContext: {
        objective:
          item.kind === 'default-conformance'
            ? 'Make the official renderers resolve the negotiated schema default to ' +
              'the same observable behavior.'
            : 'Make the official renderer converge to the same canonical surface and ' +
              'model under equivalent update streams.',
        repairability: 'renderer-change-required',
        owner:
          item.renderer === 'react'
            ? '@a2ui/react'
            : item.renderer === 'lit'
              ? '@a2ui/lit'
              : 'official renderer integration',
        editScope:
          'message processing, binding, or renderer update handling, not the ' +
          'schema-valid reproduction stream',
        oracle: structuredClone(item.oracle),
        validPatchExamples: [],
        revalidateAfterEdit: true,
      },
    };
  }
  return {
    ...item,
    repairContext: {
      objective:
        'Preserve the minimized reproduction while removing this exact renderer ' + 'finding.',
      repairability: 'unknown',
      editScope: 'minimalMessages',
      revalidateAfterEdit: true,
    },
  };
}

function validationReport(messages) {
  return validateMessages(messages).map((item) =>
    attachRepairContext(
      messages,
      finding(item.kind, 'protocol', item.reason, {
        path: item.path,
        proofStatus: item.proofStatus ?? 'VIOLATION',
        oracle: item.oracle,
        actual: item.actual,
      }),
    ),
  );
}

function minimizeInvalidMessages(original, signature) {
  let current = structuredClone(original);
  let attempts = 0;
  const reproduces = (candidate) =>
    validationReport(candidate).some((item) => item.signature === signature);
  let granularity = 2;
  while (current.length > 1 && attempts < 40) {
    const chunkSize = Math.ceil(current.length / granularity);
    let reduced = false;
    for (let start = 0; start < current.length && attempts < 40; start += chunkSize) {
      const candidate = current.filter((_, index) => index < start || index >= start + chunkSize);
      if (!candidate.length) continue;
      attempts++;
      if (reproduces(candidate)) {
        current = candidate;
        granularity = Math.max(2, granularity - 1);
        reduced = true;
        break;
      }
    }
    if (reduced) continue;
    if (granularity >= current.length) break;
    granularity = Math.min(current.length, granularity * 2);
  }
  for (let messageIndex = 0; messageIndex < current.length && attempts < 80; messageIndex++) {
    const components = current[messageIndex].updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) continue;
    let componentIndex = 0;
    while (
      componentIndex < current[messageIndex].updateComponents.components.length &&
      attempts < 80
    ) {
      const candidate = structuredClone(current);
      candidate[messageIndex].updateComponents.components.splice(componentIndex, 1);
      attempts++;
      if (reproduces(candidate)) current = candidate;
      else componentIndex++;
    }
  }
  return { messages: current, attempts };
}

function compact(messages) {
  const result = [];
  let pending;
  const flush = () => {
    if (!pending) return;
    result.push({
      version: pending.version,
      updateComponents: {
        ...pending.envelope,
        components: [...pending.components.values()],
      },
    });
    pending = undefined;
  };
  for (const message of messages) {
    const update = message.updateComponents;
    if (!update) {
      flush();
      result.push(structuredClone(message));
      continue;
    }
    if (!pending || pending.surfaceId !== update.surfaceId) {
      flush();
      const { components: _components, ...envelope } = structuredClone(update);
      pending = {
        version: message.version,
        surfaceId: update.surfaceId,
        envelope,
        components: new Map(),
      };
    }
    for (const component of update.components ?? [])
      pending.components.set(component.id, structuredClone(component));
  }
  flush();
  return result;
}

function splitComponents(messages) {
  return messages.flatMap((message) => {
    const components = message.updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) return [structuredClone(message)];
    return components.map((component) => ({
      version: message.version,
      updateComponents: {
        ...structuredClone(message.updateComponents),
        components: [structuredClone(component)],
      },
    }));
  });
}

function duplicateDataUpdates(messages) {
  return messages.flatMap((message) =>
    message.updateDataModel
      ? [structuredClone(message), structuredClone(message)]
      : [structuredClone(message)],
  );
}

function duplicateComponentUpdates(messages) {
  return messages.flatMap((message) =>
    message.updateComponents
      ? [structuredClone(message), structuredClone(message)]
      : [structuredClone(message)],
  );
}

function canonicalizeConvergentUpdates(messages) {
  const deduplicated = [];
  for (const message of messages) {
    const previous = deduplicated.at(-1);
    const safelyIdempotent =
      message.updateComponents || message.updateDataModel || message.deleteSurface;
    if (safelyIdempotent && previous && canonical(previous) === canonical(message)) continue;
    deduplicated.push(structuredClone(message));
  }
  return compact(deduplicated);
}

export function fuzzVariants(messages, seed, runs) {
  const candidates = [
    { name: 'compacted', messages: compact(messages) },
    { name: 'split-components', messages: splitComponents(messages) },
    { name: 'repeated-data-updates', messages: duplicateDataUpdates(messages) },
    { name: 'repeated-component-updates', messages: duplicateComponentUpdates(messages) },
  ];
  const variants = [];
  for (let index = 0; index < runs; index++)
    variants.push(structuredClone(candidates[(seed + index) % candidates.length]));
  return variants;
}

async function bundleHost(directory) {
  const output = join(directory, 'a2ui-host.js');
  await build({
    entryPoints: [join(here, 'a2ui-host.jsx')],
    outfile: output,
    bundle: true,
    format: 'iife',
    platform: 'browser',
    jsx: 'automatic',
    logLevel: 'silent',
  });
  return output;
}

async function openRenderedPage(browser, bundle, messages, renderer) {
  const page = await browser.newPage();
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));
  await page.setContent(
    '<!doctype html><html><body><main id="reproit-a2ui-root"></main></' + 'body></html>',
  );
  await page.evaluate(
    ({ messages, renderer }) => {
      window.__REPROIT_A2UI_MESSAGES__ = messages;
      window.__REPROIT_A2UI_RENDERER__ = renderer;
    },
    { messages, renderer },
  );
  await page.addScriptTag({ path: bundle });
  await page.waitForFunction(() => window.__REPROIT_A2UI_READY__ === true);
  await page.waitForTimeout(25);
  return { page, pageErrors };
}

async function uniquelyMarkedContainer(page, componentId, scopePath) {
  const markers = page.locator(`[${COMPONENT_MARKER}]`);
  const matches = [];
  for (let index = 0; index < (await markers.count()); index++) {
    const marker = markers.nth(index);
    if ((await marker.getAttribute(COMPONENT_MARKER)) !== componentId) continue;
    if ((await marker.getAttribute(SCOPE_MARKER)) !== scopePath) continue;
    matches.push(marker);
  }
  return matches.length === 1 ? matches[0] : undefined;
}

async function controlHandle(container, contract) {
  if (contract.controlType === 'ChoicePicker') {
    const options = container.locator('input[type="radio"], input[type="checkbox"], button.chip');
    return (await options.count()) === contract.options.length
      ? options.nth(contract.optionIndex)
      : undefined;
  }
  const selectors = {
    TextField: 'input:not([type="hidden"]), textarea',
    CheckBox: 'input[type="checkbox"]',
    Slider: 'input[type="range"]',
    DateTimeInput: 'input[type="date"], input[type="time"], input[type="datetime-local"]',
  };
  const controls = container.locator(selectors[contract.controlType]);
  return (await controls.count()) === 1 ? controls.first() : undefined;
}

async function renderedControlValue(container, control, contract) {
  if (contract.controlType === 'CheckBox') return control.isChecked();
  if (contract.controlType === 'ChoicePicker') {
    const options = container.locator('input[type="radio"], input[type="checkbox"], button.chip');
    const selected = [];
    for (let index = 0; index < (await options.count()); index++) {
      const option = options.nth(index);
      const active =
        (await option.getAttribute('aria-pressed')) !== null
          ? (await option.getAttribute('aria-pressed')) === 'true'
          : await option.isChecked();
      if (active) selected.push(contract.options[index]);
    }
    return selected;
  }
  const value = await control.inputValue();
  return contract.controlType === 'Slider' ? Number(value) : value;
}

async function editControl(control, contract) {
  if (contract.controlType === 'CheckBox' || contract.controlType === 'ChoicePicker')
    await control.click();
  else await control.fill(String(contract.sentinel));
}

async function traceBoundAction(browser, bundle, messages, renderer, contract) {
  const { page } = await openRenderedPage(browser, bundle, messages, renderer);
  try {
    const controlContainer = await uniquelyMarkedContainer(
      page,
      contract.controlId,
      contract.scopePath,
    );
    const buttonContainer = await uniquelyMarkedContainer(
      page,
      contract.buttonId,
      contract.scopePath,
    );
    if (!controlContainer || !buttonContainer)
      return {
        status: 'abstain',
        reason: 'component scope to DOM mapping is missing or ambiguous',
      };
    const control = await controlHandle(controlContainer, contract);
    const buttons = buttonContainer.locator('button');
    const button = (await buttons.count()) === 1 ? buttons.first() : undefined;
    if (!control || !button)
      return {
        status: 'abstain',
        reason: 'catalog control to DOM mapping is missing or ambiguous',
      };
    if (
      !(await control.isVisible()) ||
      !(await control.isEnabled()) ||
      !(await button.isVisible()) ||
      !(await button.isEnabled())
    ) {
      return { status: 'abstain', reason: 'the declared interaction is not currently available' };
    }
    const initialValue = await renderedControlValue(controlContainer, control, contract);
    const initialModel = await page.evaluate(
      (surfaceId) => window.__REPROIT_A2UI_DATA_MODEL__?.(surfaceId),
      contract.surfaceId,
    );
    await editControl(control, contract);
    await page.waitForTimeout(0);
    const editedValue = await renderedControlValue(controlContainer, control, contract);
    const editedModel = await page.evaluate(
      (surfaceId) => window.__REPROIT_A2UI_DATA_MODEL__?.(surfaceId),
      contract.surfaceId,
    );
    await page.evaluate(() => {
      window.__REPROIT_A2UI_ACTIONS__.length = 0;
    });
    await button.click();
    await page.waitForTimeout(0);
    const actions = await page.evaluate(() =>
      window.__REPROIT_A2UI_ACTIONS__.map((action) => ({
        name: action.name,
        surfaceId: action.surfaceId,
        sourceComponentId: action.sourceComponentId,
        context: structuredClone(action.context ?? {}),
      })),
    );
    return {
      status: 'observed',
      initialValue,
      initialModelValue: pointerGet(initialModel, contract.resolvedBindingPath),
      editedValue,
      editedModelValue: pointerGet(editedModel, contract.resolvedBindingPath),
      actions,
    };
  } finally {
    await page.close();
  }
}

function actionReproduction(contract) {
  const kinds = {
    TextField: 'fill',
    CheckBox: 'toggle',
    ChoicePicker: 'select',
    Slider: 'adjust',
    DateTimeInput: 'fill',
  };
  return [
    {
      kind: kinds[contract.controlType],
      surfaceId: contract.surfaceId,
      componentId: contract.controlId,
      scopePath: contract.scopePath,
      value: contract.sentinel,
    },
    { kind: 'activate', surfaceId: contract.surfaceId, componentId: contract.buttonId },
  ];
}

function behaviorRepairContext(messages, renderer, contract) {
  const control = componentRecords(
    messages,
    (component) =>
      component.id === contract.controlId && component.component === contract.controlType,
  )[0];
  const button = componentRecords(
    messages,
    (component) => component.id === contract.buttonId && component.component === 'Button',
  )[0];
  return {
    objective:
      'Make the official renderer preserve this declared data binding and ' +
      'event action after a real edit and activation.',
    repairability: 'renderer-change-required',
    owner: renderer === 'react' ? '@a2ui/react' : '@a2ui/lit',
    editScope:
      'renderer binding and action dispatch implementation, not the ' + 'schema-valid A2UI stream',
    control: schemaContext(control),
    button: schemaContext(button),
    contract: structuredClone(contract),
    validPatchExamples: [],
    notes: [
      `The stream declares one exact ${contract.controlType} data-model path and ` +
        'reuses it in the Button event context.',
      'Do not change labels or invent a new message property.',
      'Replay the recorded fill and activation and require the exact current ' +
        'sentinel in the emitted event context.',
    ],
    revalidateAfterEdit: true,
  };
}

export function evaluateBoundActionObservation(messages, renderer, contract, trace) {
  if (trace.status !== 'observed') return [];
  const baseOracle = {
    kind: 'bound-action-coherence',
    surfaceId: contract.surfaceId,
    controlId: contract.controlId,
    controlType: contract.controlType,
    bindingPath: contract.bindingPath,
    resolvedBindingPath: contract.resolvedBindingPath,
    scopePath: contract.scopePath,
    buttonId: contract.buttonId,
    actionName: contract.actionName,
    contextPath: contract.contextPath,
  };
  const failures = [];
  if (canonical(trace.initialValue) !== canonical(contract.renderedInitialValue))
    failures.push({
      violation: 'initial-rendered-state-mismatch',
      expected: contract.renderedInitialValue,
      actual: trace.initialValue,
    });
  if (canonical(trace.initialModelValue) !== canonical(contract.initialValue))
    failures.push({
      violation: 'initial-model-state-mismatch',
      expected: contract.initialValue,
      actual: trace.initialModelValue,
    });
  if (canonical(trace.editedValue) !== canonical(contract.sentinel))
    failures.push({
      violation: 'edited-control-mismatch',
      expected: contract.sentinel,
      actual: trace.editedValue,
    });
  if (canonical(trace.editedModelValue) !== canonical(contract.sentinel))
    failures.push({
      violation: 'edited-model-mismatch',
      expected: contract.sentinel,
      actual: trace.editedModelValue,
    });
  if (trace.actions.length !== 1)
    failures.push({
      violation: 'action-count-mismatch',
      expected: 1,
      actual: trace.actions.length,
    });
  const action = trace.actions.length === 1 ? trace.actions[0] : undefined;
  if (
    action &&
    (action.name !== contract.actionName ||
      action.surfaceId !== contract.surfaceId ||
      action.sourceComponentId !== contract.buttonId)
  ) {
    failures.push({
      violation: 'action-identity-mismatch',
      expected: {
        name: contract.actionName,
        surfaceId: contract.surfaceId,
        sourceComponentId: contract.buttonId,
      },
      actual: {
        name: action.name,
        surfaceId: action.surfaceId,
        sourceComponentId: action.sourceComponentId,
      },
    });
  }
  if (
    action &&
    canonical(pointerGet(action.context, contract.contextPath)) !== canonical(contract.sentinel)
  ) {
    failures.push({
      violation: 'action-context-mismatch',
      expected: contract.sentinel,
      actual: pointerGet(action.context, contract.contextPath),
    });
  }
  return failures.map((failure) => {
    const oracle = { ...baseOracle, violation: failure.violation, expected: failure.expected };
    const detail = {
      oracle,
      actual: failure.actual,
      reproductionActions: actionReproduction(contract),
    };
    return {
      ...finding(
        'bound-action-coherence',
        renderer,
        `the ${contract.controlId} binding and ${contract.buttonId} action ` +
          `violate ${failure.violation}`,
        detail,
      ),
      repairContext: behaviorRepairContext(messages, renderer, contract),
    };
  });
}

function hasAccessibleName(snapshot) {
  return /\"(?:[^\"\\]|\\.)+\"/.test((snapshot.split('\n')[0] ?? '').trim());
}

async function renderOne(browser, bundle, messages, renderer) {
  const { page, pageErrors } = await openRenderedPage(browser, bundle, messages, renderer);
  const inputs = page.locator('input:not([type="hidden"]), textarea, select');
  const inputObservations = [];
  for (let index = 0; index < (await inputs.count()); index++) {
    const input = inputs.nth(index);
    if (!(await input.isVisible())) continue;
    const snapshot = (await input.ariaSnapshot()).normalize('NFKC');
    inputObservations.push({
      index,
      accessibleNamePresent: hasAccessibleName(snapshot),
      accessibilitySha256: sha256(snapshot),
    });
  }
  const buttons = page.getByRole('button');
  const buttonObservations = [];
  for (let index = 0; index < (await buttons.count()); index++) {
    const button = buttons.nth(index);
    if (!(await button.isVisible())) continue;
    const snapshot = (await button.ariaSnapshot()).normalize('NFKC');
    buttonObservations.push({
      index,
      accessibleNamePresent: hasAccessibleName(snapshot),
      accessibilitySha256: sha256(snapshot),
    });
  }
  const host = await page.evaluate(() => ({
    errors: [...(window.__REPROIT_A2UI_ERRORS__ ?? [])],
    actions: [...(window.__REPROIT_A2UI_ACTIONS__ ?? [])],
    renderedElements: document.querySelectorAll('*').length,
    state: window.__REPROIT_A2UI_STATE__?.() ?? [],
    resolved: Object.fromEntries(window.__REPROIT_A2UI_RESOLVED__ ?? []),
  }));
  await page.close();
  const behavior = [];
  for (const contract of boundActionContracts(messages)) {
    behavior.push({
      contract,
      trace: await traceBoundAction(browser, bundle, messages, renderer, contract),
    });
  }
  return {
    ...host,
    errors: [...new Set([...host.errors, ...pageErrors])],
    inputs: inputObservations,
    buttons: buttonObservations,
    behavior,
  };
}

function finding(kind, renderer, reason, detail = {}) {
  const signature = sha256({ kind, renderer, reason, detail });
  return { kind, renderer, reason, signature, ...detail };
}

async function scanStream(browser, bundle, messages) {
  const observations = {};
  const findings = [];
  const expectedState = canonicalStateFromMessages(messages);
  for (const renderer of ['react', 'lit']) {
    const observation = await renderOne(browser, bundle, messages, renderer);
    observation.state = normalizedRuntimeState(observation.state);
    observations[renderer] = observation;
    const stateDifference = firstDifference(expectedState, observation.state);
    if (stateDifference) {
      findings.push(
        finding(
          'stream-convergence',
          renderer,
          `official replay state diverges at ${stateDifference.path}`,
          {
            proofStatus: 'VIOLATION',
            oracle: {
              transformation: 'official-message-replay',
              path: stateDifference.path,
              expected: stateDifference.expected,
            },
            actual: stateDifference.actual,
          },
        ),
      );
    }
    for (const reason of observation.errors)
      findings.push(finding('renderer-error', renderer, reason));
    for (const input of observation.inputs.filter((input) => !input.accessibleNamePresent)) {
      findings.push(
        finding('unlabeled-input', renderer, 'visible form control has no accessible name', {
          inputIndex: input.index,
        }),
      );
    }
    for (const button of observation.buttons.filter((button) => !button.accessibleNamePresent)) {
      findings.push(
        finding('unlabeled-button', renderer, 'visible button has no accessible name', {
          buttonIndex: button.index,
        }),
      );
    }
    for (const item of observation.behavior) {
      findings.push(
        ...evaluateBoundActionObservation(messages, renderer, item.contract, item.trace),
      );
    }
  }
  const rendererStateDifference = firstDifference(observations.react.state, observations.lit.state);
  if (rendererStateDifference) {
    findings.push(
      finding(
        'stream-convergence',
        'react-vs-lit',
        `official renderer states diverge at ${rendererStateDifference.path}`,
        {
          proofStatus: 'VIOLATION',
          oracle: {
            transformation: 'cross-renderer-replay',
            path: rendererStateDifference.path,
            expected: rendererStateDifference.expected,
          },
          actual: rendererStateDifference.actual,
        },
      ),
    );
  }
  const resolvedDifference = firstDifference(
    observations.react.resolved,
    observations.lit.resolved,
  );
  if (resolvedDifference) {
    findings.push(
      finding(
        'default-conformance',
        'react-vs-lit',
        `official resolved properties diverge at ${resolvedDifference.path}`,
        {
          proofStatus: 'VIOLATION',
          oracle: {
            transformation: 'cross-renderer-default-resolution',
            path: resolvedDifference.path,
            expected: resolvedDifference.expected,
          },
          actual: resolvedDifference.actual,
        },
      ),
    );
  }
  const canonicalMessages = canonicalizeConvergentUpdates(messages);
  if (canonical(canonicalMessages) !== canonical(messages)) {
    for (const renderer of ['react', 'lit']) {
      const canonicalObservation = await renderOne(browser, bundle, canonicalMessages, renderer);
      canonicalObservation.state = normalizedRuntimeState(canonicalObservation.state);
      const difference = firstDifference(
        equivalentObservation(observations[renderer]),
        equivalentObservation(canonicalObservation),
      );
      if (difference) {
        findings.push(
          finding(
            'stream-convergence',
            renderer,
            `idempotent update normalization diverges at ${difference.path}`,
            {
              proofStatus: 'VIOLATION',
              oracle: {
                transformation: 'deduplicate-and-compact-idempotent-updates',
                path: difference.path,
                expected: difference.expected,
              },
              actual: difference.actual,
            },
          ),
        );
      }
    }
  }
  return { observations, findings };
}

async function reproducesFinding(browser, bundle, messages, signature) {
  if (validateMessages(messages).length) return false;
  const result = await scanStream(browser, bundle, messages);
  return result.findings.some((item) => item.signature === signature);
}

async function minimizeMessages(browser, bundle, original, signature) {
  let current = structuredClone(original);
  let attempts = 0;
  let granularity = 2;
  while (current.length > 1 && attempts < 40) {
    const chunkSize = Math.ceil(current.length / granularity);
    let reduced = false;
    for (let start = 0; start < current.length && attempts < 40; start += chunkSize) {
      const candidate = current.filter((_, index) => index < start || index >= start + chunkSize);
      if (!candidate.length) continue;
      attempts++;
      if (await reproducesFinding(browser, bundle, candidate, signature)) {
        current = candidate;
        granularity = Math.max(2, granularity - 1);
        reduced = true;
        break;
      }
    }
    if (reduced) continue;
    if (granularity >= current.length) break;
    granularity = Math.min(current.length, granularity * 2);
  }
  for (let messageIndex = 0; messageIndex < current.length && attempts < 80; messageIndex++) {
    const components = current[messageIndex].updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) continue;
    let componentIndex = 0;
    while (
      componentIndex < current[messageIndex].updateComponents.components.length &&
      attempts < 80
    ) {
      const candidate = structuredClone(current);
      candidate[messageIndex].updateComponents.components.splice(componentIndex, 1);
      attempts++;
      if (await reproducesFinding(browser, bundle, candidate, signature)) current = candidate;
      else componentIndex++;
    }
  }
  return { messages: current, attempts };
}

function equivalentObservation(observation) {
  return {
    errors: observation.errors,
    state: observation.state,
    resolved: observation.resolved,
    inputs: observation.inputs.map((input) => ({
      accessibleNamePresent: input.accessibleNamePresent,
      accessibilitySha256: input.accessibilitySha256,
    })),
    buttons: observation.buttons.map((button) => ({
      accessibleNamePresent: button.accessibleNamePresent,
      accessibilitySha256: button.accessibilitySha256,
    })),
    behavior: observation.behavior,
  };
}

async function run(config) {
  const text = await readFile(config.target, 'utf8');
  let messages;
  let expected;
  if (config.command === 'replay') {
    const document = JSON.parse(text);
    if (document?.format !== 'reproit-a2ui-finding')
      throw new Error('replay target is not a Reproit A2UI finding');
    messages = document.messages;
    expected = document.finding;
  } else {
    messages = parseA2uiText(text).messages;
  }
  const validationFindings = validationReport(messages);
  if (validationFindings.length) {
    const findings = validationFindings.map((item) => {
      const minimized = minimizeInvalidMessages(messages, item.signature);
      return { ...item, minimalMessages: minimized.messages, shrinkAttempts: minimized.attempts };
    });
    if (config.command === 'replay') {
      const reproduced = findings.some(
        (item) => item.signature === (config.expect ?? expected?.signature),
      );
      return {
        format: 'reproit-a2ui-replay',
        reproduced,
        expected,
        repairContract: A2UI_REPAIR_CONTRACT,
        findings,
        observations: {},
      };
    }
    return {
      format: 'reproit-a2ui-run',
      command: config.command,
      target: basename(config.target),
      messagesSha256: sha256(messages),
      messages,
      repairContract: A2UI_REPAIR_CONTRACT,
      findings,
      observations: {},
    };
  }
  const temporary = await mkdtemp(join(tmpdir(), 'reproit-a2ui-runner-'));
  let browser;
  try {
    const bundle = await bundleHost(temporary);
    browser = await chromium.launch({ headless: true });
    const baseline = await scanStream(browser, bundle, messages);
    const findings = baseline.findings.map((item) => ({
      ...attachRepairContext(messages, item),
      reproductionMessages: messages,
    }));
    const variants = [];
    if (config.command === 'fuzz') {
      for (const variant of fuzzVariants(messages, config.seed, config.runs)) {
        const variantValidation = validateMessages(variant.messages);
        if (variantValidation.length)
          throw new Error(
            `internal ${variant.name} mutation is not schema-valid: ${variantValidation[0].reason}`,
          );
        const result = await scanStream(browser, bundle, variant.messages);
        for (const item of result.findings) {
          if (!findings.some((existing) => existing.signature === item.signature)) {
            findings.push({
              ...attachRepairContext(variant.messages, item),
              reproductionMessages: variant.messages,
            });
          }
        }
        variants.push({
          name: variant.name,
          messagesSha256: sha256(variant.messages),
          observations: result.observations,
        });
      }
    }
    if (config.command === 'replay') {
      const reproduced = findings.some(
        (item) => item.signature === (config.expect ?? expected?.signature),
      );
      return {
        format: 'reproit-a2ui-replay',
        reproduced,
        expected,
        repairContract: A2UI_REPAIR_CONTRACT,
        findings,
        observations: baseline.observations,
      };
    }
    const minimizedFindings = [];
    for (const item of findings) {
      const { reproductionMessages, ...publicFinding } = item;
      const minimized = await minimizeMessages(
        browser,
        bundle,
        reproductionMessages,
        item.signature,
      );
      minimizedFindings.push({
        ...publicFinding,
        minimalMessages: minimized.messages,
        shrinkAttempts: minimized.attempts,
      });
    }
    return {
      format: 'reproit-a2ui-run',
      command: config.command,
      target: basename(config.target),
      seed: config.seed,
      runs: config.command === 'fuzz' ? config.runs : 0,
      messagesSha256: sha256(messages),
      messages,
      repairContract: A2UI_REPAIR_CONTRACT,
      conformance: negotiatedConformance(messages),
      findings: minimizedFindings,
      observations: baseline.observations,
      variants,
    };
  } finally {
    await browser?.close();
    await rm(temporary, { recursive: true, force: true });
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const config = parseArgs(process.argv.slice(2));
  run(config)
    .then(async (report) => {
      const output = JSON.stringify(report, null, 2) + '\n';
      if (config.output) await writeFile(config.output, output);
      process.stdout.write(output);
      if (report.format === 'reproit-a2ui-replay' ? report.reproduced : report.findings.length > 0)
        process.exitCode = 1;
    })
    .catch((error) => {
      console.error(`reproit-a2ui: ${error.stack ?? error.message}`);
      process.exitCode = 2;
    });
}
