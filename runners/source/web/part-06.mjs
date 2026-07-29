  const drew = await page
    .evaluate(
      async ({ trigger, flickerKeys, oracle, linkHref }) => {
        try {
          clearInterval(window.__reproitBoxHeal);
        } catch (_) {}
        const old = document.getElementById('__reproit_boxes');
        if (old) old.remove();
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        const sx = window.scrollX,
          sy = window.scrollY;
        // {prio,mag} orders findings by user-visible impact.
        const hits = [];
        const push = (el, label, prio, mag, cat, rect) => {
          // rect overrides the element box (a range-tightened text rect).
          const r = rect || el.getBoundingClientRect();
          hits.push({
            top: r.top + sy,
            left: r.left + sx,
            w: r.width,
            h: r.height,
            label,
            prio,
            mag,
            el,
            cat,
          });
        };
        const all = document.body ? document.body.querySelectorAll('*') : [];
        // Content-bug artifacts: the literal broken-stringify tokens, on the OWN
        // text of an element (mirrors detectContentBugs' reasonOf).
        const ownText = (el) => {
          let t = '';
          for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
          return t.replace(/\s+/g, ' ').trim();
        };
        const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
        const reasonOf = (text) => {
          if (!text) return null;
          // Same prose guard as detectContentBugs for both artifact kinds.
          if (text.includes('[object Object]')) {
            const s = text
              .replace(/\[object Object\]/g, ' ')
              .replace(/\s+/g, ' ')
              .trim();
            if (dominates(s)) return '[object Object]';
          }
          if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
            const s = text
              .replace(/\{\{[^}]*\}\}/g, ' ')
              .replace(/\$\{[^}]*\}/g, ' ')
              .replace(/\s+/g, ' ')
              .trim();
            if (dominates(s)) return 'unrendered template';
          }
          return null;
        };
        // Skip a CODE context (mirrors detectContentBugs): template/markup syntax
        // shown as documentation is not a leaked binding.
        const CODE_TAGS = new Set(['code', 'pre', 'script', 'style', 'textarea']);
        const inCodeContext = (el) => {
          if (el.isContentEditable) return true;
          for (let n = el; n && n !== document.body; n = n.parentElement) {
            if (CODE_TAGS.has(n.tagName.toLowerCase())) return true;
          }
          return false;
        };
        const seenC = new Set();
        for (const el of all) {
          if (!visible(el)) continue;
          if (inCodeContext(el)) continue;
          const reason = reasonOf(ownText(el));
          if (!reason || seenC.has(el)) continue;
          seenC.add(el);
          push(el, 'content  ' + reason, 4, 1e6, 'content');
        }
        // TRIGGER element (crash / jank / hang): the control the failing action
        // targeted, tagged at click/focus time. Highest priority - it IS the bug
        // the user reproduces - so it sorts first and is the one scrolled to.
        if (trigger) {
          const t = document.querySelector('[data-reproit-trigger]');
          if (t && visible(t)) push(t, trigger, 5, 2e6, 'trigger');
        }
        // FLICKER: the persistent-chrome anchors that were rebuilt though their box
        // and text were unchanged. Resolve each key back to a live node by the same
        // id/testid/tag[role] grammar markAnchors used (first visible match).
        if (flickerKeys && flickerKeys.length) {
          const keyToEl = (key) => {
            const ci = key.indexOf(':');
            const kind = key.slice(0, ci),
              val = key.slice(ci + 1);
            if (kind === 'id') return document.getElementById(val);
            if (kind === 'testid')
              return (
                document.querySelector('[data-testid="' + val + '"]') ||
                document.querySelector('[data-test-id="' + val + '"]')
              );
            if (kind === 'tag') {
              const m = val.match(/^([a-z0-9-]+)(?:\[([a-z]+)\])?$/i);
              if (!m) return null;
              const sel = m[2] ? m[1] + '[role="' + m[2] + '"]' : m[1];
              for (const el of document.querySelectorAll(sel)) if (visible(el)) return el;
            }
            return null;
          };
          const seenF = new Set();
          for (const k of flickerKeys) {
            const el = keyToEl(k);
            if (el && !seenF.has(el) && visible(el)) {
              seenF.add(el);
              push(el, 'flicker  rebuilt', 2, 5e5, 'flicker');
            }
          }
        }
        // BROKEN-ROUTE: the source link whose navigation target is the dead route.
        // Box the <a> on THIS (source) page, captioned with its visible text +
        // href, so the bad link is locatable where a person would click it.
        if (linkHref) {
          for (const a of document.querySelectorAll('a[href]')) {
            if (!visible(a)) continue;
            const raw = a.getAttribute('href') || '';
            // A same-page fragment (#...) resolves to THIS page's pathname and
            // can never be the dead route; without this guard a "Skip to
            // Content" link matched the source path and the box landed on a
            // visually hidden element.
            if (raw.startsWith('#')) continue;
            let path = '';
            try {
              const target = new URL(raw, location.href);
              path = target.pathname + target.search;
            } catch (e) {
              continue;
            }
            if (path !== linkHref) continue;
            const txt = (a.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
            // A glyphless anchor (an image-overlay link) renders nothing of its
            // own, so a bare box reads as "a box around nothing". Caption it as
            // the image/overlay link it is, named by alt/aria-label when present.
            const img =
              a.querySelector('img') || (a.parentElement && a.parentElement.querySelector('img'));
            const label =
              txt ||
              ((img && img.getAttribute('alt')) || a.getAttribute('aria-label') || '')
                .replace(/\s+/g, ' ')
                .trim()
                .slice(0, 40);
            const kind = txt ? 'broken link' : img ? 'broken image link' : 'broken overlay link';
            // Tighten a block-level anchor's box to its rendered text so the box
            // hugs what a person sees instead of the full container width.
            let rect = null;
            if (txt) {
              try {
                const rg = document.createRange();
                rg.selectNodeContents(a);
                const rr = rg.getBoundingClientRect();
                if (rr.width > 0 && rr.height > 0) rect = rr;
              } catch (e) {
                /* keep the element rect */
              }
            }
            push(
              a,
              kind + '  ' + (label ? '"' + label + '" → ' : '') + linkHref,
              5,
              3e6,
              'link',
              rect,
            );
            break;
          }
        }
        // SCOPE to the replayed finding's oracle: when this clip is one specific
        // repro (a gallery clip), box ONLY that finding's category and show a
        // SINGLE box, so each video is "just that issue", not every problem on the
        // page. The oracle name is the invariant the repro reproduces. Without a
        // hint (a generic record) keep the old behavior: all categories, up to 6.
        // Map the repro's oracle to a box category by keyword. Oracles with no on-screen element
        // Whole-process findings such as leak map to null and draw nothing.
        const catOf = (o) => {
          if (!o) return null;
          if (o.includes('broken-render') || o.includes('content')) return 'content';
          if (o.includes('flicker')) return 'flicker';
          if (o.includes('broken-route') || o.includes('not-found')) return 'link';
          if (
            o.includes('exception') ||
            o.includes('crash') ||
            o.includes('jank') ||
            o.includes('hang') ||
            o.includes('choice')
          )
            return 'trigger';
          return null;
        };
        let scoped;
        let cap;
        if (oracle) {
          const wantCat = catOf(oracle);
          // An oracle with no on-screen element (such as leak) draws nothing
          // rather than falling back to boxing unrelated issues.
          if (!wantCat) return false;
          scoped = hits.filter((h) => h.cat === wantCat);
          cap = 1; // a per-finding clip shows a SINGLE box: just that issue
        } else {
          scoped = hits;
          cap = 6;
        }
        if (!scoped.length) return false;
        // De-dupe nested hits (keep the outer), prioritize, cap.
        scoped.sort((a, b) => b.prio - a.prio || b.mag - a.mag);
        const chosen = [];
        for (const h of scoped) {
          // Skip a hit already covered by a higher-priority one: the same
          // element or an outer element that contains it.
          if (chosen.some((c) => c.el === h.el || c.el.contains(h.el))) continue;
          chosen.push(h);
          if (chosen.length >= cap) break;
        }
        // Bring the top offender into the recorded frame, HUMAN-PACED: a smooth
        // eased scroll, then WAIT FOR IT TO SETTLE before drawing. A fixed delay
        // is too short on a long page (the smooth scroll outlasts it), so the box
        // anchored to a mid-glide viewport and ended up off-screen once the scroll
        // finished -- the "clip shows no box" bug. Poll scrollY until it stops.
        try {
          const fr = chosen[0].el.getBoundingClientRect();
          const fvh = window.innerHeight || document.documentElement.clientHeight;
          const fvw = window.innerWidth || document.documentElement.clientWidth;
          const fInView = fr.top >= 0 && fr.left >= 0 && fr.bottom <= fvh && fr.right <= fvw;
          if (!fInView)
            chosen[0].el.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
        } catch (_) {}
        {
          let lastY = -1,
            stable = 0;
          for (let i = 0; i < 50; i++) {
            await new Promise((r) => setTimeout(r, 50));
            const y = window.scrollY;
            if (y === lastY) {
              if (++stable >= 3) break;
            } else {
              stable = 0;
              lastY = y;
            }
          }
        }
        const vx = window.scrollX,
          vy2 = window.scrollY;
        const vw = window.innerWidth || document.documentElement.clientWidth;
        const vh = window.innerHeight || document.documentElement.clientHeight;
        const layer = document.createElement('div');
        layer.id = '__reproit_boxes';
        layer.style.cssText =
          'position:absolute;top:0;left:0;width:0;height:0;z-index:2147483646;' +
          'pointer-events:none';
        for (const h of chosen) {
          const box = document.createElement('div');
          // CLAMP the box to the visible viewport (with an inset): an element bigger
          // than the viewport (a horizontally-overflowing carousel, a full-bleed
          // banner) drew its true bounds entirely off-frame, so nothing showed.
          // A fully-visible element is unchanged (the clamps are no-ops).
          const ins = 8;
          // Clamp the box fully INSIDE the viewport on BOTH axes. The old clamp
          // only pulled a box's NEAR edge in, so an element entirely off to the
          // right (a horizontal marquee/carousel whose box left > viewport right)
          // kept its off-screen left and drew nothing on camera -- the "overflow
          // clip shows no box" bug on dynamic sites. Pin the near edge into
          // [inset, viewport - inset - 8] so a box always lands on screen, at the
          // edge nearest the offender. A fully-visible element is unchanged.
          const bl = Math.min(Math.max(h.left - 2, vx + ins), vx + vw - ins - 8);
          const bt = Math.min(Math.max(h.top - 2, vy2 + ins), vy2 + vh - ins - 8);
          const br = Math.min(Math.max(h.left + h.w + 2, bl + 8), vx + vw - ins);
          const bb = Math.min(Math.max(h.top + h.h + 2, bt + 8), vy2 + vh - ins);
          const bw = Math.max(8, br - bl);
          const bh = Math.max(8, bb - bt);
          box.style.cssText = [
            'position:absolute',
            'top:' + bt + 'px',
            'left:' + bl + 'px',
            'width:' + bw + 'px',
            'height:' + bh + 'px',
            'border:3px solid #e21f1f',
            'background:rgba(226,31,31,.10)',
            'border-radius:4px',
            'box-shadow:0 0 0 1px rgba(255,255,255,.5),0 4px 18px rgba(0,0,0,.35)',
            'pointer-events:none',
          ].join(';');
          const tag = document.createElement('div');
          tag.textContent = h.label;
          // Sit the label above the box, but flip it just inside the top edge when
          // the box hugs the viewport top (a clamped/banner box) so it stays on-screen.
          const labelTop = bt - vy2 < 24 ? 3 : -22;
          tag.style.cssText = [
            'position:absolute',
            'top:' + labelTop + 'px',
            'left:-3px',
            'background:#e21f1f',
            'color:#fff',
            'font:600 12px/1 ui-monospace,SFMono-Regular,Menlo,monospace',
            'padding:4px 7px',
            'border-radius:5px',
            'white-space:nowrap',
            'box-shadow:0 2px 8px rgba(0,0,0,.4)',
          ].join(';');
          box.appendChild(tag);
          layer.appendChild(box);
        }
        (document.body || document.documentElement).appendChild(layer);
        // Self-heal: some sites (a React/Next route-transition re-render) detach
        // injected nodes on their next reconcile, so the box flashed once then
        // vanished mid-clip. Re-attach it for a bounded window so it stays on
        // camera through the hold. Auto-stops; the box-removal sites clear it.
        try {
          clearInterval(window.__reproitBoxHeal);
        } catch (_) {}
        let heals = 0;
        window.__reproitBoxHeal = setInterval(() => {
          if (!document.getElementById('__reproit_boxes')) {
            (document.body || document.documentElement).appendChild(layer);
          }
          if (++heals >= 24) {
            clearInterval(window.__reproitBoxHeal);
            window.__reproitBoxHeal = null;
          }
        }, 150);
        return chosen.length > 0;
      },
      {
        trigger: hints.triggerLabel || null,
        flickerKeys: hints.flickerKeys || null,
        oracle: hints.oracle || null,
        linkHref: hints.linkHref || null,
      },
    )
    .catch(() => false);
  // TRUST GATE: tell the Rust side whether the box actually drew, so a clip that
  // did not reproduce the finding on this load is dropped rather than shipped
  // with a misleading caption.
  log('FINDING:BOXED ' + JSON.stringify({ oracle: hints.oracle || null, drew: !!drew }));
  return !!drew;
}

