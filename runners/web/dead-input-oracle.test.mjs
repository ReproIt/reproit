import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import {
  classifyKeyProbe,
  classifyWheelProbe,
  deadInputProbe,
} from './dead-input-oracle.mjs';

test('wheel verdicts require arrival, no claim, and zero movement', () => {
  const still = { wheelSeen: true, wheelPrevented: false, scrolls: 0, topDelta: 0, winDelta: 0 };
  assert.equal(classifyWheelProbe({ owner: 'target' }, still), 'dead-scroll');
  assert.equal(
    classifyWheelProbe({ owner: 'blocker' }, still),
    'blocked-by-invisible-overlay',
  );
  // A prevented wheel is claimed by the app: abstain.
  assert.equal(classifyWheelProbe({ owner: 'target' }, { ...still, wheelPrevented: true }), null);
  // Anything scrolled anywhere: abstain.
  assert.equal(classifyWheelProbe({ owner: 'target' }, { ...still, scrolls: 1 }), null);
  assert.equal(classifyWheelProbe({ owner: 'target' }, { ...still, topDelta: 60 }), null);
  // Modal interceptors and visible occluders are never findings here.
  assert.equal(classifyWheelProbe({ owner: 'dialog' }, still), null);
  assert.equal(classifyWheelProbe({ owner: 'visible-interceptor' }, still), null);
  // The wheel never arrived (headless quirk, detached target): abstain.
  assert.equal(classifyWheelProbe({ owner: 'target' }, { ...still, wheelSeen: false }), null);
});

test('keystroke verdicts abstain whenever anyone claimed or used the key', () => {
  const dead = { keySeen: true, keyPrevented: false, inputs: 0 };
  assert.equal(classifyKeyProbe(dead, '', ''), 'dead-keystroke');
  assert.equal(classifyKeyProbe({ ...dead, keyPrevented: true }, '', ''), null);
  assert.equal(classifyKeyProbe({ ...dead, inputs: 1 }, '', 'a'), null);
  assert.equal(classifyKeyProbe(dead, '', 'a'), null);
  assert.equal(classifyKeyProbe({ ...dead, keySeen: false }, '', ''), null);
});

const PAGE = (body, head = '') => `<!doctype html><html><head><style>
  .list { height: 200px; width: 300px; overflow-y: auto; }
  .row { height: 40px; }
</style>${head}</head><body>${body}</body></html>`;

const ROWS = Array.from({ length: 30 }, (_, i) => `<div class="row">row ${i}</div>`).join('');

async function probeOn(html) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(html);
    return await deadInputProbe(page);
  } finally {
    await browser.close();
  }
}

test('planted: invisible non-dialog overlay blocking a scrollable list fires', async () => {
  const items = await probeOn(PAGE(`
    <div class="list" data-testid="feed">${ROWS}</div>
    <div style="position:fixed;inset:0;background:transparent;"
         onwheel="event.stopPropagation()"></div>
  `));
  assert.equal(items.length, 1);
  assert.equal(items[0].key, 'testid:feed');
  assert.equal(items[0].input, 'wheel:down');
  assert.match(items[0].context, /blocked by div/);
});

test('clean: a plain scrollable list stays silent and keeps its offset', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(PAGE(`<div class="list" data-testid="feed">${ROWS}</div>`));
    const items = await deadInputProbe(page);
    assert.deepEqual(items, []);
    // Non-destructive: the probe restored the scroll offset it moved.
    assert.equal(
      await page.evaluate(() => document.querySelector('.list').scrollTop),
      0,
    );
  } finally {
    await browser.close();
  }
});

test('abstain: a modal dialog over the list is intentional UX', async () => {
  const items = await probeOn(PAGE(`
    <div class="list">${ROWS}</div>
    <div role="dialog" aria-modal="true"
         style="position:fixed;inset:0;background:transparent;">
      <p style="position:absolute;left:-9999px;">settings</p>
    </div>
  `));
  assert.deepEqual(items, []);
});

test('abstain: a custom scroller that preventDefaults the wheel owns it', async () => {
  const items = await probeOn(PAGE(`
    <div class="list" id="virt">${ROWS}</div>
    <script>
      document.getElementById('virt').addEventListener(
        'wheel', (e) => e.preventDefault(), { passive: false });
    </script>
  `));
  assert.deepEqual(items, []);
});

test('clean: a normal text input accepts the probe key and is restored', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(PAGE('<input type="text" name="q">'));
    const items = await deadInputProbe(page);
    assert.deepEqual(items, []);
    // Non-destructive: the probe char was backspaced away.
    assert.equal(await page.evaluate(() => document.querySelector('input').value), '');
  } finally {
    await browser.close();
  }
});

test('abstain: a numeric mask that preventDefaults letters is a filter, not a bug', async () => {
  const items = await probeOn(PAGE(`
    <input type="text" name="amount">
    <script>
      document.querySelector('input').addEventListener('keydown', (e) => {
        if (!/[0-9]/.test(e.key)) e.preventDefault();
      });
    </script>
  `));
  assert.deepEqual(items, []);
});
