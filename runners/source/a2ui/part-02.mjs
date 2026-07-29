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