// ---- COMPONENT-CHOICE differential fuzzing ----
// A multi-choice component (language tabs, a radio group) where EVERY choice has
// a similar effect (the common, expected behavior) but ONE choice deviates is a
// real bug. We exhaustively select each option and flag the one whose effect on
// the GLOBAL layout (the page OUTSIDE the component) is an OUTLIER vs its
// siblings - differential, not an absolute floor. If all choices behave alike
// (every language merely resizes the code block), NOTHING is flagged. This is
// what catches "only Go shifts the whole page" without the false positives an
// absolute layout-shift threshold produced.
// CHOICE_OUTLIER_RATIO / CHOICE_MIN_MAGNITUDE come from ./choice-oracle.mjs (the
// single source of truth shared with the electron + tauri ports); only the role
// SET is local here (detectChoiceGroups wants O(1) membership).
const CHOICE_ROLES = new Set(['tab', 'radio', 'menuitemradio']);

// Group the snapshot's choice-role tappables into mutually-exclusive option sets
// (>= 2 options). Scoped by the OWNING choice container (cgrp), so two separate
// tablists/radiogroups on one page are distinct components, not one merged group
// (comparing across independent components produced false outliers). When no
// container owns the options, the role alone is the key (the prior v1 behavior).
function detectChoiceGroups(tappables) {
  const groups = [];
  const claimed = new Set();
  // 1) ARIA choice roles: a set of tab/radio/menuitemradio options, partitioned
  // by `role|owning-container` so independent groups never merge.
  const byRole = new Map();
  for (const t of tappables) {
    if (CHOICE_ROLES.has(t.role)) {
      const key = t.role + '|' + (t.cgrp != null ? t.cgrp : 'role');
      if (!byRole.has(key)) byRole.set(key, []);
      byRole.get(key).push(t);
    }
  }
  for (const opts of byRole.values()) {
    if (opts.length >= 2) {
      groups.push({ role: opts[0].role, opts });
      for (const o of opts) claimed.add(o.sel);
    }
  }
  // 2) Button-cluster pickers (no ARIA choice role, e.g. a code-block language
  // switcher rendered as plain buttons): a set of >=3 same-parent, same-role
  // tappables where EXACTLY ONE is selected. The one-of-N selected state is what
  // separates a mutually-exclusive choice group from a row of action buttons
  // (Save/Delete, none selected), so we never blindly tap a Delete.
  const byGrp = new Map();
  for (const t of tappables) {
    // Only plain BUTTONS (links navigate, they are not a choice picker), with a
    // label (a real picker labels every option).
    if (claimed.has(t.sel) || t.role !== 'button' || !t.label || t.grp == null || t.grp < 0)
      continue;
    if (!byGrp.has(t.grp)) byGrp.set(t.grp, []);
    byGrp.get(t.grp).push(t);
  }
  for (const opts of byGrp.values()) {
    if (opts.length >= 3 && opts.filter((o) => o.selected).length === 1) {
      groups.push({ role: 'button-cluster', opts });
    }
  }
  return groups;
}

