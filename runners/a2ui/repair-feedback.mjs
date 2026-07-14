const DEFAULT_REPRODUCTION_BUDGET = 16_000;

function compactContext(context, componentSchemas, messageSchemas) {
  if (!context || typeof context !== 'object') return context;
  const result = structuredClone(context);
  if (result.component?.schema && result.component.type) {
    componentSchemas[result.component.type] ??= result.component.schema;
    delete result.component.schema;
    result.component.schemaRef = `#/componentSchemas/${result.component.type}`;
  }
  if (result.message?.schema && result.message.operation) {
    messageSchemas[result.message.operation] ??= result.message.schema;
    delete result.message.schema;
    result.message.schemaRef = `#/messageSchemas/${result.message.operation}`;
  }
  return result;
}

export function exactRepairFeedback(report, options = {}) {
  const schemas = {};
  const messageSchemas = {};
  let reproductionBudget = options.reproductionBudget ?? DEFAULT_REPRODUCTION_BUDGET;
  const findings = (report.findings ?? []).map(finding => {
    const minimized = finding.minimalMessages;
    const minimizedJson = minimized === undefined ? '' : JSON.stringify(minimized);
    const includeMessages = minimizedJson.length <= reproductionBudget;
    if (includeMessages) reproductionBudget -= minimizedJson.length;
    return {
      kind: finding.kind,
      renderer: finding.renderer,
      reason: finding.reason,
      path: finding.path,
      signature: finding.signature,
      minimalMessages: includeMessages ? minimized : undefined,
      minimalReproduction: minimized === undefined ? undefined : {
        messageCount: Array.isArray(minimized) ? minimized.length : undefined,
        included: includeMessages,
        omittedReason: includeMessages ? undefined : 'The complete original stream is already present in the repair request.',
      },
      repairContext: compactContext(finding.repairContext, schemas, messageSchemas),
    };
  });
  return JSON.stringify({
    status: 'REPRODUCED',
    repairContract: report.repairContract,
    componentSchemas: schemas,
    messageSchemas,
    findings,
  });
}
