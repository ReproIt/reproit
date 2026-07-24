// Interactive review controls for a deterministic web replay.
//
// The inspector is injected only after a state has been observed and is removed
// before the next action runs. It therefore cannot enter the state signature,
// action candidates, or oracle evidence. Normal replay never imports behavior
// from this module unless REPROIT_INSPECT=1.

const DEFAULT_WAIT_MS = 240_000;
const MIN_WAIT_MS = 1_000;
const MAX_WAIT_MS = 900_000;
const ROOT_ID = '__reproit_inspector';

export function boundedInspectWaitMs(raw) {
  const parsed = Number.parseInt(String(raw || ''), 10);
  if (!Number.isFinite(parsed)) return DEFAULT_WAIT_MS;
  return Math.min(MAX_WAIT_MS, Math.max(MIN_WAIT_MS, parsed));
}

export function humanizeInspectAction(action) {
  if (!action || action === 'load') return 'Load';
  if (action === 'back') return 'Go back';
  if (action.startsWith('tap:')) return `Tap ${action.slice(4)}`;
  if (action.startsWith('type:')) {
    const body = action.slice(5);
    const split = body.lastIndexOf('=');
    if (split < 0) return `Type in ${body}`;
    return `Type "${body.slice(split + 1)}" in ${body.slice(0, split)}`;
  }
  if (action.startsWith('assert:')) return `Check ${action.slice(7)}`;
  return action;
}

export function inspectStepModel(action, stepIndex, totalSteps, observation) {
  const selector = actionSelector(action);
  const target = selector
    ? (observation?.tappables || []).find((candidate) => candidate.sel === selector)
    : null;
  return {
    action,
    actionLabel: humanizeInspectAction(action),
    stepIndex,
    totalSteps,
    isTrigger: stepIndex === totalSteps,
    targetLabel: target?.label || selector || null,
    targetBounds: validBounds(target?.bounds) ? target.bounds : null,
  };
}

export async function inspectReplayStep(page, model, waitMs = DEFAULT_WAIT_MS) {
  await page.bringToFront();
  await installOverlay(page, { ...model, mode: 'step' });
  try {
    await page.waitForFunction(
      (rootId) => Boolean(document.getElementById(rootId)?.dataset.decision),
      ROOT_ID,
      { timeout: boundedInspectWaitMs(waitMs) },
    );
    return await page.evaluate(removeOverlayAndReadDecision, ROOT_ID);
  } catch (error) {
    await removeOverlay(page);
    throw new Error(`inspection timed out while waiting at step ${model.stepIndex}: ${error}`);
  }
}

export async function inspectReplayFinished(page, waitMs = DEFAULT_WAIT_MS) {
  if (page.isClosed()) return;
  await page.bringToFront();
  await installOverlay(page, {
    mode: 'finished',
    actionLabel: 'Replay finished',
    stepIndex: 0,
    totalSteps: 0,
    isTrigger: false,
    targetBounds: null,
    targetLabel: null,
  });
  try {
    await page.waitForFunction(
      (rootId) => Boolean(document.getElementById(rootId)?.dataset.decision),
      ROOT_ID,
      { timeout: boundedInspectWaitMs(waitMs) },
    );
    await page.evaluate(removeOverlayAndReadDecision, ROOT_ID);
  } catch (error) {
    await removeOverlay(page);
    throw new Error(`inspection timed out after the replay: ${error}`);
  }
}

function actionSelector(action) {
  if (action.startsWith('tap:')) return action.slice(4);
  if (!action.startsWith('type:')) return null;
  const body = action.slice(5);
  const split = body.lastIndexOf('=');
  return split < 0 ? body : body.slice(0, split);
}

function validBounds(bounds) {
  return (
    Array.isArray(bounds) &&
    bounds.length === 4 &&
    bounds.every((value) => Number.isFinite(value))
  );
}

