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