// FEATURE 1: native <select> as a multi-choice component. The snapshot maps a
// <select> to a `textfield` role, so detectChoiceGroups (which keys off ARIA
// choice roles / button clusters) never sees it -- the most common real-world
// picker. Here we query the page for visible <select>s with >= 3 enabled
// <option>s and return a choice group per select, keyed by a stable structural
// selector (data-testid > name) so the same picker re-resolves across the
// option-by-option exercise even as the framework re-renders. Each option carries
// its raw `value` (the thing we set on the element), exercised below by setting
// select.value + dispatching change/input so a bound framework reacts. The group
// shape mirrors the ARIA/button groups so exerciseChoiceGroup difffs it with the
// SAME global-layout measurement and the SAME outlier rule; the only difference
// is how an option is selected (set value vs click), branched on group.role.
async function detectSelectGroups(page) {
  const raw = await page
    .evaluate(() => {
      const visible = (el) => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        const st = getComputedStyle(el);
        return st.visibility !== 'hidden' && st.display !== 'none';
      };
      const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
      const keyOf = (el) => {
        const tid = (
          el.getAttribute('data-testid') ||
          el.getAttribute('data-test-id') ||
          ''
        ).trim();
        if (tid) return 'testid:' + tid;
        const name = (el.getAttribute('name') || '').trim();
        if (name) return 'name:' + name;
        return null;
      };
      const out = [];
      let nth = -1;
      for (const sel of document.querySelectorAll('select')) {
        nth++;
        if (!visible(sel)) continue;
        const opts = Array.from(sel.options || []).filter((o) => !o.disabled);
        if (opts.length < 3) continue;
        const key = keyOf(sel);
        // Structural selector for replay/exercise: stable key, else document-order
        // index among <select>s (never the visible text), matching the runner's
        // selector grammar.
        const ssel = key ? 'key:' + key : 'tag:select#' + nth;
        out.push({
          ssel,
          orig: sel.value,
          opts: opts.map((o) => ({
            value: o.value,
            label: norm(o.label || o.textContent) || o.value,
          })),
        });
      }
      return out;
    })
    .catch(() => []);
  // One choice group per select. opts carry the option `value` + `label`; `sel`
  // is the option's addressable identity (selectSelector=optionValue) so a
  // recorded clip / dedup key is stable and locale-invariant.
  return raw.map((s) => ({
    role: 'select',
    selectSel: s.ssel,
    orig: s.orig,
    opts: s.opts.map((o) => ({ sel: s.ssel + '=' + o.value, value: o.value, label: o.label })),
  }));
}

