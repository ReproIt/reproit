// Per-seed state hygiene for the web runner: clearing client-side persistence on
// reset (so a state-persisting app does not leak state between seeds). Browser-backed via Playwright
// where a real engine is needed. Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { clearClientStorage } from './runner.mjs';

test('clearClientStorage wipes localStorage / sessionStorage / IndexedDB and calls the app reset hook', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // Serve over a real origin: localStorage/IndexedDB are denied on about:blank.
    await page.route('**/*', (r) => r.fulfill({ contentType: 'text/html', body: '<!doctype html><body>persist</body>' }));
    await page.goto('http://example.test/');
    // Seed persistence the way a state-persisting app (TodoMVC) would, plus an
    // app-provided reset hook we expect to be invoked.
    await page.evaluate(async () => {
      localStorage.setItem('todos', '[{"t":"buy milk","done":false}]');
      sessionStorage.setItem('draft', 'unsent');
      window.__reproitResetCalled = false;
      window.__reproitReset = () => { window.__reproitResetCalled = true; };
      if (window.indexedDB) {
        await new Promise((res) => {
          const r = indexedDB.open('appdb', 1);
          r.onupgradeneeded = () => r.result.createObjectStore('kv');
          r.onsuccess = () => { r.result.close(); res(); };
          r.onerror = () => res();
          r.onblocked = () => res();
        });
      }
    });

    await clearClientStorage(page);

    const after = await page.evaluate(async () => {
      let dbNames = [];
      try {
        if (indexedDB.databases) dbNames = (await indexedDB.databases()).map((d) => d.name);
      } catch (_) { /* older engines: skip the IDB assertion */ dbNames = null; }
      return {
        ls: localStorage.length,
        ss: sessionStorage.length,
        hookCalled: window.__reproitResetCalled === true,
        dbNames,
      };
    });
    assert.strictEqual(after.ls, 0, 'localStorage must be cleared so the next seed starts clean');
    assert.strictEqual(after.ss, 0, 'sessionStorage must be cleared');
    assert.strictEqual(after.hookCalled, true, 'an app-provided window.__reproitReset() hook must be called');
    if (Array.isArray(after.dbNames)) {
      assert.ok(!after.dbNames.includes('appdb'), `IndexedDB database must be deleted, saw ${after.dbNames}`);
    }
  } finally {
    await browser.close();
  }
});
