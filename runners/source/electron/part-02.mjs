  const snap = await page.evaluate(
    ({ maxLen, valueNodeSelectors }) => {
      const labels = []; // DISPLAY-ONLY visible text
      const rawTaps = []; // tappable nodes in document order
      const textNodes = []; // (stable-key, trimmed text) for the Layer-1 fingerprint

      // Fixed canonical role vocabulary (docs/signature.md "Roles").
      const ROLES = {
        screen: 1,
        header: 1,
        text: 1,
        button: 1,
        link: 1,
        textfield: 1,
        image: 1,
        icon: 1,
        list: 1,
        listitem: 1,
        tab: 1,
        switch: 1,
        checkbox: 1,
        radio: 1,
        slider: 1,
        menu: 1,
        menuitem: 1,
        dialog: 1,
        group: 1,
        node: 1,
      };
      const TRANSIENT_ROLES = {
        toast: 1,
        snackbar: 1,
        spinner: 1,
        progress: 1,
        tooltip: 1,
        badge: 1,
      };

      // DOM -> canonical role, from tag + aria role + input type, NEVER text.
      const roleOf = (el) => {
        const tag = el.tagName.toLowerCase();
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (ariaRole) {
          if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
            return 'textfield';
          if (ariaRole === 'heading') return 'header';
          if (ariaRole === 'img') return 'image';
          if (ariaRole === 'switch') return 'switch';
          if (ariaRole === 'link') return 'link';
          if (ariaRole === 'button') return 'button';
          if (ROLES[ariaRole]) return ariaRole;
        }
        if (tag === 'input') {
          const t = (el.getAttribute('type') || 'text').toLowerCase();
          if (t === 'checkbox') return 'checkbox';
          if (t === 'radio') return 'radio';
          if (t === 'range') return 'slider';
          if (['button', 'submit', 'reset', 'image'].includes(t)) return 'button';
          return 'textfield';
        }
        if (tag === 'textarea' || tag === 'select') return 'textfield';
        if (tag === 'a') return 'link';
        if (tag === 'button') return 'button';
        if (tag === 'img' || tag === 'svg') return 'image';
        if (/^h[1-6]$/.test(tag) || tag === 'header') return 'header';
        if (tag === 'ul' || tag === 'ol') return 'list';
        if (tag === 'li') return 'listitem';
        if (tag === 'dialog') return 'dialog';
        if (tag === 'nav' || tag === 'menu') return 'menu';
        return 'node';
      };

      // Optional input type refinement (textfield only).
      const typeOf = (el, role) => {
        if (role !== 'textfield') return null;
        if (el.tagName.toLowerCase() !== 'input') return null;
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        const allowed = { text: 1, password: 1, email: 1, number: 1, search: 1 };
        return allowed[t] ? t : 'text';
      };

      // Language-independent icon identity: svg <use> href / data-icon. No text.
      const iconOf = (el) => {
        const di = el.getAttribute('data-icon') || el.getAttribute('data-icon-name');
        if (di && di.trim()) return di.trim();
        const use = el.querySelector ? el.querySelector('use[href], use[xlink\\:href]') : null;
        if (use) {
          const href = use.getAttribute('href') || use.getAttribute('xlink:href');
          if (href && href.trim()) return href.trim().replace(/^#/, '');
        }
        return null;
      };

      // Stable developer id: data-testid > id > name (for the descriptor token).
      const idOf = (el) => {
        const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
        if (testid && testid.trim()) return testid.trim();
        const id = el.getAttribute('id');
        if (id && id.trim()) return id.trim();
        const name = el.getAttribute('name');
        if (name && name.trim()) return name.trim();
        return null;
      };

      // Selector KEY (for replay): kind-tagged so tap() can resolve it.
      const keyOf = (el) => {
        const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
        if (testid && testid.trim()) return 'testid:' + testid.trim();
        const id = el.getAttribute('id');
        if (id && id.trim()) return 'id:' + id.trim();
        const name = el.getAttribute('name');
        if (name && name.trim()) return 'name:' + name.trim();
        return null;
      };

      // Elements running an INFINITE animation, computed ONCE per snapshot from a
      // single document.getAnimations() call (a per-node call is O(nodes) on a large
      // DOM and dominates the crawl; mirrors the web runner).
      const infiniteAnimEls = new Set();
      try {
        const all = document.getAnimations ? document.getAnimations() : [];
        for (const a of all) {
          if (a.playState !== 'running') continue;
          const t = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
          if (t && t.iterations === Infinity && a.effect && a.effect.target)
            infiniteAnimEls.add(a.effect.target);
        }
      } catch (_) {}

      // Transient heuristic: role / aria-live / class flag a flickering node.
      const isTransientEl = (el) => {
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (TRANSIENT_ROLES[ariaRole]) return true;
        if (ariaRole === 'alert' || ariaRole === 'status') return true;
        const live = (el.getAttribute('aria-live') || '').toLowerCase();
        if (live === 'assertive' || live === 'polite') return true;
        const cls = (el.getAttribute('class') || '').toLowerCase();
        if (/\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test(cls))
          return true;
        if (el.hasAttribute('data-transient')) return true;
        // A node mid-INFINITE-animation samples a different frame every capture, so
        // exclude it. Membership in a per-snapshot precomputed Set (one
        // document.getAnimations() call) instead of a per-node call (mirrors web).
        if (infiniteAnimEls.has(el)) return true;
        return false;
      };

      // RAW value-role (docs/signature.md "Value-state"): the value-role name for
      // a value-bearing DOM element, NEVER from text. role=status/log/progressbar/
      // meter/timer pass through; <output>/role=output -> output; an aria-live
      // region (polite/assertive) -> status (so a live counter is value-bearing
      // WITHOUT opt-in); text form fields -> textfield. null for chrome / non-text
      // inputs (password is never read). Identical to the web runner.
      const valueRoleOf = (el) => {
        const tag = el.tagName.toLowerCase();
        const ar = (el.getAttribute('role') || '').toLowerCase();
        if (
          ar === 'status' ||
          ar === 'log' ||
          ar === 'progressbar' ||
          ar === 'meter' ||
          ar === 'timer'
        )
          return ar;
        if (tag === 'output' || ar === 'output') return 'output';
        const live = (el.getAttribute('aria-live') || '').toLowerCase();
        if (live === 'polite' || live === 'assertive') return 'status';
        if (tag === 'input') {
          const t = (el.getAttribute('type') || 'text').toLowerCase();
          if (
            [
              'checkbox',
              'radio',
              'range',
              'button',
              'submit',
              'reset',
              'image',
              'hidden',
              'file',
              'password',
            ].includes(t)
          )
            return null;
          return 'textfield';
        }
        if (tag === 'textarea' || tag === 'select') return 'textfield';
        if (ar === 'textbox' || ar === 'searchbox' || ar === 'combobox') return 'textfield';
        return null;
      };
      // The displayed value: the field .value for form controls, else trimmed
      // textContent for output/status/live nodes.
      const valueOf = (el) => {
        const tag = el.tagName.toLowerCase();
        if (tag === 'input' || tag === 'textarea' || tag === 'select')
          return el.value != null ? String(el.value) : '';
        return (el.textContent != null ? el.textContent : '').trim();
      };
      // Layer-3 opt-in: does this element match one of the value_nodes selectors?
      // key:<id> | role:<role>#<idx> | raw CSS. Same grammar as reproit.yaml.
      const selList = valueNodeSelectors || [];
      const matchesValueNode = (el) => {
        for (const sel of selList) {
          if (!sel) continue;
          if (sel.indexOf('key:') === 0) {
            const id = sel.slice(4);
            const got = (
              el.getAttribute('data-testid') ||
              el.getAttribute('data-test-id') ||
              el.getAttribute('id') ||
              el.getAttribute('name') ||
              ''
            ).trim();
            if (id && got === id) return true;
          } else if (sel.indexOf('role:') === 0) {
            const hash = sel.indexOf('#');
            if (hash < 0) continue;
            const role = sel.slice(5, hash);
            const idx = parseInt(sel.slice(hash + 1), 10);
            if (!(idx >= 0)) continue;
            let seen = -1,
              target = null;
            const root = document.body || document.documentElement;
            (function walk(node) {
              if (target || !node) return;
              if (roleOf(node) === role) {
                seen++;
                if (seen === idx) {
                  target = node;
                  return;
                }
              }
              for (const c of node.children) walk(c);
            })(root);
            if (target === el) return true;
          } else {
            try {
              if (el.matches && el.matches(sel)) return true;
            } catch (e) {}
          }
        }
        return false;
      };

      const interactive = (el, role) => {
        const tag = el.tagName.toLowerCase();
        if (['a', 'button', 'select'].includes(tag)) return true;
        if (tag === 'input') {
          const t = (el.getAttribute('type') || 'text').toLowerCase();
          return !['text', 'password', 'email', 'number', 'search'].includes(t);
        }
        if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
          return true;
        if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
        return false;
      };

      const nameOf = (el) => {
        const aria = el.getAttribute('aria-label');
        if (aria && aria.trim()) return aria.trim();
        const title = el.getAttribute('title');
        if (title && title.trim()) return title.trim();
        const alt = el.getAttribute('alt');
        if (alt && alt.trim()) return alt.trim();
        const text = (el.innerText || el.textContent || '').trim().split('\n')[0].trim();
        return text;
      };
      const visible = (el) => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        const st = getComputedStyle(el);
        return st.visibility !== 'hidden' && st.display !== 'none';
      };
      const fnvLbl = (name) => {
        let h = 0x811c9dc5;
        for (let i = 0; i < name.length; i++) {
          h ^= name.charCodeAt(i);
          h = Math.imul(h, 0x01000193) >>> 0;
        }
        return (h >>> 0).toString(16).padStart(8, '0');
      };
      const clipLabel = (name) => {
        if (name.length <= maxLen) return name;
        const suffix = '#' + fnvLbl(name);
        return name.slice(0, maxLen - suffix.length) + suffix;
      };

      // Build the canonical Node tree (role + id + type + icon + children). The
      // root is the screen; invisible wrappers are skipped but their visible
      // descendants are hoisted; transient subtrees carry transient:true so the
      // host-side normalizer drops them. We also collect labels + tappables for
      // the display/elements list along the way.
      const buildNode = (el, isRoot) => {
        const role = isRoot ? 'screen' : roleOf(el);
        // Value-state (Layer 2): a value-role element (by tag/aria), an aria-live
        // region, or a Layer-3 opt-in node is value-bearing. Value-bearing WINS
        // over the transient heuristic, so a role=status / aria-live counter that
        // the transient heuristic would otherwise drop is kept as a value node and
        // its keypresses produce DISTINCT value-states.
        const vrole = !isRoot ? valueRoleOf(el) : null;
        const optIn = !isRoot && matchesValueNode(el);
        const valueBearing = !isRoot && (!!vrole || optIn);
        const transient = !isRoot && !valueBearing && isTransientEl(el);
        const node = { role: role };
        const id = idOf(el);
        if (id != null) node.id = id;
        const type = typeOf(el, role);
        if (type != null) node.type = type;
        const icon = iconOf(el);
        if (icon != null) node.icon = icon;
        if (valueBearing) {
          node.value = valueOf(el);
          // The flag makes the canonical is_value_bearing accept the node even
          // when roleOf normalized its raw value-role (status/output/...) to node.
          node.value_node = true;
          // Layer-1 content fingerprint: a value node's stable key + its raw value.
          const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
          textNodes.push([fkey, node.value]);
        }
        if (transient) {
          node.transient = true;
          node.children = [];
          return node;
        }

        // Layer-1 content fingerprint over text-bearing nodes (runner-local, NOT
        // canonical): any keyed element's own (non-child) trimmed text contributes
        // (stable-key, text). This catches a display whose textContent changes
        // without any structural move (a calculator/counter), so the action is seen
        // as EFFECTIVE even when the value node itself was not detected as a
        // value-role. The raw text never enters the canonical key.
        if (!isRoot && id != null && !valueBearing) {
          let own = '';
          for (const c of el.childNodes) {
            if (c.nodeType === 3) own += c.textContent;
          }
          own = own.trim();
          if (own) textNodes.push(['text:' + id, own]);
        }

        // labels + tappables (display/elements list; never in the hash)
        if (!isRoot) {
          const name = nameOf(el);
          if (name) labels.push(clipLabel(name));
          if (interactive(el, role)) {
            rawTaps.push({
              role,
              key: keyOf(el),
              label: name ? clipLabel(name) : '',
            });
          }
        }

        node.children = [];
        collectChildren(el, node.children);
        return node;
      };
      const collectChildren = (el, out) => {
        for (const child of el.children) {
          if (!visible(child)) {
            collectChildren(child, out);
            continue;
          }
          out.push(buildNode(child, false));
        }
      };

      const root = document.body || document.documentElement;
      const tree = root ? buildNode(root, true) : { role: 'screen', children: [] };

      // Structural selectors for replay (key, else role+per-role index).
      const perRole = {};
      const tappables = rawTaps.map((tn) => {
        const idx = perRole[tn.role] || 0;
        perRole[tn.role] = idx + 1;
        const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
        return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label };
      });

      // Anchor: route/path of the current screen.
      let anchor = null;
      try {
        if (location && location.pathname) {
          let pth = location.pathname;
          // Trailing-slash route normalization: /a/ and /a are the same screen.
          if (pth.length > 1) pth = pth.replace(/\/+$/, '') || '/';
          anchor = pth;
        }
      } catch (e) {}

      // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
      // value + keyed-text nodes. Sorted here so it is order-independent.
      textNodes.sort((a, b) =>
        a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0,
      );

      return { tree, anchor, labels: [...new Set(labels)], tappables, textNodes };
    },
    { maxLen: MAX_LABEL_LEN, valueNodeSelectors: valueNodeSelectors || [] },
  );

  // Hash the canonical Node tree with the host-pure canonical signature, exactly
  // like the Rust oracle and the golden vectors. Text never contributes.
  snap.sig = signatureOf(snap.anchor, snap.tree);
  // Structural-only signature (no V: section): the per-node key the Layer-1 cap
  // tracks. Computed by hashing the descriptor with the value-class suffix
  // stripped, so it is the exact pre-value-state signature of this structure.
  const full = descriptorOf(snap.anchor, snap.tree);
  const vAt = full.indexOf('\nV:');
  snap.vsection = vAt >= 0 ? full.slice(vAt + 3) : '';
  snap.structuralSig = vAt >= 0 ? fnv1a(full.slice(0, vAt)) : snap.sig;
  // Layer-1 content fingerprint (runner-local, ephemeral): structural sig plus
  // the sorted (stable-key, trimmed raw text) list. An action is EFFECTIVE iff
  // the structural sig OR this fingerprint changed (see observe/effect checks).
  // This carries raw localized text and is NEVER folded into the canonical key.
  snap.content = snap.sig + '|' + snap.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return snap;
}