// Set a native <select>'s value by structural selector (key:<...> or
// tag:select#<idx>) and dispatch input+change so frameworks bound to it react.
// Returns true when the select was found and set. Non-destructive aside from the
// value change (restored by exerciseChoiceGroup after the pass).
async function setSelectValue(page, selectSel, value) {
  return await page
    .evaluate(
      ({ selectSel, value }) => {
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : String(v).replace(/["\\]/g, '\\$&');
        let el = null;
        if (selectSel.startsWith('key:')) {
          const body = selectSel.slice(4);
          const ci = body.indexOf(':');
          const kind = ci >= 0 ? body.slice(0, ci) : '';
          const val = ci >= 0 ? body.slice(ci + 1) : body;
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
          } else if (kind === 'id') el = document.getElementById(val);
          else if (kind === 'name')
            el = document.querySelector('select[name="' + cssEscape(val) + '"]');
        } else if (selectSel.startsWith('tag:select#')) {
          const idx = parseInt(selectSel.slice('tag:select#'.length), 10);
          const all = document.querySelectorAll('select');
          el = idx >= 0 && idx < all.length ? all[idx] : null;
        }
        if (!el || el.tagName.toLowerCase() !== 'select') return false;
        el.scrollIntoView({ block: 'center', inline: 'center' });
        el.value = value;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return true;
      },
      { selectSel, value },
    )
    .catch(() => false);
}

// Capture a GLOBAL-layout fingerprint: page horizontal overflow + the positions
// of PERSISTENT (fixed/sticky) chrome anchors. The point is to measure a choice's
// effect that BREAKS the shared page geometry, not a choice that legitimately
// swaps content of a different height. A content-switching picker (a category /
// preview tab set) makes each choice grow the page to a DIFFERENT height, which
// pushes flow content (an h1/h2/footer below the fold) by different amounts --
// that is EXPECTED, not a bug, and was the choice-anomaly false positive. So the
// anchors are restricted to FIXED/STICKY chrome (a header/nav bar that must not
// move regardless of which choice is active); ordinary flow content is not an
// anchor. The horizontal-overflow term stays: a choice that shoves the page into
// horizontal overflow (a real layout break) is still caught (and is what the
// unit-test fixture's "Broken" option trips).
async function measureGlobalLayout(page) {
  return await page
    .evaluate(() => {
      // Measure from a FIXED scroll (top): the choice exercise scrolls each option
      // into view, and on a lazy-loading page different scroll depths load different
      // amounts of content, drifting far-down anchors by thousands of px between
      // options (a progressive-load artifact, not a reflow). Pinning scroll to 0
      // gives every option the same lazy-load state; only the above-the-fold hero
      // (where a taller pane's shift actually shows) is anchored below. Force an
      // INSTANT jump: many sites set CSS `scroll-behavior:smooth`, under which a
      // plain scrollTo animates and the rects below are read MID-SCROLL, which
      // shifts the "above-fold" set and injects huge phantom deltas.
      try {
        window.scrollTo({ top: 0, left: 0, behavior: 'instant' });
      } catch (_) {
        window.scrollTo(0, 0);
      }
      document.documentElement.scrollTop = 0;
      const de = document.documentElement;
      const anchors = [];
      // PINNED chrome only (fixed, or sticky while actually stuck), in VIEWPORT
      // coords: pinned chrome must not move in the viewport regardless of the
      // active choice, and viewport position is scroll-invariant. Page-absolute
      // coords (the previous fingerprint) are scroll-DEPENDENT for fixed chrome
      // (`rect.top + scrollY`), so a synced code-language picker that changed
      // content height above the component moved scrollY and read as "the header
      // moved 33px", blaming an innocent option (measured on a docs quickstart
      // page). Unpinned sticky is ordinary flow content: skipped. Anchors are
      // keyed by tag + query index so the stuck-state filter cannot misalign the
      // key-matched delta.
      const els = document.querySelectorAll('header, nav, [role=banner], [role=navigation]');
      for (let i = 0; i < els.length; i++) {
        const el = els[i];
        const cs = getComputedStyle(el);
        if (cs.position !== 'fixed' && cs.position !== 'sticky') continue;
        const r = el.getBoundingClientRect();
        if (r.width <= 0) continue;
        if (cs.position === 'sticky') {
          const topPx = parseFloat(cs.top);
          if (!Number.isFinite(topPx) || Math.abs(r.top - topPx) > 1) continue;
        }
        anchors.push([el.tagName.toLowerCase() + ':' + i, Math.round(r.top), Math.round(r.left)]);
      }
      // FLOW-content landmarks in DOCUMENT-absolute coords (scroll-invariant). THE
      // signal document.scrollHeight misses: when one option's pane is taller (the
      // code-language case -- Go's sample ~60px taller than siblings), the total
      // scrollHeight may barely grow (trailing whitespace / a height-coupled hero
      // row absorbs it) yet every heading BELOW the picker visibly shifts down.
      // Keyed by tag + clipped text (stable across a language switch: CODE text
      // changes, headings do not), so the by-key delta compares the same element.
      // Summed displacement over many shifted headings makes the outlier tower over
      // its ~0-shift siblings. Bounded for determinism; pinned chrome is measured
      // above in VIEWPORT coords so a scroll change injects no phantom delta. Kept
      // byte-identical to choice-oracle.mjs measureGlobalLayoutInPage.
      const seen = {};
      const vh = window.innerHeight || 800;
      const marks = document.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]');
      for (let i = 0; i < marks.length && anchors.length < 40; i++) {
        const el = marks[i];
        const cs = getComputedStyle(el);
        if (cs.position === 'fixed' || cs.position === 'sticky') continue;
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) continue;
        // Above-the-fold only (scroll is pinned to 0): a taller pane pushes these
        // hero headings down; below-fold headings are lazy/accumulating, so excluded.
        if (r.top < 0 || r.top > vh) continue;
        const txt = (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
        if (!txt) continue;
        const key = 'f:' + el.tagName.toLowerCase() + ':' + txt;
        if (seen[key]) continue;
        seen[key] = 1;
        anchors.push([key, Math.round(r.top), Math.round(r.left)]);
      }
      return {
        hOverflow: Math.max(0, de.scrollWidth - window.innerWidth),
        scrollH: de.scrollHeight,
        anchors,
      };
    })
    .catch(() => null);
}

