const r=(e,t=40)=>`
  const maxLen = ${t};
  const selList = ${JSON.stringify(e||[])};
  const labels = [];
  const rawTaps = [];
  const textNodes = [];

  const ROLES = {
    screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
    icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
    slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
  };
  const TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };

  const roleOf = (el) => {
    const tag = el.tagName.toLowerCase();
    const ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (ariaRole) {
      if (
        ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox'
      ) return 'textfield';
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

  const typeOf = (el, role) => {
    if (role !== 'textfield') return null;
    if (el.tagName.toLowerCase() !== 'input') return null;
    const t = (el.getAttribute('type') || 'text').toLowerCase();
    const allowed = { text: 1, password: 1, email: 1, number: 1, search: 1 };
    return allowed[t] ? t : 'text';
  };

  const iconOf = (el) => {
    const di = el.getAttribute('data-icon') || el.getAttribute('data-icon-name');
    if (di && di.trim()) return di.trim();
    const use = el.querySelector ? el.querySelector('use[href], use[xlink\\\\:href]') : null;
    if (use) {
      const href = use.getAttribute('href') || use.getAttribute('xlink:href');
      if (href && href.trim()) return href.trim().replace(/^#/, '');
    }
    return null;
  };

  const idOf = (el) => {
    const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return testid.trim();
    const id = el.getAttribute('id');
    if (id && id.trim()) return id.trim();
    const name = el.getAttribute('name');
    if (name && name.trim()) return name.trim();
    return null;
  };

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
    const allAnims = document.getAnimations ? document.getAnimations() : [];
    for (const a of allAnims) {
      if (a.playState !== 'running') continue;
      const t = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
      if (t && t.iterations === Infinity && a.effect && a.effect.target) {
        infiniteAnimEls.add(a.effect.target);
      }
    }
  } catch (_) {}

  const isTransientEl = (el) => {
    const ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (TRANSIENT_ROLES[ariaRole]) return true;
    if (ariaRole === 'alert' || ariaRole === 'status') return true;
    const live = (el.getAttribute('aria-live') || '').toLowerCase();
    if (live === 'assertive' || live === 'polite') return true;
    const cls = (el.getAttribute('class') || '').toLowerCase();
    if (
      /\\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\\b/.test(cls)
    ) return true;
    if (el.hasAttribute('data-transient')) return true;
    // A node mid-INFINITE-animation samples a different frame every capture, so
    // exclude it (finite animations are settled by settleForSignature first).
    if (infiniteAnimEls.has(el)) return true;
    return false;
  };

  // RAW value-role (docs/signature.md "Value-state"): the value-role name for a
  // value-bearing DOM element, NEVER from text. role=status/log/progressbar/
  // meter/timer pass through; <output>/role=output -> output; an aria-live
  // region (polite/assertive) -> status; text form fields -> textfield. null for
  // chrome / non-text inputs (password is never read). Identical to web runner.
  const valueRoleOf = (el) => {
    const tag = el.tagName.toLowerCase();
    const ar = (el.getAttribute('role') || '').toLowerCase();
    if (
      ar === 'status' || ar === 'log' || ar === 'progressbar' ||
      ar === 'meter' || ar === 'timer'
    ) return ar;
    if (tag === 'output' || ar === 'output') return 'output';
    const live = (el.getAttribute('aria-live') || '').toLowerCase();
    if (live === 'polite' || live === 'assertive') return 'status';
    if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      if (
        ['checkbox', 'radio', 'range', 'button', 'submit', 'reset',
          'image', 'hidden', 'file', 'password'].includes(t)
      ) return null;
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
    if (tag === 'input' || tag === 'textarea' || tag === 'select') {
      return el.value != null ? String(el.value) : '';
    }
    return (el.textContent != null ? el.textContent : '').trim();
  };
  // Layer-3 opt-in: does this element match one of the value_nodes selectors?
  // key:<id> | role:<role>#<idx> | raw CSS. Same grammar as reproit.yaml.
  const matchesValueNode = (el) => {
    for (const sel of selList) {
      if (!sel) continue;
      if (sel.indexOf('key:') === 0) {
        const id = sel.slice(4);
        const got = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') ||
          el.getAttribute('id') || el.getAttribute('name') || '').trim();
        if (id && got === id) return true;
      } else if (sel.indexOf('role:') === 0) {
        const hash = sel.indexOf('#');
        if (hash < 0) continue;
        const role = sel.slice(5, hash);
        const idx = parseInt(sel.slice(hash + 1), 10);
        if (!(idx >= 0)) continue;
        let seen = -1, target = null;
        const root = document.body || document.documentElement;
        (function walk(node) {
          if (target || !node) return;
          if (roleOf(node) === role) { seen++; if (seen === idx) { target = node; return; } }
          for (const c of node.children) walk(c);
        })(root);
        if (target === el) return true;
      } else {
        try { if (el.matches && el.matches(sel)) return true; } catch (e) {}
      }
    }
    return false;
  };

  // CANONICAL tappable grammar, byte-identical to shared/dom-walk.mjs. This walk
  // ASSIGNS Tauri's \`role:<role>#<idx>\`, so it must count exactly what the shared
  // resolver counts when TAP_JS reads that index back. It did not: this copy
  // dropped <input type=text/password/email/number/search>, so a text field was
  // unaddressable while the ground-truth walk in tauri/part-02.mjs indexed it,
  // and the two streams disagreed about which element role:textfield#N named.
  const interactive = (el, role) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select'].includes(tag)) return true;
    if (tag === 'input' || tag === 'textarea') return true;
    if (role === 'textfield') return true;
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
    return (el.innerText || el.textContent || '').trim().split('\\n')[0].trim();
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

  const buildNode = (el, isRoot) => {
    const role = isRoot ? 'screen' : roleOf(el);
    // Value-state (Layer 2): a value-role element (by tag/aria), an aria-live
    // region, or a Layer-3 opt-in node is value-bearing. Value-bearing WINS over
    // the transient heuristic, so a role=status / aria-live counter that the
    // transient heuristic would otherwise drop is kept as a value node and its
    // keypresses produce DISTINCT value-states.
    const vrole = !isRoot ? valueRoleOf(el) : null;
    const optIn = !isRoot && matchesValueNode(el);
    const valueBearing = !isRoot && (!!vrole || optIn);
    const transient = !isRoot && !valueBearing && isTransientEl(el);
    const node = { role: role };
    const id = idOf(el); if (id != null) node.id = id;
    const type = typeOf(el, role); if (type != null) node.type = type;
    const icon = iconOf(el); if (icon != null) node.icon = icon;
    if (valueBearing) {
      node.value = valueOf(el);
      // The flag makes the canonical is_value_bearing accept the node even when
      // roleOf normalized its raw value-role (status/output/...) to node.
      node.value_node = true;
      // Layer-1 content fingerprint: a value node's stable key + its raw value.
      const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
      textNodes.push([fkey, node.value]);
    }
    if (transient) { node.transient = true; node.children = []; return node; }

    // Layer-1 content fingerprint over text-bearing nodes (runner-local, NOT
    // canonical): any keyed element's own (non-child) trimmed text contributes
    // (stable-key, text). Catches a display whose textContent changes without
    // any structural move; the raw text never enters the canonical key.
    if (!isRoot && id != null && !valueBearing) {
      let own = '';
      for (const c of el.childNodes) { if (c.nodeType === 3) own += c.textContent; }
      own = own.trim();
      if (own) textNodes.push(['text:' + id, own]);
    }

    if (!isRoot) {
      const name = nameOf(el);
      if (name) labels.push(clipLabel(name));
      if (interactive(el, role)) {
        rawTaps.push({ role, key: keyOf(el), label: name ? clipLabel(name) : '' });
      }
    }

    node.children = [];
    collectChildren(el, node.children);
    return node;
  };
  const collectChildren = (el, out) => {
    for (const child of el.children) {
      if (!visible(child)) { collectChildren(child, out); continue; }
      out.push(buildNode(child, false));
    }
  };

  const root = document.body || document.documentElement;
  const tree = root ? buildNode(root, true) : { role: 'screen', children: [] };

  const perRole = {};
  const tappables = rawTaps.map((tn) => {
    const idx = perRole[tn.role] || 0;
    perRole[tn.role] = idx + 1;
    const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
    return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label };
  });

  let anchor = null;
  try {
    if (location && location.pathname) {
      let pth = location.pathname;
      // Trailing-slash route normalization: /docs/ and /docs are the same screen,
      // so a redirect that toggles the slash is not a distinct route.
      while (pth.length > 1 && pth.charCodeAt(pth.length - 1) === 47) pth = pth.slice(0, -1);
      anchor = pth;
    }
  } catch (e) {}

  // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
  // value + keyed-text nodes. Sorted here so it is order-independent.
  textNodes.sort((a, b) => (
    a[0] < b[0] ? -1 : a[0] > b[0] ? 1 :
      (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)
  ));

  return { tree, anchor, labels: [...new Set(labels)], tappables, textNodes };
`;export{r as snapshotJs};
