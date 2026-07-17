import React from 'react';
import { createRoot } from 'react-dom/client';
import { GenericBinder, MessageProcessor } from '@a2ui/web_core/v0_9';
import { A2uiSurface as ReactSurface, basicCatalog as reactCatalog } from '@a2ui/react/v0_9';
import { A2uiSurface as LitSurface, basicCatalog as litCatalog } from '@a2ui/lit/v0_9';

const messages = structuredClone(window.__REPROIT_A2UI_MESSAGES__ ?? []);
const renderer = window.__REPROIT_A2UI_RENDERER__;
const root = document.getElementById('reproit-a2ui-root');
const actions = [];
const errors = [];
const resolved = new Map();
const MARKER_ATTRIBUTE = 'data-reproit-a2ui-component-id';
const SCOPE_ATTRIBUTE = 'data-reproit-a2ui-scope';

window.__REPROIT_A2UI_ACTIONS__ = actions;
window.__REPROIT_A2UI_ERRORS__ = errors;
window.__REPROIT_A2UI_RESOLVED__ = resolved;
window.addEventListener('error', (event) =>
  errors.push(String(event.error?.message ?? event.message)),
);
window.addEventListener('unhandledrejection', (event) =>
  errors.push(String(event.reason?.message ?? event.reason)),
);

function proofValues(value) {
  if (value === null || ['string', 'number', 'boolean'].includes(typeof value)) return value;
  if (Array.isArray(value)) return value.map(proofValues).filter((item) => item !== undefined);
  if (!value || typeof value !== 'object') return undefined;
  return Object.fromEntries(
    Object.entries(value).flatMap(([key, child]) => {
      const safe = proofValues(child);
      return safe === undefined ? [] : [[key, safe]];
    }),
  );
}

function instrumentReactCatalog(catalog) {
  for (const implementation of catalog.components.values()) {
    if (implementation.__reproitInstrumented) continue;
    const marked = [
      ...['TextField', 'CheckBox', 'ChoicePicker', 'Slider', 'DateTimeInput'],
      'Button',
    ].includes(implementation.name);
    const Original = implementation.render;
    implementation.render = (props) => {
      const binding = new GenericBinder(props.context, implementation.schema);
      resolved.set(
        `${props.context.componentModel.id}\u0000${props.context.dataContext.path}`,
        proofValues(binding.snapshot),
      );
      binding.dispose();
      if (!marked) return <Original {...props} />;
      return (
        <div
          data-reproit-a2ui-component-id={props.context.componentModel.id}
          data-reproit-a2ui-component-type={props.context.componentModel.type}
          data-reproit-a2ui-scope={props.context.dataContext.path}
          style={{ display: 'contents' }}
        >
          <Original {...props} />
        </div>
      );
    };
    implementation.__reproitInstrumented = true;
  }
  return catalog;
}

function markLitComponents(node = root) {
  for (const element of node?.children ?? []) {
    const component = element.context?.componentModel;
    if (component?.id && component?.type) {
      element.setAttribute(MARKER_ATTRIBUTE, component.id);
      element.setAttribute('data-reproit-a2ui-component-type', component.type);
      element.setAttribute(SCOPE_ATTRIBUTE, element.context.dataContext.path);
      resolved.set(
        `${component.id}\u0000${element.context.dataContext.path}`,
        proofValues(element.controller?.props ?? {}),
      );
    }
    if (element.shadowRoot) markLitComponents(element.shadowRoot);
    markLitComponents(element);
  }
}

function processorFor(catalog) {
  const processor = new MessageProcessor([catalog], (action) =>
    actions.push(structuredClone(action)),
  );
  processor.model.onSurfaceCreated.subscribe((surface) => {
    surface.onError.subscribe((error) => errors.push(String(error?.message ?? error)));
  });
  processor.processMessages(messages);
  window.__REPROIT_A2UI_DATA_MODEL__ = (surfaceId) =>
    structuredClone(processor.model.surfacesMap.get(surfaceId)?.dataModel.get('/') ?? undefined);
  window.__REPROIT_A2UI_STATE__ = () =>
    [...processor.model.surfacesMap.values()].map((surface) => ({
      id: surface.id,
      catalogId: surface.catalog.id,
      theme: structuredClone(surface.theme),
      sendDataModel: surface.sendDataModel,
      data: structuredClone(surface.dataModel.get('/')),
      components: [...surface.componentsModel.entries].map(([, component]) =>
        structuredClone(component.componentTree),
      ),
    }));
  return processor;
}

try {
  if (!root) throw new Error('A2UI host root is missing');
  if (renderer === 'react') {
    const processor = processorFor(instrumentReactCatalog(reactCatalog));
    const surfaces = [...processor.model.surfacesMap.values()];
    createRoot(root).render(
      <React.StrictMode>
        {surfaces.map((surface) => (
          <ReactSurface key={surface.id} surface={surface} />
        ))}
      </React.StrictMode>,
    );
  } else if (renderer === 'lit') {
    const processor = processorFor(litCatalog);
    for (const surface of processor.model.surfacesMap.values()) {
      const element = new LitSurface();
      element.surface = surface;
      root.append(element);
    }
  } else {
    throw new Error(`unsupported A2UI renderer: ${String(renderer)}`);
  }
} catch (error) {
  errors.push(String(error?.message ?? error));
}

requestAnimationFrame(() =>
  requestAnimationFrame(() => {
    if (renderer === 'lit') markLitComponents();
    window.__REPROIT_A2UI_READY__ = true;
  }),
);