// layoutDelta (global-layout move between two fingerprints) and medianOf are
// imported from ./choice-oracle.mjs so the web reference and the electron/tauri
// ports difference identically.

// Exhaustively select each option of a choice group, measure its effect on the
// global layout, and emit at most one EXPLORE:CHOICEBUG for the outlier (a choice
// whose effect is >= CHOICE_OUTLIER_RATIO x the median of its siblings AND at
// least CHOICE_MIN_MAGNITUDE px). Needs >= 3 options so >= 2 siblings define the
// norm. The caller re-observes afterward (the last option is left selected).
// Select one option by its accessible label (scroll into view + click), robust
// to below-fold pickers and to the positional selectors going stale as the
// picker re-renders between choices. Returns true if an element was clicked.
// Click a choice option by its ACCESSIBLE LABEL, scrolling it into view first
// (below-fold pickers must be exercised). Used as the fallback when the precise
// selector click can't resolve/reach the option (tap's reachability gate rejects
// an off-screen control before it is scrolled in).
async function clickOptionByLabel(page, role, label) {
  if (!label) return false;
  const point = await page
    .evaluate(
      ({ label }) => {
        const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
        for (const el of document.querySelectorAll(
          'button, [role=button], [role=tab], [role=radio]',
        )) {
          const ll = el.getAttribute('aria-labelledby');
          let name = norm(el.getAttribute('aria-label'));
          if (!name && ll) {
            const ref = document.getElementById(ll.split(/\s+/)[0]);
            if (ref) name = norm(ref.textContent);
          }
          if (!name) name = norm(el.textContent);
          if (name === label) {
            el.scrollIntoView({ block: 'center', inline: 'center' });
            const rect = el.getBoundingClientRect();
            return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
          }
        }
        return null;
      },
      { label },
    )
    .catch(() => null);
  if (!point) return false;
  return page.mouse
    .click(point.x, point.y, { delay: 10 })
    .then(() => true)
    .catch(() => false);
}

