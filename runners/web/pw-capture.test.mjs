// Validates pw-capture's selector -> reproit-finder mapping and the trace-action
// -> reproit-action conversion WITHOUT running a browser: the mapping is pure, so
// we feed synthetic Playwright selectors / trace actions and assert the finders.
// Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { mapSelector, mapAction, buildResult } from './pw-capture.mjs';

test('getByTestId -> key:testid', () => {
  assert.deepStrictEqual(mapSelector('internal:testid=[data-testid="username"s]'), { finder: 'key:testid:username' });
  assert.deepStrictEqual(mapSelector('internal:testid=[data-test-id="x"]'), { finder: 'key:testid:x' });
  assert.deepStrictEqual(mapSelector('[data-testid="add"]'), { finder: 'key:testid:add' });
});

test('css #id / [id] / [name] -> key:id / key:name', () => {
  assert.deepStrictEqual(mapSelector('css=#submit'), { finder: 'key:id:submit' });
  assert.deepStrictEqual(mapSelector('#submit'), { finder: 'key:id:submit' });
  assert.deepStrictEqual(mapSelector('[id="email"]'), { finder: 'key:id:email' });
  assert.deepStrictEqual(mapSelector('[name="email"]'), { finder: 'key:name:email' });
  assert.deepStrictEqual(mapSelector('internal:attr=[name="email"s]'), { finder: 'key:name:email' });
  assert.deepStrictEqual(mapSelector('internal:attr=[id="email"s]'), { finder: 'key:id:email' });
});

test('getByRole with name -> label:N', () => {
  assert.deepStrictEqual(mapSelector('internal:role=button[name="Save"i]'), { finder: 'label:Save' });
  assert.deepStrictEqual(mapSelector('internal:role=heading[name="Step 2: account"s]'), { finder: 'label:Step 2: account' });
});

test('getByRole WITHOUT a name -> role:<role>#0, flagged weak (not guessed)', () => {
  const m = mapSelector('internal:role=button');
  assert.strictEqual(m.finder, 'role:button#0');
  assert.strictEqual(m.weak, true);
  assert.ok(/without an accessible name/.test(m.reason));
});

test('getByText / getByLabel / getByPlaceholder -> label:T', () => {
  assert.deepStrictEqual(mapSelector('internal:text="Sign in"i'), { finder: 'label:Sign in' });
  assert.deepStrictEqual(mapSelector('internal:label="Email"s'), { finder: 'label:Email' });
  assert.deepStrictEqual(mapSelector('internal:has-text="Continue"'), { finder: 'label:Continue' });
  assert.deepStrictEqual(mapSelector('internal:attr=[placeholder="Search…"s]'), { finder: 'label:Search…' });
});

test('complex / xpath / chained selectors are SKIPPED with a reason (never guessed)', () => {
  const x = mapSelector('xpath=//div[@class="x"]');
  assert.strictEqual(x.skip, true);
  assert.ok(/xpath/.test(x.reason));

  const chained = mapSelector('internal:testid=[data-testid="a"s] >> internal:role=button');
  assert.strictEqual(chained.skip, true);
  assert.ok(/chained/.test(chained.reason));

  const css = mapSelector('css=.list > li:nth-child(2) .badge');
  assert.strictEqual(css.skip, true);
  assert.ok(/complex css/.test(css.reason));

  assert.strictEqual(mapSelector('').skip, true);
});

test('mapAction: click/check -> tap, fill/type -> type=value, goto -> goto', () => {
  assert.deepStrictEqual(
    mapAction('locator.click', 'internal:testid=[data-testid="continue"s]'),
    { kind: 'tap', finder: 'key:testid:continue', weak: undefined, reason: undefined }
  );
  assert.deepStrictEqual(
    mapAction('locator.check', '[id="agree"]'),
    { kind: 'tap', finder: 'key:id:agree', weak: undefined, reason: undefined }
  );
  assert.deepStrictEqual(
    mapAction('locator.fill', 'internal:testid=[data-testid="username"s]', 'ada'),
    { kind: 'type', finder: 'key:testid:username', value: 'ada', weak: undefined, reason: undefined }
  );
  const g = mapAction('page.goto', null, 'http://localhost:8099/');
  assert.deepStrictEqual(g, { kind: 'goto', value: 'http://localhost:8099/' });
});

test('mapAction: a click on an unmappable selector is skipped (carries the raw)', () => {
  const m = mapAction('locator.click', 'xpath=//button[1]');
  assert.strictEqual(m.skip, true);
  assert.strictEqual(m.raw, 'xpath=//button[1]');
});

test('mapAction: non-action apis (expect/hover/waitFor) are skipped as non-action', () => {
  for (const api of ['expect.toBeVisible', 'locator.hover', 'locator.waitFor', 'page.screenshot']) {
    const m = mapAction(api, 'internal:testid=[data-testid="x"s]');
    assert.strictEqual(m.skip, true);
    assert.strictEqual(m.nonAction, true);
  }
});

test('buildResult: first goto -> gotoUrl+baseURL, later goto -> note, actions in order', () => {
  const raw = [
    { apiName: 'page.goto', selector: null, value: 'http://localhost:8099/sub' },
    { apiName: 'locator.fill', selector: 'internal:testid=[data-testid="u"s]', value: 'ada' },
    { apiName: 'locator.click', selector: 'internal:testid=[data-testid="go"s]' },
    { apiName: 'page.goto', selector: null, value: 'http://localhost:8099/other' },
    { apiName: 'locator.click', selector: 'xpath=//div' },
    { apiName: 'expect.toBeVisible', selector: 'internal:role=heading[name="X"s]' },
  ];
  const r = buildResult(raw);
  assert.strictEqual(r.gotoUrl, 'http://localhost:8099/sub');
  assert.strictEqual(r.baseURL, 'http://localhost:8099');
  assert.deepStrictEqual(r.actions.map((a) => a.action), [
    'type:key:testid:u=ada',
    'tap:key:testid:go',
  ]);
  // The xpath click became an honest unsupported entry; expect/non-action did not.
  assert.strictEqual(r.unsupported.length, 1);
  assert.ok(/xpath/.test(r.unsupported[0].reason));
  // The second goto became a note, not a dropped silent action.
  assert.ok(r.notes.some((n) => /extra page.goto/.test(n)));
});

test('buildResult: a weak getByRole (no name) replays but is noted', () => {
  const r = buildResult([
    { apiName: 'page.goto', selector: null, value: 'http://x/' },
    { apiName: 'locator.click', selector: 'internal:role=button' },
  ]);
  assert.deepStrictEqual(r.actions.map((a) => a.action), ['tap:role:button#0']);
  assert.ok(r.notes.some((n) => /weak finder/.test(n)));
});