async function installOverlay(page, model) {
  await page.evaluate(
    ({ rootId, model }) => {
      if (typeof window.__reproitInspectorCleanup === 'function') {
        window.__reproitInspectorCleanup();
      }
      document.getElementById(rootId)?.remove();

      const root = document.createElement('div');
      root.id = rootId;
      root.dataset.decision = '';
      root.style.cssText =
        'position:fixed;inset:0;z-index:2147483647;pointer-events:none;font-family:' +
        'ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,monospace';
      const shadow = root.attachShadow({ mode: 'open' });

      const style = document.createElement('style');
      style.textContent = `
        * { box-sizing: border-box; }
        .target {
          position: fixed;
          border: 3px solid #4ade80;
          border-radius: 7px;
          background: rgba(74, 222, 128, .13);
          box-shadow: 0 0 0 2px rgba(0,0,0,.55), 0 0 28px rgba(74,222,128,.35);
          pointer-events: none;
        }
        .panel {
          position: fixed;
          right: 22px;
          bottom: 22px;
          width: min(440px, calc(100vw - 44px));
          padding: 17px;
          border: 1px solid #394047;
          border-radius: 12px;
          background: rgba(14, 17, 20, .97);
          color: #e7ecef;
          box-shadow: 0 16px 48px rgba(0,0,0,.55);
          pointer-events: auto;
        }
        .eyebrow { color: #4ade80; font-size: 11px; letter-spacing: .12em; }
        .title { margin-top: 8px; font-size: 15px; line-height: 1.45; }
        .target-name { margin-top: 7px; color: #9ba5ad; font-size: 12px; }
        .actions { display: flex; gap: 9px; margin-top: 15px; }
        button {
          appearance: none;
          border: 1px solid #46515a;
          border-radius: 8px;
          padding: 9px 12px;
          background: #20262b;
          color: #f4f7f8;
          font: 600 12px/1.2 inherit;
          cursor: pointer;
        }
        button.primary { border-color: #4ade80; background: #35c96d; color: #07110b; }
        button:hover { filter: brightness(1.12); }
        .hint { margin-top: 11px; color: #77828a; font-size: 11px; }
      `;
      shadow.appendChild(style);

      if (model.targetBounds) {
        const [x, y, width, height] = model.targetBounds;
        const target = document.createElement('div');
        target.className = 'target';
        target.style.left = `${Math.max(0, x - 3)}px`;
        target.style.top = `${Math.max(0, y - 3)}px`;
        target.style.width = `${Math.max(1, width + 6)}px`;
        target.style.height = `${Math.max(1, height + 6)}px`;
        shadow.appendChild(target);
      }

      const panel = document.createElement('section');
      panel.className = 'panel';
      const eyebrow = document.createElement('div');
      eyebrow.className = 'eyebrow';
      eyebrow.textContent =
        model.mode === 'finished'
          ? 'REPROIT INSPECT'
          : `REPROIT INSPECT  ${model.stepIndex}/${model.totalSteps}${
              model.isTrigger ? '  TRIGGER' : ''
            }`;
      const title = document.createElement('div');
      title.className = 'title';
      title.textContent =
        model.mode === 'finished'
          ? 'Replay finished. Close the inspector to classify the result.'
          : model.actionLabel;
      panel.append(eyebrow, title);

      if (model.targetLabel) {
        const targetName = document.createElement('div');
        targetName.className = 'target-name';
        targetName.textContent = `Target: ${model.targetLabel}`;
        panel.appendChild(targetName);
      }

      const actions = document.createElement('div');
      actions.className = 'actions';
      const decide = (decision) => {
        root.dataset.decision = decision;
      };
      if (model.mode === 'finished') {
        actions.appendChild(makeButton('Close inspector', 'primary', () => decide('finish')));
      } else {
        actions.appendChild(makeButton('Run next action', 'primary', () => decide('step')));
        actions.appendChild(makeButton('Continue to failure', '', () => decide('continue')));
      }
      panel.appendChild(actions);

      const hint = document.createElement('div');
      hint.className = 'hint';
      hint.textContent =
        model.mode === 'finished'
          ? 'Press Enter to close'
          : 'Enter: next action   C: continue to failure';
      panel.appendChild(hint);
      shadow.appendChild(panel);
      (document.body || document.documentElement).appendChild(root);

      const onKey = (event) => {
        if (event.key === 'Enter' && !event.repeat) {
          event.preventDefault();
          decide(model.mode === 'finished' ? 'finish' : 'step');
        } else if (model.mode !== 'finished' && event.key.toLowerCase() === 'c') {
          event.preventDefault();
          decide('continue');
        }
      };
      window.addEventListener('keydown', onKey, true);
      window.__reproitInspectorCleanup = () => {
        window.removeEventListener('keydown', onKey, true);
        root.remove();
        window.__reproitInspectorCleanup = null;
      };

      function makeButton(label, className, onClick) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = className;
        button.textContent = label;
        button.addEventListener('click', onClick);
        return button;
      }
    },
    { rootId: ROOT_ID, model },
  );
}

async function removeOverlay(page) {
  if (page.isClosed()) return;
  await page
    .evaluate(
      (rootId) => {
        if (typeof window.__reproitInspectorCleanup === 'function') {
          window.__reproitInspectorCleanup();
        } else {
          document.getElementById(rootId)?.remove();
        }
      },
      ROOT_ID,
    )
    .catch(() => {});
}

function removeOverlayAndReadDecision(rootId) {
  const decision = document.getElementById(rootId)?.dataset.decision || '';
  if (typeof window.__reproitInspectorCleanup === 'function') {
    window.__reproitInspectorCleanup();
  } else {
    document.getElementById(rootId)?.remove();
  }
  return decision;
}