// Measure the global layout AFTER IT SETTLES: sample until two consecutive
// fingerprints match (or the cap hits). A choice whose layout effect lands
// asynchronously (lazy-loaded content, fonts, a CSS transition) settles PAST any
// fixed wait; with the old fixed 600ms wait the late shift landed in the NEXT
// option's measurement window and the oracle blamed the wrong sibling (measured:
// a docs code-language picker whose real offender was the option BEFORE the one
// reported). Sampling to stability pins each option's effect to the option that
// caused it, at the same 600ms cost in the common already-stable case.
async function measureSettledLayout(page) {
  await page.waitForTimeout(300);
  let prev = await measureGlobalLayout(page);
  for (let waited = 300; waited < 2400; waited += 300) {
    await page.waitForTimeout(300);
    const cur = await measureGlobalLayout(page);
    if (cur && prev && layoutDelta(prev, cur) === 0) return cur;
    prev = cur;
  }
  return prev;
}

// Select one option of a choice group. A native <select> (FEATURE 1) is driven
// by setting its .value + dispatching change/input (no element to click); every
// other group kind clicks the option element. Prefer the EXACT option by its
// structural selector (so two groups sharing an option label don't
// cross-exercise each other's components), but fall back to the label click when
// the precise selector can't be reached -- tap()'s reachability gate rejects a
// below-fold picker before clickOptionByLabel scrolls it into view.
async function pickChoiceOption(page, group, opt) {
  return group.role === 'select'
    ? await setSelectValue(page, group.selectSel, opt.value)
    : (await tap(page, opt.sel)) || (await clickOptionByLabel(page, group.role, opt.label));
}

