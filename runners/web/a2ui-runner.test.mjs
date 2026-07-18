import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import {
  A2UI_REPAIR_CONTRACT,
  boundActionContracts,
  evaluateBoundActionObservation,
  fuzzVariants,
  negotiatedConformance,
  parseA2uiText,
  validateMessages,
} from './a2ui-runner.mjs';

const create = {
  version: 'v0.9',
  createSurface: {
    surfaceId: 'test',
    catalogId: 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json',
  },
};
const heading = {
  version: 'v0.9',
  updateComponents: {
    surfaceId: 'test',
    components: [{ id: 'root', component: 'Text', text: 'Ready', variant: 'h2' }],
  },
};

function boundForm() {
  return [
    create,
    {
      version: 'v0.9',
      updateDataModel: { surfaceId: 'test', path: '/', value: { email: 'before@example.test' } },
    },
    {
      version: 'v0.9',
      updateComponents: {
        surfaceId: 'test',
        components: [
          { id: 'root', component: 'Column', children: ['email', 'submit'] },
          { id: 'email', component: 'TextField', label: 'Email', value: { path: '/email' } },
          {
            id: 'submit',
            component: 'Button',
            child: 'submit-label',
            action: { event: { name: 'submit', context: { email: { path: '/email' } } } },
          },
          { id: 'submit-label', component: 'Text', text: 'Submit' },
        ],
      },
    },
  ];
}

function catalogControlsForm() {
  const controls = [
    { id: 'enabled', component: 'CheckBox', label: 'Enabled', value: { path: '/enabled' } },
    {
      id: 'choice',
      component: 'ChoicePicker',
      label: 'Choice',
      value: { path: '/choice' },
      variant: 'mutuallyExclusive',
      options: [
        { label: 'Alpha', value: 'a' },
        { label: 'Beta', value: 'b' },
      ],
    },
    {
      id: 'level',
      component: 'Slider',
      label: 'Level',
      value: { path: '/level' },
      min: 0,
      max: 10,
      step: 2,
    },
    {
      id: 'when',
      component: 'DateTimeInput',
      label: 'When',
      value: { path: '/when' },
      enableDate: true,
      enableTime: true,
    },
  ];
  const components = [
    {
      id: 'root',
      component: 'Column',
      children: controls.flatMap((control) => [control.id, `${control.id}-submit`]),
    },
  ];
  for (const control of controls) {
    components.push(
      control,
      {
        id: `${control.id}-submit`,
        component: 'Button',
        child: `${control.id}-label`,
        action: {
          event: { name: `save-${control.id}`, context: { value: { path: control.value.path } } },
        },
      },
      { id: `${control.id}-label`, component: 'Text', text: `Save ${control.id}` },
    );
  }
  return [
    create,
    {
      version: 'v0.9',
      updateDataModel: {
        surfaceId: 'test',
        path: '/',
        value: {
          enabled: false,
          choice: ['a'],
          level: 2,
          when: '2026-07-16T10:00Z',
        },
      },
    },
    { version: 'v0.9', updateComponents: { surfaceId: 'test', components } },
  ];
}

function listScopedForm() {
  return [
    create,
    {
      version: 'v0.9',
      updateDataModel: {
        surfaceId: 'test',
        path: '/',
        value: { rows: [{ enabled: false }, { enabled: true }] },
      },
    },
    {
      version: 'v0.9',
      updateComponents: {
        surfaceId: 'test',
        components: [
          { id: 'root', component: 'List', children: { componentId: 'row', path: '/rows' } },
          { id: 'row', component: 'Column', children: ['row-enabled', 'row-submit'] },
          {
            id: 'row-enabled',
            component: 'CheckBox',
            label: 'Enabled',
            value: { path: 'enabled' },
          },
          {
            id: 'row-submit',
            component: 'Button',
            child: 'row-label',
            action: { event: { name: 'save-row', context: { enabled: { path: 'enabled' } } } },
          },
          { id: 'row-label', component: 'Text', text: 'Save row' },
        ],
      },
    },
  ];
}

test('parses JSON, wrapper objects, and JSONL without language assumptions', () => {
  assert.deepEqual(parseA2uiText(JSON.stringify([create])).messages, [create]);
  assert.deepEqual(parseA2uiText(JSON.stringify({ messages: [create] })).messages, [create]);
  assert.deepEqual(
    parseA2uiText(`${JSON.stringify(create)}\n${JSON.stringify(heading)}\n`).messages,
    [create, heading],
  );
});

