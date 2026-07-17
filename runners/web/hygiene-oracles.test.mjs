// Zero-FP hardening for the shared DOM-hygiene oracles (occlusion and security
// tabnabbing). Each check is browser-backed
// via Playwright over static markup, so the DOM predicate is exercised on a real
// engine. Run `node --test`.
//
// The through-line is the hardening pass that brought these oracles to near-zero
// false positives on well-maintained sites (vuejs.org, react.dev, bootstrap, ...)
// while KEEPING a genuine-bug positive for each, so the oracle is tightened, not
// neutered.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import {
  occlusionScan,
  confirmOcclusions,
  securityScan,
  indicatorRelationshipScan,
  confirmRelationshipViolations,
  focusLossArm,
  focusLossCheck,
  scrollRoundTripScan,
  zoomTappableKeys,
  zoomReflowScan,
} from './hygiene-oracles.mjs';

// ── EXPLICIT INDICATOR RELATIONSHIPS ──────────────────────────────────────

test(
  'indicator relationship PROVES a detached indicator from explicit ' + 'structure',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><body>' +
          ('<nav id="bottom-nav" data-reproit-indicator-container style="position:' +
            'relative;width:500px;height:80px">') +
          ('<button id="liked" data-reproit-indicator-owner style="position:' +
            'absolute;left:200px;top:20px;width:100px;height:40px">Liked You</' +
            'button>') +
          ('<span id="liked-dot" data-reproit-indicator-for="liked" ' +
            'style="position:absolute;left:245px;top:150px;width:12px;height:' +
            '12px"></span>') +
          '</nav></body>',
      );
      const result = await page.evaluate(indicatorRelationshipScan);
      assert.strictEqual(result.outcome, 'PROVEN');
      assert.deepStrictEqual(
        result.items.map((item) => ({
          kind: item.kind,
          dependentKey: item.dependentKey,
          ownerKey: item.ownerKey,
          containerKey: item.containerKey,
          violation: item.violation,
        })),
        [
          {
            kind: 'indicator-anchor',
            dependentKey: 'key:id:liked-dot',
            ownerKey: 'key:id:liked',
            containerKey: 'key:id:bottom-nav',
            violation: 'escaped-container',
          },
        ],
      );
    } finally {
      await browser.close();
    }
  },
);

test(
  'indicator relationship is VALID when the explicit indicator stays ' + 'attached',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><body>' +
          ('<nav id="bottom-nav" data-reproit-indicator-container style="position:' +
            'relative;width:500px;height:80px">') +
          ('<button id="liked" data-reproit-indicator-owner style="position:' +
            'absolute;left:200px;top:20px;width:100px;height:40px">Liked You</' +
            'button>') +
          ('<span id="liked-dot" data-reproit-indicator-for="liked" ' +
            'style="position:absolute;left:292px;top:18px;width:12px;height:12px"></' +
            'span>') +
          '</nav></body>',
      );
      const result = await page.evaluate(indicatorRelationshipScan);
      assert.strictEqual(result.outcome, 'VALID');
      assert.deepStrictEqual(result.items, []);
    } finally {
      await browser.close();
    }
  },
);

test(
  'indicator relationship is UNKNOWN and silent without complete explicit ' + 'ownership',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // Looks like a detached red badge to a person, but has no structural contract.
      await page.setContent(
        '<!doctype html><body><button id="liked">Liked You</button>' +
          ('<span id="red-dot" style="position:absolute;top:500px;width:12px;' +
            'height:12px;border-radius:50%;background:red"></span></body>'),
      );
      assert.deepStrictEqual(await page.evaluate(indicatorRelationshipScan), {
        outcome: 'UNKNOWN',
        items: [],
        checks: [],
        proven: 0,
        valid: 0,
        unknown: 0,
      });
      // A partial contract also abstains rather than guessing the intended owner.
      await page
        .locator('#red-dot')
        .evaluate((el) => el.setAttribute('data-reproit-indicator-for', 'liked'));
      const partial = await page.evaluate(indicatorRelationshipScan);
      assert.strictEqual(partial.outcome, 'UNKNOWN');
      assert.strictEqual(partial.unknown, 1);
      assert.deepStrictEqual(partial.items, []);
    } finally {
      await browser.close();
    }
  },
);