async function exerciseChoiceGroup(page, group, fromSig, keepBox = false) {
  // FIRST PASS: select each option in turn and capture its SETTLED ABSOLUTE
  // layout fingerprint (sampled to stability). Absolute per-option states, not
  // deltas: a late-settling shift lands inside its own option's settled
  // fingerprint, and no baseline choice can hide or misattribute anything.
  const results = [];
  for (const opt of group.opts) {
    const ok = await pickChoiceOption(page, group, opt);
    results.push({ opt, fp: ok ? await measureSettledLayout(page) : null });
  }
  const valid = results.filter((r) => r.fp);
  if (valid.length < 3) {
    if (group.role === 'select' && group.selectSel) {
      await setSelectValue(page, group.selectSel, group.orig);
    }
    return false; // need >= 2 siblings to call one an outlier
  }
  // NORM: the MEDOID fingerprint (the option whose layout is most like the
  // others) is the group's typical page geometry; each option's magnitude is
  // its distance from that norm. The pack of ordinary options defines the
  // median deviation, so a picker whose panes all differ by comparable amounts
  // stays quiet, while a genuine odd-one-out (one language whose selection
  // reflows the whole page while its siblings sit within px of each other)
  // towers over the median and fires.
  let medoid = valid[0];
  let bestSum = Infinity;
  for (const r of valid) {
    let s = 0;
    for (const o of valid) if (o !== r) s += layoutDelta(r.fp, o.fp);
    if (s < bestSum) {
      bestSum = s;
      medoid = r;
    }
  }
  for (const r of valid) r.mag = layoutDelta(medoid.fp, r.fp);
  const siblingMedFor = (cand) => medianOf(valid.filter((o) => o !== cand).map((o) => o.mag));
  const candidates = valid
    .filter((r) => {
      if (r === medoid || r.mag < CHOICE_MIN_MAGNITUDE) return false;
      return r.mag >= CHOICE_OUTLIER_RATIO * Math.max(siblingMedFor(r), 1);
    })
    .sort((a, b) => b.mag - a.mag);
  // This oracle promises an odd ONE out. If several options differ from the
  // ordinary pack, the component is showing legitimately different-sized
  // content rather than one broken choice. Reporting every large pane here
  // produced false findings on documentation language/sample pickers.
  if (candidates.length !== 1) {
    if (group.role === 'select' && group.selectSel) {
      await setSelectValue(page, group.selectSel, group.orig);
    }
    return false;
  }
  // CAUSAL CONFIRMATION: the first pass attributes; only a controlled A/B
  // re-toggle PROVES ownership. Park the group on the
  // medoid (the typical layout), settle, then select the candidate, settle --
  // the candidate owns a bug only if the deviation FOLLOWS it in this isolated
  // pair. This is what stops a slow async shift from convicting
  // an innocent neighbor, and it doubles as the reproducibility check the
  // recorded clip relies on.
  const confirmed = [];
  for (const cand of candidates) {
    if (!(await pickChoiceOption(page, group, medoid.opt))) continue;
    const a = await measureSettledLayout(page);
    if (!(await pickChoiceOption(page, group, cand.opt))) continue;
    const b = await measureSettledLayout(page);
    const mag = a && b ? layoutDelta(a, b) : null;
    const med = siblingMedFor(cand);
    if (
      mag !== null &&
      mag >= CHOICE_MIN_MAGNITUDE &&
      mag >= CHOICE_OUTLIER_RATIO * Math.max(med, 1)
    ) {
      confirmed.push({ opt: cand.opt, mag, med });
    }
  }
  confirmed.sort((a, b) => b.mag - a.mag);
  const max = confirmed[0] || null;
  // FEATURE 1 restore: a native <select> is left on the last exercised option
  // above, so put it back to its original value (non-destructive, like the rest
  // of the oracle). ARIA/button groups are left selected by design (the caller
  // re-observes the resulting state); a hidden form value is not a navigable
  // state, so it is restored instead.
  if (group.role === 'select' && group.selectSel) {
    await setSelectValue(page, group.selectSel, group.orig);
  }
  const isOutlier = !!max;
  if (isOutlier) {
    for (const c of confirmed) {
      log(
        'EXPLORE:CHOICEBUG ' +
          JSON.stringify({
            from: fromSig,
            role: group.role,
            outlier: c.opt.label || c.opt.sel,
            sel: c.opt.sel,
            magnitude: Math.round(c.mag),
            siblingMedian: Math.round(c.med),
          }),
      );
    }
    // Recorded fuzz walk (`fuzz --record`): re-select the outlier and box it so
    // the clip shows WHICH choice shifts the page - the differential finding made
    // visible. Unlike the other oracles this fires during the fuzz walk (the
    // exercise is fuzz-only), so it draws here, holds, then cleans up so the rest
    // of the walk is untouched. Reuses the trigger path of drawFindingBoxes (the
    // boxed outlier, plus any overflow the shift causes).
    if (VIDEO_DIR) {
      // A native <select> outlier: re-set the select to the outlier value and tag
      // the SELECT element so the box lands on the picker that shifted the page.
      let tapped = false;
      if (group.role === 'select' && group.selectSel) {
        await setSelectValue(page, group.selectSel, max.opt.value);
        tapped = await page
          .evaluate(
            ({ selectSel }) => {
              const cssEscape = (v) =>
                window.CSS && CSS.escape ? CSS.escape(v) : String(v).replace(/["\\]/g, '\\$&');
              let el = null;
              if (selectSel.startsWith('key:')) {
                const body = selectSel.slice(4);
                const ci = body.indexOf(':');
                const kind = ci >= 0 ? body.slice(0, ci) : '';
                const val = ci >= 0 ? body.slice(ci + 1) : body;
                if (kind === 'testid')
                  el =
                    document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
                    document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
                else if (kind === 'id') el = document.getElementById(val);
                else if (kind === 'name')
                  el = document.querySelector('select[name="' + cssEscape(val) + '"]');
              } else if (selectSel.startsWith('tag:select#')) {
                const idx = parseInt(selectSel.slice('tag:select#'.length), 10);
                const all = document.querySelectorAll('select');
                el = idx >= 0 && idx < all.length ? all[idx] : null;
              }
              if (!el) return false;
              for (const e of document.querySelectorAll('[data-reproit-trigger]'))
                e.removeAttribute('data-reproit-trigger');
              el.setAttribute('data-reproit-trigger', '1');
              return true;
            },
            { selectSel: group.selectSel },
          )
          .catch(() => false);
      } else {
        // Re-select the EXACT outlier by selector and tag it (mark) so the box lands
        // on the choice that shifted the page, not a same-label sibling. Fall back
        // to the label click + a manual trigger tag when the selector can't be
        // reached (below-fold), so the clip still boxes the right control.
        tapped = await tap(page, max.opt.sel, { mark: true });
      }
      if (!tapped) {
        const label = max.opt.label || max.opt.sel;
        await clickOptionByLabel(page, group.role, label);
        await page
          .evaluate(
            ({ label }) => {
              const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
              for (const e of document.querySelectorAll('[data-reproit-trigger]'))
                e.removeAttribute('data-reproit-trigger');
              for (const el of document.querySelectorAll(
                'button, [role=button], [role=tab], [role=radio]',
              )) {
                const ll = el.getAttribute('aria-labelledby');
                let name = norm(el.getAttribute('aria-label'));
                if (!name && ll) {
                  const ref = document.getElementById(ll.split(/\s+/)[0]);
                  if (ref) name = norm(ref.textContent);
                }
                if (!name) name = norm(el.textContent);
                if (name === label) {
                  el.setAttribute('data-reproit-trigger', '1');
                  break;
                }
              }
            },
            { label },
          )
          .catch(() => {});
      }
      await page.waitForTimeout(500);
      await drawFindingBoxes(page, {
        triggerLabel: 'layout shift +' + Math.round(max.mag) + 'px',
        oracle: 'no-choice-anomaly',
      }).catch(() => {});
      await page.waitForTimeout(2200);
      // A scan clip (`keepBox`) ends on the boxed outlier, so the cleanup that a
      // mid-walk exercise does is skipped; the caller holds + finishes the clip.
      if (!keepBox) {
        await page
          .evaluate(() => {
            try {
              clearInterval(window.__reproitBoxHeal);
            } catch (_) {}
            const b = document.getElementById('__reproit_boxes');
            if (b) b.remove();
            for (const e of document.querySelectorAll('[data-reproit-trigger]'))
              e.removeAttribute('data-reproit-trigger');
          })
          .catch(() => {});
      }
      return true;
    }
    return false;
  }
}

async function main() {