test('official schemas reject invalid components and accept every fuzz ' + 'mutation', () => {
  assert.deepEqual(validateMessages([create, heading]), []);
  const invalid = structuredClone(heading);
  invalid.updateComponents.components[0].component = 'ImaginaryWidget';
  assert.equal(validateMessages([create, invalid])[0].kind, 'protocol-invalid');
  for (const variant of fuzzVariants([create, heading], 0, 6)) {
    assert.deepEqual(validateMessages(variant.messages), [], variant.name);
  }
});

test('malformed component collections become findings instead of verifier ' + 'crashes', () => {
  const invalid = {
    version: 'v0.9',
    updateComponents: { surfaceId: 'test', components: { invalid: true } },
  };
  const findings = validateMessages([create, invalid]);
  assert.ok(findings.some((finding) => finding.kind === 'protocol-invalid'));
});

test('proves lifecycle ordering violations and permits idempotent unknown ' + 'deletes', () => {
  const update = {
    version: 'v0.9',
    updateDataModel: { surfaceId: 'test', path: '/ready', value: true },
  };
  assert.match(
    validateMessages([update]).find((finding) => finding.proofStatus === 'VIOLATION').reason,
    /before createSurface/,
  );
  const duplicateCreate = validateMessages([create, create]);
  assert.match(
    duplicateCreate.find((finding) => finding.proofStatus === 'VIOLATION').reason,
    /already live/,
  );
  const deleted = { version: 'v0.9', deleteSurface: { surfaceId: 'test' } };
  assert.match(
    validateMessages([create, deleted, update]).find((finding) => finding.proofStatus === 'VIOLATION')
      .reason,
    /after deleteSurface/,
  );
  assert.deepEqual(validateMessages([deleted, deleted]), []);
});

test(
  'negotiated conformance proves typed binding contradictions and ' +
    'abstains on function results',
  () => {
    const wrongType = [
      create,
      { version: 'v0.9', updateDataModel: { surfaceId: 'test', path: '/', value: { name: 42 } } },
      {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'test',
          components: [
            { id: 'root', component: 'TextField', label: 'Name', value: { path: '/name' } },
          ],
        },
      },
    ];
    const proof = negotiatedConformance(wrongType);
    assert.equal(proof.status, 'VIOLATION');
    assert.equal(proof.errors.length, 1);
    assert.equal(proof.errors[0].oracle.expectedType, 'string');
    assert.equal(
      validateMessages(wrongType).some((finding) => finding.reason.includes('expected string')),
      true,
    );

    const external = structuredClone(wrongType);
    external[2].updateComponents.components[0].value = {
      call: 'lookupName',
      args: {},
      returnType: 'string',
    };
    const unknown = negotiatedConformance(external);
    assert.equal(unknown.status, 'ABSTAIN');
    assert.equal(unknown.errors.length, 0);
    assert.equal(
      unknown.claims.some((claim) => claim.status === 'ABSTAIN'),
      true,
    );
  },
);