test('indicator relationship abstains while its declared nodes animate', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(
      '<!doctype html><style>@keyframes drift{to{transform:translateY(100px)}}' +
        '</style><body>' +
        ('<nav id="nav" data-reproit-indicator-container style="position:' +
          'relative;width:400px;height:80px">') +
        ('<button id="inbox" data-reproit-indicator-owner style="width:100px;' +
          'height:40px">Inbox</button>') +
        ('<span id="dot" data-reproit-indicator-for="inbox" style="position:' +
          'absolute;left:300px;top:50px;width:10px;height:10px;animation:drift ' +
          '10s linear infinite"></span>') +
        '</nav></body>',
    );
    const result = await page.evaluate(indicatorRelationshipScan);
    assert.strictEqual(result.outcome, 'UNKNOWN');
    assert.deepStrictEqual(result.items, []);
  } finally {
    await browser.close();
  }
});

test(
  'indicator relationship confirmation requires the same structural ' + 'violation twice',
  () => {
    const detached = {
      kind: 'indicator-anchor',
      dependentKey: 'key:id:dot',
      ownerKey: 'key:id:tab',
      containerKey: 'key:id:nav',
      violation: 'detached',
      gap: 90,
    };
    const escaped = { ...detached, violation: 'escaped-container' };
    assert.deepStrictEqual(
      confirmRelationshipViolations({ items: [detached] }, { items: [{ ...detached, gap: 91 }] }),
      [detached],
    );
    assert.deepStrictEqual(
      confirmRelationshipViolations({ items: [detached] }, { items: [escaped] }),
      [],
    );
    assert.deepStrictEqual(confirmRelationshipViolations(null, { items: [detached] }), []);
  },
);

test(
  'zoom reflow ignores offscreen controls and responsive breakpoint ' + 'replacement',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><style>' +
          ('#desktop{width:120px;height:32px}@media(max-width:500px)' +
            '{#desktop{position:absolute;left:-9999px;width:0}#mobile{display:block}' +
            '}') +
          '#mobile{display:none}</style><body>' +
          ('<a id="skip" href="#main" style="position:absolute;left:-9999px">Skip</' + 'a>') +
          ('<button id="desktop">Search</button><button id="mobile">Menu</' +
            'button><main id="main">Main</main>'),
      );
      const pre = await page.evaluate(zoomTappableKeys);
      assert.ok(
        !pre.some((x) => x.key === 'key:id:skip'),
        'offscreen skip link must not enter baseline',
      );
      await page.setViewportSize({ width: 400, height: 600 });
      assert.deepStrictEqual(
        await page.evaluate(zoomReflowScan, pre),
        [],
        'responsive replacement is not collapse',
      );
    } finally {
      await browser.close();
    }
  },
);

test('zoom reflow still reports a genuinely collapsed in-place control', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(
      '<!doctype html><style>@media(max-width:500px){#buy{width:0!important;' +
        'height:0!important;padding:0!important;border:0!important;overflow:' +
        'hidden}}</style>' +
        ('<button id="buy" style="position:absolute;left:20px;top:20px;width:' +
          '100px;height:32px">Buy</button>'),
    );
    const pre = await page.evaluate(zoomTappableKeys);
    await page.setViewportSize({ width: 400, height: 600 });
    const out = await page.evaluate(zoomReflowScan, pre);
    assert.ok(
      out.some((x) => x.key === 'key:id:buy' && x.kind === 'collapsed'),
      JSON.stringify(out),
    );
  } finally {
    await browser.close();
  }
});