// PARITY: keep in sync with runners/web/runner.mjs (operability + flicker oracle)
//
// Tier-1 flicker oracle (persistent-anchor churn). A re-render flicker is a
// transition that tears down and rebuilds chrome that did NOT need to change:
// for a frame the header/nav/list vanish, then settle back to the same thing.
// The settled-frame visual oracle cannot see it (both endpoints are correct).
// We catch it deterministically from the DOM instead of from pixels: tag the
// persistent "anchors" before a transition, then after it settles check whether
// any anchor that is VISUALLY UNCHANGED (same key, text, box) was nonetheless
// REPLACED (its DOM node identity changed). A framework that reconciles
// (React/Vue/Svelte) preserves node identity for unchanged nodes, so it does
// not trip; only an innerHTML-wipe-and-rebuild does, which is the flicker bug.
const ANCHOR_SEL =
  'header,nav,main,footer,aside,' +
  '[role=banner],[role=navigation],[role=main],[role=contentinfo],' +
  '[role=complementary],[role=region],[role=search],[role=listbox],' +
  '[role=list],[role=tablist],[role=toolbar],[role=dialog],[id]';

function markAnchors(sel) {
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const keyOf = (el) => {
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const role = (el.getAttribute('role') || '').trim();
    return 'tag:' + el.tagName.toLowerCase() + (role ? '[' + role + ']' : '');
  };
  const anchors = [];
  for (const el of document.querySelectorAll(sel)) {
    if (!visible(el)) continue;
    const r = el.getBoundingClientRect();
    anchors.push({
      key: keyOf(el),
      node: el,
      text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x),
      y: Math.round(r.y),
      w: Math.round(r.width),
      h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
}

function churnedAnchors(sel) {
  const old = window.__reproitAnchors;
  // No mark, or the document was replaced (navigation): not a flicker candidate.
  if (!old || window.__reproitAnchorDoc !== document) {
    window.__reproitAnchors = null;
    return null;
  }
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const keyOf = (el) => {
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const role = (el.getAttribute('role') || '').trim();
    return 'tag:' + el.tagName.toLowerCase() + (role ? '[' + role + ']' : '');
  };
  const cur = new Map();
  const dup = new Set();
  for (const el of document.querySelectorAll(sel)) {
    if (!visible(el)) continue;
    const k = keyOf(el);
    if (cur.has(k)) {
      dup.add(k);
      continue;
    }
    cur.set(k, el);
  }
  const churned = [];
  for (const a of old) {
    if (dup.has(a.key)) continue; // ambiguous key -> skip
    const now = cur.get(a.key);
    if (!now) continue; // gone in the new state -> a real removal, not flicker
    if (now === a.node) continue; // same node survived -> reconciled, no churn (good)
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x &&
      Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w &&
      Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key); // unchanged yet rebuilt = flicker
  }
  window.__reproitAnchors = null;
  return churned;
}

// PARITY: keep in sync with runners/web/runner.mjs (overflow oracle).
//
// CONTENT-BUG oracle (deterministic, DOM/label-based). Fires ONLY on a GROUND-
// TRUTH artifact impossible to render as legitimate copy: [object Object] (an
// object coerced to a string) or an unrendered {{...}}/${...} template placeholder.
// The bare words undefined/null/NaN are NOT matched (they occur in real copy and
// code samples -- a false positive), and text inside a CODE context (<code>/<pre>/
// <script>/<style>/<textarea>/[contenteditable]) is skipped (docs show template
// syntax legitimately). Scans only the OWN text of keyed, visible elements so the
// finding is addressed by a stable, locale-invariant key (never the text). Pure
// substring/structure test, no pixel or timing read, so the same DOM yields the
// same finding on every run/replay.
function detectContentBugs(injectedValues) {
  // Fuzzer provenance (mirrors the web tier + brokenAssetScan): a reflected fuzzer
  // probe is not the app's own broken content.
  const injected = (Array.isArray(injectedValues) ? injectedValues : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    if (injected.some((v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1)))
      return true;
    // Fragmented reflection: the browser parsed markup out of the probe, so the
    // visible text is a fragment; check the specific artifact tokens for provenance.
    const arts = [];
    const tm = n.match(/\{\{[^}]*\}\}/g);
    if (tm) arts.push(...tm);
    const dm = n.match(/\$\{[^}]*\}/g);
    if (dm) arts.push(...dm);
    if (n.indexOf('[object object]') !== -1) arts.push('[object object]');
    return arts.some((a) => injected.some((v) => v.indexOf(a) !== -1));
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const CODE_TAGS = new Set(['code', 'pre', 'script', 'style', 'textarea']);
  const inCodeContext = (el) => {
    if (el.isContentEditable) return true;
    for (let n = el; n && n !== document.body; n = n.parentElement) {
      if (CODE_TAGS.has(n.tagName.toLowerCase())) return true;
    }
    return false;
  };
  const keyOf = (el) => {
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const name = (el.getAttribute('name') || '').trim();
    if (name) return 'name:' + name;
    return null;
  };
  const ownText = (el) => {
    let t = '';
    for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
    return t.replace(/\s+/g, ' ').trim();
  };
  // Prose guard for BOTH artifact kinds: fire only when the artifact IS the label,
  // never when docs prose merely mentions "[object Object]" or the "{{ }}" syntax.
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text
        .replace(/\[object Object\]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'object-object';
    }
    if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
      const s = text
        .replace(/\{\{[^}]*\}\}/g, ' ')
        .replace(/\$\{[^}]*\}/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'unrendered-template';
    }
    return null;
  };
  const out = [];
  const seen = new Set();
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (!visible(el)) continue;
    if (inCodeContext(el)) continue;
    const key = keyOf(el);
    if (!key) continue;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    if (fromFuzzInjection(text)) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  out.sort((a, b) =>
    a.key < b.key ? -1 : a.key > b.key ? 1 : a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0,
  );
  return out;
}

// PARITY: keep in sync with runners/web/runner.mjs (jank/hang watchdog).
//
// JANK / HANG watchdog (deterministic, recorded-trace based). We key off the
// browser's own Long Tasks trace, never a wall-clock duration sample: a
// `longtask` PerformanceObserver entry is emitted for any task that blocks the
// main thread > 50ms, buffered and delivered after the blocking task finishes.
// We classify by the MAX blocked duration into coarse, well-separated floors so
// timing jitter can never flip the verdict. Electron's renderer is Chromium, so
// the Long Tasks API is present and this is verbatim with the web runner.
const JANK_FLOOR_MS = 200;
const HANG_FLOOR_MS = 2000;
async function installLongTaskObserver(page) {
