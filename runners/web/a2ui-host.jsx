import React from 'react';
import {createRoot} from 'react-dom/client';
import {MessageProcessor} from '@a2ui/web_core/v0_9';
import {
  A2uiSurface as ReactSurface,
  basicCatalog as reactCatalog,
} from '@a2ui/react/v0_9';
import {
  A2uiSurface as LitSurface,
  basicCatalog as litCatalog,
} from '@a2ui/lit/v0_9';

const messages = structuredClone(window.__REPROIT_A2UI_MESSAGES__ ?? []);
const renderer = window.__REPROIT_A2UI_RENDERER__;
const root = document.getElementById('reproit-a2ui-root');
const actions = [];
const errors = [];

window.__REPROIT_A2UI_ACTIONS__ = actions;
window.__REPROIT_A2UI_ERRORS__ = errors;
window.addEventListener('error', event => errors.push(String(event.error?.message ?? event.message)));
window.addEventListener('unhandledrejection', event => errors.push(String(event.reason?.message ?? event.reason)));

function processorFor(catalog) {
  const processor = new MessageProcessor([catalog], action => actions.push(structuredClone(action)));
  processor.model.onSurfaceCreated.subscribe(surface => {
    surface.onError.subscribe(error => errors.push(String(error?.message ?? error)));
  });
  processor.processMessages(messages);
  return processor;
}

try {
  if (!root) throw new Error('A2UI host root is missing');
  if (renderer === 'react') {
    const processor = processorFor(reactCatalog);
    const surfaces = [...processor.model.surfacesMap.values()];
    createRoot(root).render(
      <React.StrictMode>
        {surfaces.map(surface => <ReactSurface key={surface.id} surface={surface} />)}
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

requestAnimationFrame(() => requestAnimationFrame(() => {
  window.__REPROIT_A2UI_READY__ = true;
}));