test(
  'scroll round-trip fires only for stable same-shape rows with changed ' + 'content',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><body style="margin:0"><div id="list" style="width:' +
          '400px;height:180px;overflow-y:auto"></div>',
      );
      await page.locator('#list').evaluate((list) => {
        for (let i = 0; i < 20; i++) {
          const row = document.createElement('div');
          row.style.height = '60px';
          row.textContent = 'Row ' + i;
          list.appendChild(row);
        }
        let bottom = false;
        list.addEventListener('scroll', () => {
          if (list.scrollTop > 500) bottom = true;
          if (bottom && list.scrollTop === 0) list.children[0].textContent = 'Wrong row';
        });
      });
      const result = await page.evaluate(scrollRoundTripScan);
      assert.ok(result.length > 0, 'same row shape rebound to different content must fire');
    } finally {
      await browser.close();
    }
  },
);

test(
  'scroll round-trip ignores a leaf-to-container sample while ' + 'virtualization settles',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><body style="margin:0"><div id="list" style="width:' +
          '400px;height:180px;overflow-y:auto"></div>',
      );
      await page.locator('#list').evaluate((list) => {
        for (let i = 0; i < 20; i++) {
          const row = document.createElement('span');
          row.style.cssText = 'display:block;height:60px';
          row.textContent = 'Row ' + i;
          list.appendChild(row);
        }
        let bottom = false;
        list.addEventListener('scroll', () => {
          if (list.scrollTop > 500) bottom = true;
          if (bottom && list.scrollTop === 0)
            list.replaceChildren(
              Object.assign(document.createElement('div'), {
                textContent: 'Loading all virtual rows together',
              }),
            );
        });
      });
      const result = await page.evaluate(scrollRoundTripScan);
      assert.deepStrictEqual(result, [], 'non-comparable post-return container must not fire');
    } finally {
      await browser.close();
    }
  },
);

// ── OCCLUSION ───────────────────────────────────────────────────────────────

test('occlusion FIRES on a real button covered by a foreign opaque overlay', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // A genuinely usable button with a stray opaque element mispositioned on top
    // of it (a z-index accident): the user sees the button but a click lands on
    // the red box. Not chrome, not an overlay/modal, not viewport-spanning.
    await page.setContent(
      '<!doctype html><body style="margin:0">' +
        ('<button id="real-btn" style="position:absolute;left:60px;top:60px;' +
          'width:120px;height:40px">Buy now</button>') +
        ('<div id="stray" style="position:absolute;left:50px;top:50px;width:' +
          '150px;height:70px;background:#c00;z-index:99"></div>') +
        '</body>',
    );
    const out = await page.evaluate(occlusionScan);
    assert.ok(
      out.some((o) => o.target === 'key:id:real-btn'),
      `expected the covered button to fire, got ${JSON.stringify(out)}`,
    );
  } finally {
    await browser.close();
  }
});

