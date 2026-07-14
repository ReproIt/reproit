import assert from 'node:assert/strict';
import test from 'node:test';
import {exactRepairFeedback} from './repair-feedback.mjs';

test('deduplicates component schemas and caps minimized reproduction payloads', () => {
  const schema = {type: 'object', properties: {action: {type: 'object'}}};
  const report = {
    repairContract: {protocolVersion: 'v0.9'},
    findings: [0, 1].map(index => ({
      kind: 'protocol-invalid',
      renderer: 'protocol',
      reason: 'Invalid input',
      path: `1.updateComponents.components.${index}.action`,
      signature: String(index),
      minimalMessages: [{payload: 'x'.repeat(100)}],
      repairContext: {component: {type: 'Button', path: String(index), schema}},
    })),
  };
  const feedback = JSON.parse(exactRepairFeedback(report, {reproductionBudget: 130}));
  assert.deepEqual(feedback.componentSchemas, {Button: schema});
  assert.equal(feedback.findings[0].repairContext.component.schema, undefined);
  assert.equal(feedback.findings[0].repairContext.component.schemaRef, '#/componentSchemas/Button');
  assert.equal(feedback.findings.filter(finding => finding.minimalReproduction.included).length, 1);
});

test('deduplicates message operation schemas and leaves an exact schema reference', () => {
  const schema = {
    type: 'object',
    properties: {
      updateDataModel: {
        type: 'object',
        properties: {surfaceId: {type: 'string'}, path: {type: 'string'}, value: {}},
        required: ['surfaceId'],
      },
    },
  };
  const report = {
    repairContract: {protocolVersion: 'v0.9'},
    findings: [{
      kind: 'protocol-invalid',
      renderer: 'protocol',
      reason: 'Unrecognized key: data',
      path: '1.updateDataModel',
      signature: 'operation-schema',
      repairContext: {
        message: {operation: 'updateDataModel', schema},
      },
    }],
  };
  const feedback = JSON.parse(exactRepairFeedback(report));
  assert.deepEqual(feedback.messageSchemas, {updateDataModel: schema});
  assert.equal(feedback.findings[0].repairContext.message.schema, undefined);
  assert.equal(
    feedback.findings[0].repairContext.message.schemaRef,
    '#/messageSchemas/updateDataModel',
  );
});