test(
  'protocol findings shrink without launching a renderer and keep the ' + 'exact signature',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-invalid-'));
    try {
      const fixture = join(directory, 'invalid.json');
      const invalid = {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'test',
          components: [
            { id: 'broken', component: 'ImaginaryWidget' },
            { id: 'irrelevant', component: 'Text', text: 'Remove me' },
          ],
        },
      };
      await writeFile(fixture, JSON.stringify([create, invalid]));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.equal(scan.status, 1, scan.stderr);
      const report = JSON.parse(scan.stdout);
      const finding = report.findings.find((item) => item.reason.includes('ImaginaryWidget'));
      assert.ok(finding);
      assert.ok(finding.shrinkAttempts > 0);
      assert.equal(finding.minimalMessages[1].updateComponents.components.length, 1);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  'protocol findings include the exact legal component schema and repair ' + 'contract',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-repair-context-'));
    try {
      const fixture = join(directory, 'invalid-button.json');
      const invalid = {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'test',
          components: [
            { id: 'label', component: 'Text', text: 'Submit' },
            { id: 'submit', component: 'Button', child: 'label', action: 'submit' },
          ],
        },
      };
      await writeFile(fixture, JSON.stringify([create, invalid]));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.equal(scan.status, 1, scan.stderr);
      const report = JSON.parse(scan.stdout);
      const finding = report.findings.find((item) => item.path.endsWith('.action'));
      assert.ok(finding);
      assert.equal(report.repairContract.catalogId, A2UI_REPAIR_CONTRACT.catalogId);
      assert.equal(finding.repairContext.component.type, 'Button');
      assert.ok(finding.repairContext.component.allowedProperties.includes('action'));
      assert.deepEqual(finding.repairContext.component.requiredProperties, [
        'id',
        'component',
        'child',
        'action',
      ]);
      assert.equal(finding.repairContext.component.schema.properties.action.anyOf.length, 2);
      assert.ok(report.repairContract.prohibitedProperties.includes('ariaLabel'));
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  'protocol findings include the exact operation schema for invalid ' + 'data-model updates',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-operation-context-'));
    try {
      const fixture = join(directory, 'invalid-data-model.json');
      const invalid = {
        version: 'v0.9',
        updateDataModel: { surfaceId: 'test', data: { name: 'Ada' } },
      };
      await writeFile(fixture, JSON.stringify([create, invalid]));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.equal(scan.status, 1, scan.stderr);
      const report = JSON.parse(scan.stdout);
      const finding = report.findings.find((item) => item.path === '1.updateDataModel');
      assert.ok(finding);
      const { message } = finding.repairContext;
      assert.equal(message.operation, 'updateDataModel');
      assert.deepEqual(message.operationAllowedProperties.sort(), ['path', 'surfaceId', 'value']);
      assert.deepEqual(message.operationRequiredProperties, ['surfaceId']);
      assert.ok(message.schema.properties.updateDataModel.properties.value);
      assert.equal(finding.repairContext.validPatchExamples[0].path, '1.updateDataModel');
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test('legacy wrapped components receive an exact flat-shape migration', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-legacy-context-'));
  try {
    const fixture = join(directory, 'legacy.json');
    const legacy = {
      version: 'v0.9',
      updateComponents: {
        surfaceId: 'test',
        components: [{ id: 'heading', component: { Text: { text: 'Welcome', variant: 'h1' } } }],
      },
    };
    await writeFile(fixture, JSON.stringify([create, legacy]));
    const scan = spawnSync(
      process.execPath,
      [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
      { encoding: 'utf8' },
    );
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    const context = report.findings.find(
      (item) => item.path === '1.updateComponents.components.0',
    ).repairContext;
    assert.equal(context.detectedShape, 'legacy-wrapped-component');
    assert.equal(context.component.type, 'Text');
    assert.deepEqual(context.validPatchExamples[0].value, {
      id: 'heading',
      component: 'Text',
      text: 'Welcome',
      variant: 'h1',
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('derives only exact supported catalog-control to Button binding ' + 'contracts', () => {
  const messages = boundForm();
  const contracts = boundActionContracts(messages);
  assert.equal(contracts.length, 1);
  assert.deepEqual(
    { ...contracts[0], sentinel: '<sentinel>' },
    {
      surfaceId: 'test',
      controlId: 'email',
      controlType: 'TextField',
      bindingPath: '/email',
      resolvedBindingPath: '/email',
      scopePath: '/',
      buttonId: 'submit',
      actionName: 'submit',
      contextPath: '/email',
      initialValue: 'before@example.test',
      renderedInitialValue: 'before@example.test',
      sentinel: '<sentinel>',
    },
  );
  assert.match(contracts[0].sentinel, /^reproit\+[0-9a-f]{16}@example\.test$/);

  const literal = structuredClone(messages);
  literal[2].updateComponents.components[1].value = 'before@example.test';
  assert.deepEqual(boundActionContracts(literal), []);
  const checked = structuredClone(messages);
  checked[2].updateComponents.components[1].checks = [{ condition: true, message: 'required' }];
  assert.deepEqual(boundActionContracts(checked), []);
  const unrelated = structuredClone(messages);
  unrelated[2].updateComponents.components[2].action.event.context.email.path = '/other';
  assert.deepEqual(boundActionContracts(unrelated), []);
});

test(
  'derives typed contracts for every official interactive catalog control ' +
    'and exact list scope',
  () => {
    const contracts = boundActionContracts(catalogControlsForm());
    assert.deepEqual(contracts.map((contract) => contract.controlType).sort(), [
      'CheckBox',
      'ChoicePicker',
      'DateTimeInput',
      'Slider',
    ]);
    const byType = Object.fromEntries(
      contracts.map((contract) => [contract.controlType, contract]),
    );
    assert.equal(byType.CheckBox.sentinel, true);
    assert.deepEqual(byType.ChoicePicker.sentinel, ['b']);
    assert.equal(byType.Slider.sentinel, 0);
    assert.equal(byType.DateTimeInput.sentinel, '2031-02-03T13:37');

    const scoped = boundActionContracts(listScopedForm());
    assert.equal(scoped.length, 2);
    assert.deepEqual(
      scoped.map((contract) => [
        contract.scopePath,
        contract.resolvedBindingPath,
        contract.initialValue,
      ]),
      [
        ['/rows/0', '/rows/0/enabled', false],
        ['/rows/1', '/rows/1/enabled', true],
      ],
    );
  },
);

test(
  'official React and Lit renderers preserve all typed control state ' + 'through actions',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-catalog-controls-'));
    try {
      const fixture = join(directory, 'catalog-controls.json');
      await writeFile(fixture, JSON.stringify(catalogControlsForm()));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.ok([0, 1].includes(scan.status), scan.stderr);
      const report = JSON.parse(scan.stdout);
      assert.equal(
        report.findings.some((finding) => finding.kind === 'bound-action-coherence'),
        false,
        JSON.stringify(
          report.findings.filter((finding) => finding.kind === 'bound-action-coherence'),
          null,
          2,
        ),
      );
      assert.equal(
        report.findings.some((finding) =>
          ['stream-convergence', 'default-conformance'].includes(finding.kind),
        ),
        false,
        JSON.stringify(
          report.findings.filter((finding) =>
            ['stream-convergence', 'default-conformance'].includes(finding.kind),
          ),
          null,
          2,
        ),
      );
      for (const renderer of ['react', 'lit']) {
        assert.equal(report.observations[renderer].behavior.length, 4);
        assert.equal(
          report.observations[renderer].behavior.every((item) => item.trace.status === 'observed'),
          true,
        );
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  'duplicate and split official updates converge in both official web ' + 'renderers',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-convergence-'));
    try {
      const fixture = join(directory, 'convergent-updates.json');
      const messages = boundForm();
      messages.splice(2, 0, structuredClone(messages[1]));
      messages.push(structuredClone(messages.at(-1)));
      await writeFile(fixture, JSON.stringify(messages));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.ok([0, 1].includes(scan.status), scan.stderr);
      const report = JSON.parse(scan.stdout);
      assert.equal(
        report.findings.some((finding) =>
          ['stream-convergence', 'default-conformance'].includes(finding.kind),
        ),
        false,
        JSON.stringify(
          report.findings.filter((finding) =>
            ['stream-convergence', 'default-conformance'].includes(finding.kind),
          ),
          null,
          2,
        ),
      );
      assert.equal(report.observations.react.state.length, 1);
      assert.deepEqual(report.observations.react.state, report.observations.lit.state);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  'official React and Lit renderers preserve independently scoped list ' + 'bindings',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-list-scope-'));
    try {
      const fixture = join(directory, 'list-scope.json');
      await writeFile(fixture, JSON.stringify(listScopedForm()));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.ok([0, 1].includes(scan.status), scan.stderr);
      const report = JSON.parse(scan.stdout);
      assert.equal(
        report.findings.some((finding) => finding.kind === 'bound-action-coherence'),
        false,
        JSON.stringify(
          report.findings.filter((finding) => finding.kind === 'bound-action-coherence'),
          null,
          2,
        ),
      );
      for (const renderer of ['react', 'lit']) {
        assert.deepEqual(
          report.observations[renderer].behavior.map((item) => item.contract.scopePath),
          ['/rows/0', '/rows/1'],
        );
        assert.equal(
          report.observations[renderer].behavior.every((item) => item.trace.status === 'observed'),
          true,
        );
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test('bound action oracle abstains or emits exact replayable renderer-owned ' + 'feedback', () => {
  const messages = boundForm();
  const contract = boundActionContracts(messages)[0];
  assert.deepEqual(
    evaluateBoundActionObservation(messages, 'react', contract, {
      status: 'abstain',
      reason: 'ambiguous',
    }),
    [],
  );
  const cleanTrace = {
    status: 'observed',
    initialValue: contract.renderedInitialValue,
    initialModelValue: contract.initialValue,
    editedValue: contract.sentinel,
    editedModelValue: contract.sentinel,
    actions: [
      {
        name: contract.actionName,
        surfaceId: contract.surfaceId,
        sourceComponentId: contract.buttonId,
        context: { email: contract.sentinel },
      },
    ],
  };
  assert.deepEqual(evaluateBoundActionObservation(messages, 'react', contract, cleanTrace), []);
  const stale = structuredClone(cleanTrace);
  stale.actions[0].context.email = contract.initialValue;
  const findings = evaluateBoundActionObservation(messages, 'lit', contract, stale);
  assert.equal(findings.length, 1);
  assert.equal(findings[0].kind, 'bound-action-coherence');
  assert.equal(findings[0].oracle.violation, 'action-context-mismatch');
  assert.equal(findings[0].actual, contract.initialValue);
  assert.deepEqual(findings[0].reproductionActions, [
    {
      kind: 'fill',
      surfaceId: 'test',
      componentId: 'email',
      scopePath: '/',
      value: contract.sentinel,
    },
    { kind: 'activate', surfaceId: 'test', componentId: 'submit' },
  ]);
  assert.equal(findings[0].repairContext.owner, '@a2ui/lit');
  assert.equal(findings[0].repairContext.repairability, 'renderer-change-required');
  assert.deepEqual(findings[0].repairContext.validPatchExamples, []);
  assert.equal(
    evaluateBoundActionObservation(messages, 'lit', contract, stale)[0].signature,
    findings[0].signature,
  );
  const differentActual = structuredClone(stale);
  differentActual.actions[0].context.email = 'different@example.test';
  assert.notEqual(
    evaluateBoundActionObservation(messages, 'lit', contract, differentActual)[0].signature,
    findings[0].signature,
  );
});

test(
  'official React and Lit renderers expose stable component IDs and pass ' +
    'bound action coherence',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-bound-action-'));
    try {
      const fixture = join(directory, 'bound-form.json');
      await writeFile(fixture, JSON.stringify(boundForm()));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.ok([0, 1].includes(scan.status), scan.stderr);
      const report = JSON.parse(scan.stdout);
      assert.equal(
        report.findings.some((finding) => finding.kind === 'bound-action-coherence'),
        false,
      );
      for (const renderer of ['react', 'lit']) {
        const behavior = report.observations[renderer].behavior;
        assert.equal(behavior.length, 1);
        assert.equal(behavior[0].trace.status, 'observed');
        assert.equal(behavior[0].trace.actions[0].sourceComponentId, 'submit');
        assert.equal(behavior[0].trace.actions[0].context.email, behavior[0].contract.sentinel);
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  'standalone host finds, shrinks, and exactly replays an official ' + 'renderer bug',
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-test-'));
    try {
      const fixture = join(directory, 'stream.json');
      const messages = [
        create,
        {
          version: 'v0.9',
          updateComponents: {
            surfaceId: 'test',
            components: [{ id: 'root', component: 'TextField', label: 'Account', value: '' }],
          },
        },
      ];
      await writeFile(fixture, JSON.stringify(messages));
      const scan = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture],
        { encoding: 'utf8' },
      );
      assert.equal(scan.status, 1, scan.stderr);
      const report = JSON.parse(scan.stdout);
      assert.deepEqual(
        report.findings.map((finding) => [finding.kind, finding.renderer]),
        [['unlabeled-input', 'lit']],
      );
      assert.equal(report.findings[0].minimalMessages.length, 2);
      assert.equal(report.findings[0].repairContext.component.type, 'TextField');
      assert.equal(report.findings[0].repairContext.repairability, 'renderer-change-required');
      assert.equal(report.findings[0].repairContext.owner, '@a2ui/lit');
      assert.deepEqual(report.findings[0].repairContext.validPatchExamples, []);

      const artifact = join(directory, 'finding.json');
      await writeFile(
        artifact,
        JSON.stringify({
          format: 'reproit-a2ui-finding',
          version: 1,
          messages: report.findings[0].minimalMessages,
          finding: report.findings[0],
        }),
      );
      const replay = spawnSync(
        process.execPath,
        [join(import.meta.dirname, 'a2ui-runner.mjs'), 'replay', artifact],
        { encoding: 'utf8' },
      );
      assert.equal(replay.status, 1, replay.stderr);
      assert.equal(JSON.parse(replay.stdout).reproduced, true);

      const persisted = JSON.parse(await readFile(artifact, 'utf8'));
      assert.equal(persisted.finding.signature, report.findings[0].signature);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);