test(
  'occlusion is SILENT on a control inside a CLOSED flyout (opacity:0 ' + 'ancestor)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // A nav dropdown whose panel is collapsed via opacity:0 on an ANCESTOR (the
      // link's own computed opacity is 1, so the per-element check misses it). The
      // flyout button sits over the collapsed link. Reveal-on-hover pattern, not a bug.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          '<div style="position:relative;width:120px;height:32px">' +
          ('<button style="position:absolute;inset:0;width:120px;height:' +
            '32px">Ecosystem</button>') +
          '<div style="opacity:0;position:absolute;top:0;left:0">' +
          ('<a id="flyout-link" href="/x" style="display:block;width:120px;height:' +
            '32px">Awesome Vue</a>') +
          '</div></div></body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        !out.some((o) => o.target.includes('flyout-link')),
        `a closed-flyout link must not fire, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test('occlusion is SILENT on an off-screen sr-only skip-link', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(
      '<!doctype html><body style="margin:0">' +
        ('<a id="skip" href="#main" style="position:absolute;left:-9999px;top:0;' +
          'width:120px;height:30px">Skip to content</a>') +
        '<main id="main">content</main></body>',
    );
    const out = await page.evaluate(occlusionScan);
    assert.ok(
      !out.some((o) => o.target.includes('skip')),
      `an off-screen skip link must not fire, got ${JSON.stringify(out)}`,
    );
  } finally {
    await browser.close();
  }
});

test(
  'occlusion is SILENT on a link scrolled OUT of an overflow:auto ' + 'container (clipped away)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The svelte.dev tutorial-picker FP: a scrollable dropdown list (overflow:auto,
      // fixed height) whose lower items are scrolled past the clip box. Those links
      // keep their layout rect, which lands on whatever paints behind the container,
      // so elementFromPoint returns a foreign opaque element -- but the link is
      // CLIPPED AWAY, not covered. A sibling opaque pane behind the dropdown stands in
      // for the editor pane the real page hit-tested onto.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          ('<div style="position:absolute;left:0;top:0;width:400px;height:400px;' +
            'background:#0a2;z-index:0"></div>') +
          ('<div style="position:absolute;left:0;top:0;overflow:auto;width:200px;' +
            'height:80px;z-index:1">') +
          '<a href="/a" style="display:block;height:30px">Row 1</a>' +
          '<a href="/b" style="display:block;height:30px">Row 2</a>' +
          ('<a id="clipped" href="/c" style="display:block;height:30px">Row 3 ' + 'clipped</a>') +
          ('<a id="clipped2" href="/d" style="display:block;height:30px">Row 4 ' + 'clipped</a>') +
          '</div></body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        !out.some((o) => o.target.includes('clipped')),
        'a link scrolled out of an overflow:auto viewport must not fire, got ' +
          JSON.stringify(out),
      );
    } finally {
      await browser.close();
    }
  },
);

test(
  'occlusion is SILENT on a control inside a CLOSED <details> (collapsed ' + 'disclosure)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The svelte.dev section-picker FP: menu links inside a closed examples-select
      // <details> stay laid out (custom disclosures animate height, keeping a real
      // rect) and hit-test onto the opaque article/code painted behind them. A
      // collapsed disclosure's body is not presented as clickable.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          ('<div style="position:absolute;left:0;top:0;width:400px;height:200px;' +
            'background:#c00;z-index:0"></div>') +
          ('<details style="position:absolute;left:0;top:0;z-index:' +
            '1"><summary>Sections</summary>') +
          ('<a id="menu-link" href="/styling" style="display:block;width:150px;' +
            'height:30px">Styling</a>') +
          '</details></body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        !out.some((o) => o.target.includes('menu-link')),
        `a link in a closed <details> must not fire, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test(
  'occlusion STILL FIRES on a buried link inside the SUMMARY of a closed ' + '<details>',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The <summary> stays shown when the <details> is closed, so a control there
      // that a foreign opaque box buries is a real occlusion -- the guard must not
      // over-suppress it.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          ('<div style="position:absolute;left:0;top:0;width:400px;height:60px;' +
            'background:#06c;z-index:5"></div>') +
          '<details style="position:absolute;left:0;top:0;z-index:0"><summary>' +
          ('<a id="sum-link" href="/x" style="display:inline-block;width:120px;' +
            'height:30px">Open</a>') +
          '</summary>body</details></body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        out.some((o) => o.target.includes('sum-link')),
        `a buried link in a closed details' summary must still fire, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test(
  'occlusion STILL FIRES on an in-view control opaquely covered inside a ' + 'scroll container',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The clip guard must not over-suppress: a control genuinely WITHIN the scroll
      // viewport that a foreign opaque box paints over is a real occlusion.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          '<div style="position:relative;overflow:auto;width:300px;height:300px">' +
          ('<button id="inview" style="position:absolute;left:20px;top:20px;width:' +
            '100px;height:40px">Save</button>') +
          ('<div style="position:absolute;left:10px;top:10px;width:150px;height:' +
            '80px;background:#06c"></div>') +
          '</div></body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        out.some((o) => o.target.includes('inview')),
        'an in-view control opaquely covered inside a scroll box must still fire, got ' +
          JSON.stringify(out),
      );
    } finally {
      await browser.close();
    }
  },
);

test('confirmOcclusions keeps a stable occlusion and drops a transient one', () => {
  const buried = { target: 'key:id:buy', cover: 'div#scrim' };
  const first = [buried, { target: 'key:id:clock', cover: 'span.icon' }];
  // Second frame (settled): the buried control persists; the menu-item transient
  // has cleared, and a different link now transiently overlaps a different cover.
  const second = [buried, { target: 'key:id:bar', cover: 'div.cm-line' }];
  assert.deepStrictEqual(confirmOcclusions(first, second), [buried]);
});

test(
  'confirmOcclusions drops a same-target occlusion whose cover SHIFTED ' + 'between frames',
  () => {
    // The svelte.dev transient: same control, different element underneath each
    // frame -> not a stable occlusion.
    const first = [{ target: 'key:id:x', cover: 'span.highlight' }];
    const second = [{ target: 'key:id:x', cover: 'i.drag-handle' }];
    assert.deepStrictEqual(confirmOcclusions(first, second), []);
  },
);

test('confirmOcclusions is empty when either frame is empty or non-array', () => {
  assert.deepStrictEqual(confirmOcclusions([{ target: 'a', cover: 'b' }], []), []);
  assert.deepStrictEqual(confirmOcclusions([], [{ target: 'a', cover: 'b' }]), []);
  assert.deepStrictEqual(confirmOcclusions(null, [{ target: 'a', cover: 'b' }]), []);
});

test(
  'occlusion is SILENT on an on-screen skip-link behind a fixed header ' + '(chrome cover)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The VitePress pattern: a skip-link at the top-left, revealed on focus, sits
      // behind the sticky navbar until then. The cover is site chrome, not a foreign
      // overlay.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          ('<header style="position:fixed;top:0;left:0;width:100%;height:60px;' +
            'background:#fff;z-index:10">Nav</header>') +
          ('<a id="skip2" href="#main" style="position:absolute;left:8px;top:8px;' +
            'width:140px;height:30px">Skip to content</a>') +
          '</body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        !out.some((o) => o.target.includes('skip2')),
        `a skip-link behind a fixed header must not fire, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test('occlusion is SILENT on background controls behind an open modal ' + 'backdrop', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(
      '<!doctype html><body style="margin:0">' +
        ('<button id="bg-btn" style="position:absolute;left:60px;top:120px;width:' +
          '120px;height:40px">Behind</button>') +
        ('<div class="modal-backdrop" style="position:fixed;inset:0;background:' +
          'rgba(0,0,0,.5);z-index:100"></div>') +
        '</body>',
    );
    const out = await page.evaluate(occlusionScan);
    assert.ok(
      !out.some((o) => o.target.includes('bg-btn')),
      `a control behind an open modal backdrop must not fire, got ${JSON.stringify(out)}`,
    );
  } finally {
    await browser.close();
  }
});

