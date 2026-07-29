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

const DATE_VALUE_SOURCE = String.raw`\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])`;
const TIME_VALUE_SOURCE = String.raw`(?:[01]\d|2[0-3]):[0-5]\d`;
const TIME_SUFFIX_SOURCE =
  String.raw`(?::[0-5]\d(?:\.\d+)?)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)?`;
const DATE_TIME_INPUT_PATTERN = new RegExp(
  `^(${DATE_VALUE_SOURCE})T(${TIME_VALUE_SOURCE})${TIME_SUFFIX_SOURCE}$`,
);
const TIME_INPUT_PATTERN = new RegExp(`^(${TIME_VALUE_SOURCE})${TIME_SUFFIX_SOURCE}$`);
const DATE_INPUT_PATTERN = /^(\d{4})-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$/;

export function normalizeDateTimeInputValue(value, mode) {
  if (value === '') return '';
  const dateTime = value.match(DATE_TIME_INPUT_PATTERN);
  const time = value.match(TIME_INPUT_PATTERN);
  const dateValue = dateTime?.[1] ?? (mode === 'date' ? value : undefined);
  const date = dateValue?.match(DATE_INPUT_PATTERN);
  if ((mode === 'date' || mode === 'datetime-local') && !date) return undefined;
  if (mode === 'time' && !time && !dateTime) return undefined;
  if (!['date', 'time', 'datetime-local'].includes(mode)) return undefined;
  const year = Number(date?.[1]);
  const month = Number(date?.[2]);
  const day = Number(date?.[3]);
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (date && (year === 0 || day > daysInMonth[month - 1])) return undefined;
  if (mode === 'date') return dateValue;
  const timeValue = dateTime?.[2] ?? time?.[1];
  return mode === 'time' ? timeValue : `${dateValue}T${timeValue}`;
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
      const sentinel =
        mode === 'date' ? '2031-02-03' : mode === 'time' ? '13:37' : '2031-02-03T13:37';
      const renderedInitialValue = normalizeDateTimeInputValue(initialValue, mode);
      if (renderedInitialValue === undefined || renderedInitialValue === sentinel) return undefined;
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
