// The single authored copy of the in-page DOM predicates every DOM runner
// (web, Electron, Tauri) uses to name and to find a control.
//
// WHY THIS FILE EXISTS. `role:<role>#<idx>` is the product's element identity:
// a snapshot ASSIGNS the index and a later tap/type/clip RESOLVES it. Those two
// halves must count the same set, or a selector denotes one element when it is
// written and a different element when it is read. They did not: twelve copies
// of `interactive()` carried two incompatible rules, and Electron and Tauri each
// shipped both at once, so `role:textfield#N` meant different elements inside a
// single run. One copy lives here now.
//
// HOW IT IS SHARED. The predicates run INSIDE the page, so they cannot be
// imported there. They are composed host-side with `new Function`, which gives
// one object that both transports accept:
//   - Playwright (web, Electron) serializes a function for page.evaluate, and a
//     `new Function` value stringifies to valid source, so it round-trips.
//   - WebDriver (Tauri) takes a body STRING, so callers interpolate `.toString()`.
// The `new Function` call is HOST-side codegen only; nothing is eval'd in the
// page, so an app's CSP is untouched. This is the same idiom the choice-anomaly
// and scroll-round-trip oracles already use (CHOICE_ANOMALY_IN_PAGE_SRC).
//
// Every function here must be SELF-CONTAINED: no module-scope references, since
// only its own source crosses into the page.

// Style visibility: laid out with a non-empty box and not hidden by computed
// style. Deliberately NOT reachability (hit-testable, on-screen) -- reachability
// is viewport-dependent, and the structural index must not change when a window
// is resized. Callers that need a user-reachable element gate on top of this.
function visible(el) {
  const r = el.getBoundingClientRect();
  if (r.width === 0 || r.height === 0) return false;
  const st = getComputedStyle(el);
  return st.visibility !== 'hidden' && st.display !== 'none';
}

// The canonical role vocabulary. An explicit ARIA role wins over the tag, so an
// authored `role=` is honoured; otherwise the tag's native semantics decide.
function roleOf(el) {
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
}

// THE TAPPABLE GRAMMAR: which elements occupy a `role:<role>#<idx>` slot.
//
// Text fields ARE in it. The explorer drives them with a "type" action, so an
// app whose only control is a field (login, search, TodoMVC's new-todo) has a
// drivable graph rather than one dead state. The rejected alternative excluded
// `<input>` of type text/password/email/number/search: that rule cannot be the
// canonical one, because (a) it makes those fields unaddressable, so a finding
// on one is unreportable, and (b) it is not even the rule it claims to be --
// `<textarea>` and `<select>` still entered through the tabIndex fallback
// below, so it excluded five input types rather than "text fields".
//
// Keeping both was also rejected. A capability difference must be FORCED by the
// platform; the Electron renderer is Chromium and the Tauri webview is a full
// DOM, so both rules run identically on all three. Two rules here were a
// divergence, not a capability.
function interactive(el, role) {
  const tag = el.tagName.toLowerCase();
  if (['a', 'button', 'select'].includes(tag)) return true;
  if (tag === 'input' || tag === 'textarea') return true;
  if (role === 'textfield') return true;
  if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
    return true;
  if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
  return false;
}

// The three predicates as in-page `const` declarations. Bound to fixed names so
// the composed bodies below can call them by name after the bundler has renamed
// the declarations above.
export const DOM_PREDICATES_SRC =
  'const visible = ' +
  visible.toString() +
  ';\nconst roleOf = ' +
  roleOf.toString() +
  ';\nconst interactive = ' +
  interactive.toString() +
  ';\n';

// Resolve one structural selector against the live tree. The grammar is the
// runner's whole element-addressing surface, and no visible text is ever used:
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<r>#<i>   -> the i-th style-visible tappable of role r, document order
// Returns the Element or null. Reachability, scrolling and the click itself are
// the CALLER's business: this decides identity only, so the index a snapshot
// assigned cannot shift with the viewport a replay happens to use.
const RESOLVE_TARGET_BODY = `
  const s = String(sel == null ? '' : sel);
  const cssEscape = (v) => (
    window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&')
  );
  if (s.startsWith('key:')) {
    const body = s.slice(4);
    const ci = body.indexOf(':');
    if (ci < 0) return null;
    const kind = body.slice(0, ci);
    const val = body.slice(ci + 1);
    if (kind === 'testid') {
      return document.querySelector('[data-testid="' + cssEscape(val) + '"]')
        || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
    }
    if (kind === 'id') return document.getElementById(val);
    if (kind === 'name') return document.querySelector('[name="' + cssEscape(val) + '"]');
    return null;
  }
  if (!s.startsWith('role:')) return null;
  const hash = s.indexOf('#');
  if (hash < 0) return null;
  const role = s.slice('role:'.length, hash);
  const idx = parseInt(s.slice(hash + 1), 10);
  if (!(idx >= 0)) return null;
  let seen = -1;
  let target = null;
  const walk = (el) => {
    if (target) return;
    if (!visible(el)) { for (const c of el.children) walk(c); return; }
    const r = roleOf(el);
    if (interactive(el, r) && r === role) {
      seen++;
      if (seen === idx) { target = el; return; }
    }
    for (const c of el.children) walk(c);
  };
  const root = document.body || document.documentElement;
  if (root) walk(root);
  return target;
`;