test(
  'occlusion is SILENT on a styled checkbox (label covering its ' + 'visually-hidden input)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // The Bootstrap .btn-check pattern: a visually-hidden <input> driven through an
      // opaque <label class="btn"> stacked over it. The label IS the affordance.
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          ('<input type="checkbox" id="btn-check" style="position:absolute;left:' +
            '20px;top:20px;width:40px;height:40px;opacity:.01">') +
          ('<label class="btn" for="btn-check" style="position:absolute;left:20px;' +
            'top:20px;width:120px;height:40px;background:#0d6efd;color:#fff">Bold</' +
            'label>') +
          '</body>',
      );
      const out = await page.evaluate(occlusionScan);
      assert.ok(
        !out.some((o) => o.target.includes('btn-check')),
        `a label-over-input styled checkbox must not fire, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

// ── SECURITY (reverse tabnabbing) ─────────────────────────────────────────────

test(
  'tabnabbing FIRES only on an explicit rel="opener" cross-origin _blank ' + 'link',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage();
      await page.goto('https://example.com/', { waitUntil: 'domcontentloaded' }).catch(() => {});
      await page.setContent(
        '<!doctype html><body>' +
          ('<a id="vuln" href="https://evil.example.org/" target="_blank" ' +
            'rel="opener">deliberate opener</a>') +
          ('<a id="plain" href="https://other.example.org/" target="_blank">plain ' +
            'blank (safe by default)</a>') +
          ('<a id="safe" href="https://other.example.org/" target="_blank" ' +
            'rel="noopener">noopener</a>') +
          ('<a id="same" href="/local" target="_blank" rel="opener">same-origin ' + 'opener</a>') +
          '</body>',
      );
      const out = await page.evaluate(securityScan);
      const tab = out.filter((o) => o.kind === 'tabnabbing');
      assert.strictEqual(
        tab.length,
        1,
        `exactly one tabnabbing (the rel=opener x-origin), got ${JSON.stringify(out)}`,
      );
      assert.ok(
        tab[0].target.includes('deliberate opener'),
        `the rel=opener link, got ${JSON.stringify(tab)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test(
  'tabnabbing is SILENT on a plain target=_blank (modern browsers imply ' + 'noopener)',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage();
      await page.goto('https://example.com/', { waitUntil: 'domcontentloaded' }).catch(() => {});
      await page.setContent(
        '<!doctype html><body>' +
          '<a href="https://a.example.org/" target="_blank">one</a>' +
          ('<a href="https://b.example.org/" target="_blank" rel="noreferrer">two</' + 'a>') +
          '</body>',
      );
      const out = await page.evaluate(securityScan);
      assert.ok(
        !out.some((o) => o.kind === 'tabnabbing'),
        `plain _blank links must not fire tabnabbing, got ${JSON.stringify(out)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

// ── FOCUS-LOSS ────────────────────────────────────────────────────────────────

test(
  'focus-loss: a fresh click on a never-focused button is NOT a loss; a ' +
    'loss from an already-focused element IS',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><body style="margin:0">' +
          '<button id="btn">Go</button><input id="inp"></body>',
      );
      await page.addScriptTag({
        content:
          `window.armRef = ${focusLossArm.toString()}; ` +
          `window.checkRef = ${focusLossCheck.toString()};`,
      });

      // FALSE POSITIVE (the platform artifact that fired on every ordinary button on
      // the Electron/Tauri clean apps): a fresh mouse activation of a NEVER-focused
      // button. macOS Chromium / WebKitGTK do not focus a button on mouse click, so
      // focus stays on <body>. The synthetic probe focus (__reproitTapFocused=true)
      // must NOT count as "had focus", so this is suppressed.
      const fp = await page.evaluate(() => {
        if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
        armRef(); // __reproitFocusPre = <body> (nothing focused)
        window.__reproitLastTap = document.getElementById('btn');
        window.__reproitTapFocused = true; // the probe's el.focus() "succeeded"
        if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
        return checkRef(); // activeElement is <body>
      });
      assert.strictEqual(
        fp,
        false,
        'a click on a never-focused button ending on <body> is a platform ' +
          'artifact, not a loss',
      );

      // TRUE POSITIVE: the user TABBED to THIS control (it held focus), activated it,
      // and the re-render dropped its focus to <body> while the control survives. A
      // real keyboard focus loss: must still fire.
      const tp = await page.evaluate(() => {
        const btn = document.getElementById('btn');
        btn.focus(); // genuine pre-focus ON the tapped control
        armRef(); // __reproitFocusPre = the button itself
        window.__reproitLastTap = btn; // ... and it IS the tapped control
        window.__reproitTapFocused = false;
        if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
        return checkRef(); // activeElement is <body>
      });
      assert.strictEqual(
        tp,
        true,
        'a focused control that loses ITS OWN focus to <body> on activation is ' + 'a real loss',
      );

      // NOT A LOSS: focus sat on a DIFFERENT element (an input the user typed into,
      // or a leftover synthetic focus from the previous action) and a separate
      // control was tapped. That is not the tapped control losing its own focus, so
      // it must be suppressed (this is what kept the clean apps firing after the
      // first-cut fix: type-then-tap and force-focus carryover).
      const other = await page.evaluate(() => {
        document.getElementById('inp').focus(); // focus was on the INPUT
        armRef(); // __reproitFocusPre = the input
        window.__reproitLastTap = document.getElementById('btn'); // a DIFFERENT control tapped
        if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
        return checkRef();
      });
      assert.strictEqual(
        other,
        false,
        'focus on a different element than the tapped control is not this ' +
          'control losing focus',
      );
    } finally {
      await browser.close();
    }
  },
);