// A function object, so Playwright can serialize it, and its source, so
// WebDriver can interpolate it. One authored definition, two transports.
export const resolveStructuralTarget = new Function(
  'sel',
  DOM_PREDICATES_SRC + RESOLVE_TARGET_BODY,
);
export const RESOLVE_STRUCTURAL_TARGET_SRC = resolveStructuralTarget.toString();

// CONTENT-BUG oracle (deterministic, DOM/label-based). The literal artifacts a
// stringify/template bug leaks to the screen: `[object Object]` and an
// unrendered `{{...}}` / `${...}` placeholder. Scans only the OWN text of
// visible elements, so a container's text is never attributed to it via its
// children, and addresses each finding by a stable, locale-invariant key, never
// by the text. Pure substring/structure test, no pixel or timing read, so the
// same DOM yields the same finding on every run and replay.
export function detectContentBugs(injectedValues) {
  // Fuzzer provenance: a value reproit's own fuzzer TYPED into the app this run,
  // reflected back into a label, is not the app's broken content -- it is our probe
  // echoed (the XSS/template-injection probe `"><img src=x onerror=alert(1)>{{7*7}}`
  // reflected into a <strong> was a false positive). Mirror brokenAssetScan: skip a
  // label whose text contains, or is contained by, a non-trivial injected value.
  const injected = (Array.isArray(injectedValues) ? injectedValues : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    // Direct: the whole label is fuzzer-provenanced (either containment direction).
    if (injected.some((v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1)))
      return true;
    // Fragmented: when the browser PARSES a reflected probe (e.g.
    // `"><img src=x onerror=alert(1)>{{7*7}}`), the `<img>` markup is stripped from
    // the visible text, leaving a fragment that is not a contiguous substring of the
    // raw injected value. So also check the specific ARTIFACT tokens that trigger a
    // finding -- a `{{...}}`/`${...}` binding, or the object-coercion literal -- for
    // fuzzer provenance (the probe that produced them was typed by us).
    const arts = [];
    const tm = n.match(/\{\{[^}]*\}\}/g);
    if (tm) arts.push(...tm);
    const dm = n.match(/\$\{[^}]*\}/g);
    if (dm) arts.push(...dm);
    if (n.indexOf('[object object]') !== -1) arts.push('[object object]');
    return arts.some((a) => injected.some((v) => v.indexOf(a) !== -1));
  };
  const isVisible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  // A CODE context legitimately shows template/markup syntax as literal text
  // (documentation, a code sample, an editable field), so its text is never a
  // leaked binding. True if the element or any ancestor is a code container or is
  // contenteditable.
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
  // The OWN (non-descendant) trimmed text of an element: only text directly under
  // it, so a container's text isn't attributed to it via its children.
  const ownText = (el) => {
    let t = '';
    for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
    return t.replace(/\s+/g, ' ').trim();
  };
  // The artifact classifiers. Each returns a stable reason tag or null. Order is
  // fixed and the first match wins, so a label can only carry one reason.
  // Shared PROSE GUARD: a real leaked artifact IS the label (a bare token, or a
  // short field-name prefix like "Price: X"). Documentation PROSE that merely
  // MENTIONS the token -- "The rendered result will be [object Object] because...",
  // "As with transitions... the double {{ }} syntax" -- has natural-language words
  // around it. Fire only when, with the artifact(s) removed, the remainder is a
  // SHORT label with no sentence structure. This kills the docs-site FP for BOTH
  // the object-coercion literal AND the template-brace token (every templating
  // framework's docs shows `{{ }}` in prose).
  const dominates = (stripped) => stripped.length <= 24 && !/[.!?]/.test(stripped);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text
        .replace(/\[object Object\]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'object-object';
      // else prose mention -- fall through to the template check below.
    }
    // An unrendered template placeholder: a `{{ expr }}` or `${ expr }` survived
    // into the DOM (the binding engine never evaluated it), gated by the prose guard.
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
  // Document-order index per tag, so an UNKEYED element still gets a stable,
  // distinct positional key (`tag:<tag>#<idx>`) -- a plain `<span>[object
  // Object]</span>` with no id/testid was silently skipped on the Electron and
  // Tauri runners, which dropped a whole common class of broken-render artifacts
  // while still declaring content-bug supported. Same grammar as the overflow
  // oracle's tag fallback; the index keeps two unkeyed artifacts from colliding.
  const tagIdx = {};
  for (const el of all) {
    if (!isVisible(el)) continue;
    if (inCodeContext(el)) continue;
    const tag = el.tagName.toLowerCase();
    const n = tagIdx[tag] || 0;
    tagIdx[tag] = n + 1;
    const key = keyOf(el) || 'tag:' + tag + '#' + n;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    // Reflected fuzzer probe, not the app's own content -> not a bug.
    if (fromFuzzInjection(text)) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    // Clip the offending text so the marker stays bounded; the reason+key are the
    // stable identity, the text is human detail.
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  // Stable order: by key then reason, so the marker is byte-identical run to run.
  out.sort((a, b) =>
    a.key < b.key ? -1 : a.key > b.key ? 1 : a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0,
  );
  return out;
}

// The same oracle as a WebDriver execute() body: `arguments[0]` is the injected
// values array. Built from the function above so there is no second copy.
export const DETECT_CONTENT_BUGS_SRC =
  'return (' + detectContentBugs.toString() + ')(arguments[0]);';
