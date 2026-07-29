function roleFromXcui(tag) {
  switch (tag) {
    case 'XCUIElementTypeButton':
    case 'XCUIElementTypeBackButton':
    case 'XCUIElementTypeMenuButton':
    case 'XCUIElementTypeToolbarButton':
      return 'button';
    case 'XCUIElementTypeLink':
      return 'link';
    case 'XCUIElementTypeTextField':
    case 'XCUIElementTypeSecureTextField':
    case 'XCUIElementTypeSearchField':
    case 'XCUIElementTypeTextView':
      return 'textfield';
    case 'XCUIElementTypeStaticText':
      return 'text';
    case 'XCUIElementTypeImage':
      return 'image';
    case 'XCUIElementTypeSwitch':
    case 'XCUIElementTypeToggle':
      return 'switch';
    case 'XCUIElementTypeSlider':
      return 'slider';
    case 'XCUIElementTypeCheckBox':
      return 'checkbox';
    case 'XCUIElementTypeRadioButton':
      return 'radio';
    case 'XCUIElementTypeTabBar':
    case 'XCUIElementTypeSegmentedControl':
      return 'menu';
    case 'XCUIElementTypeTab':
      return 'tab';
    case 'XCUIElementTypeNavigationBar':
      return 'header';
    case 'XCUIElementTypeTable':
    case 'XCUIElementTypeCollectionView':
    case 'XCUIElementTypeScrollView':
      return 'list';
    case 'XCUIElementTypeCell':
      return 'listitem';
    case 'XCUIElementTypeMenu':
      return 'menu';
    case 'XCUIElementTypeMenuItem':
      return 'menuitem';
    case 'XCUIElementTypeAlert':
    case 'XCUIElementTypeSheet':
    case 'XCUIElementTypeDialog':
      return 'dialog';
    case 'XCUIElementTypeActivityIndicator':
    case 'XCUIElementTypeProgressIndicator':
      return 'progress';
    case 'XCUIElementTypeApplication':
    case 'XCUIElementTypeWindow':
      return 'screen';
    default:
      return null;
  }
}

// Android widget class -> canonical role. The class attribute (or the tag) holds
// the fully-qualified widget name; we match on its leaf, case-insensitively.
function roleFromAndroid(cls) {
  const c = cls.toLowerCase();
  if (c.includes('imagebutton') || c.includes('togglebutton')) return 'button';
  if (c.includes('button')) return 'button';
  if (c.includes('edittext') || c.includes('autocompletetextview') || c.includes('textinput'))
    return 'textfield';
  if (c.includes('switch')) return 'switch';
  if (c.includes('seekbar') || c.includes('slider')) return 'slider';
  if (c.includes('checkbox')) return 'checkbox';
  if (c.includes('radiobutton')) return 'radio';
  if (c.includes('progressbar')) return 'progress';
  if (c.includes('imageview') || c.includes('image')) return 'image';
  if (c.includes('tablayout')) return 'menu';
  if (c.includes('recyclerview') || c.includes('listview') || c.includes('scrollview'))
    return 'list';
  if (
    c.includes('viewgroup') ||
    c.includes('linearlayout') ||
    c.includes('framelayout') ||
    c.includes('relativelayout')
  )
    return 'group';
  if (c.includes('textview')) return 'text';
  if (c.includes('toolbar') || c.includes('actionbar')) return 'header';
  return null;
}

// ARIA-style / generic a11y trait (accessibility-role, role) -> canonical role.
function roleFromTrait(trait) {
  switch ((trait || '').toLowerCase()) {
    case 'header':
    case 'heading':
      return 'header';
    case 'button':
    case 'imagebutton':
    case 'togglebutton':
      return 'button';
    case 'link':
      return 'link';
    case 'search':
    case 'searchbox':
    case 'combobox':
    case 'textbox':
      return 'textfield';
    case 'image':
    case 'img':
      return 'image';
    case 'switch':
      return 'switch';
    case 'checkbox':
      return 'checkbox';
    case 'radio':
      return 'radio';
    case 'adjustable':
    case 'slider':
      return 'slider';
    case 'tab':
      return 'tab';
    case 'tablist':
    case 'menubar':
    case 'toolbar':
    case 'menu':
      return 'menu';
    case 'menuitem':
      return 'menuitem';
    case 'list':
      return 'list';
    case 'listitem':
    case 'cell':
      return 'listitem';
    case 'alert':
    case 'dialog':
      return 'dialog';
    case 'text':
    case 'summary':
      return 'text';
    case 'progressbar':
      return 'progress';
    default:
      return null;
  }
}

// Canonical role for an Appium element: explicit a11y trait wins, then the iOS
// XCUI tag, then the Android widget class/tag, else `node`. Never from text.
function roleOfEl(tag, get) {
  const trait = get('accessibility-role') || get('role') || '';
  if (trait) {
    const r = roleFromTrait(trait);
    if (r) return r;
  }
  const xc = roleFromXcui(tag);
  if (xc) return xc;
  const cls = get('class') || tag;
  const ar = roleFromAndroid(cls);
  if (ar) return ar;
  return 'node';
}

// Stable developer id: resource-id (Android) > accessibility-id / testID > name.
// On iOS, `name` is the accessibilityIdentifier when set (else the label), so we
// only take it when it looks like an identifier (no spaces) to avoid folding
// localized text into the hash; the display label is captured separately.
function idOfEl(get) {
  const rid = get('resource-id');
  if (rid && rid.trim()) {
    const leaf = rid.includes('/') ? rid.split('/').pop() : rid;
    if (leaf && leaf.trim()) return leaf.trim();
  }
  for (const key of ['accessibility-id', 'testID', 'test-id', 'nativeID']) {
    const v = get(key);
    if (v && v.trim()) return v.trim();
  }
  const name = get('name');
  if (name && name.trim() && !/\s/.test(name.trim())) return name.trim();
  return null;
}

// Android SYSTEM-CHROME node: a view in the platform's own `android:` resource
// namespace (android:id/navigationBarBackground, android:id/statusBarBackground,
// and framework decor generally), as opposed to app content in the app package's
// namespace (com.example.app:id/...). The OS draws these to the device insets /
// screen edges, so their frame legitimately spills past the app content box. An
// overflow marker on them is pure noise about system UI the developer neither
// owns nor can fix. Excluded from OVERFLOW candidacy, mirroring the Windows
// caption-chrome exclusion. `idOfEl` strips the namespace, so we read the RAW
// resource-id here.
// Input-type refinement for textfields. iOS SecureTextField + Android password
// flags => password; numeric/email keyboards refine the rest. Never text value.
function typeOfEl(tag, get, role) {
  if (role !== 'textfield') return null;
  if (tag === 'XCUIElementTypeSecureTextField') return 'password';
  if (tag === 'XCUIElementTypeSearchField') return 'search';
  if (get('password') === 'true') return 'password';
  const it = (get('inputType') || get('keyboardType') || '').toLowerCase();
  if (it.includes('password')) return 'password';
  if (it.includes('email')) return 'email';
  if (it.includes('number') || it.includes('numeric') || it.includes('phone')) return 'number';
  if (it.includes('search')) return 'search';
  const t = (get('type') || '').toLowerCase();
  if (['text', 'password', 'email', 'number', 'search'].includes(t)) return t;
  return 'text';
}

// Locale-independent credential purpose. Uses platform autofill/content-type
// metadata only; visible labels and placeholders are intentionally excluded.
function inputPurposeOfEl(tag, get, role) {
  if (role !== 'textfield') return null;
  const hint = [
    get('textContentType'),
    get('content-type'),
    get('autofillHints'),
    get('autofill-hints'),
    get('importantForAutofill'),
    get('autocomplete'),
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
  const type = typeOfEl(tag, get, role);
  if (hint.includes('onetimecode') || hint.includes('one-time-code') || hint.includes('smsotp'))
    return 'otp';
  if (hint.includes('password') || type === 'password') return 'password';
  if (hint.includes('username')) return 'username';
  if (hint.includes('email') || type === 'email') return 'email';
  if (hint.includes('phone') || hint.includes('telephone')) return 'phone';
  return null;
}

// Language-independent icon identity from a stable attribute (no visible text).
function iconOfEl(get) {
  for (const key of ['icon', 'icon-name', 'data-icon']) {
    const v = get(key);
    if (v && v.trim()) return v.trim();
  }
  return null;
}

// Transient heuristic: progress role, live-region announcement, or a flagged
// class drops the node + subtree from the hash (matches the web/RN SDKs).
function isTransientEl(get, role, cls) {
  if (role === 'progress') return true;
  const live = (get('aria-live') || get('live-region') || '').toLowerCase();
  if (live === 'assertive' || live === 'polite') return true;
  const trait = (get('accessibility-role') || get('role') || '').toLowerCase();
  if (trait === 'alert' || trait === 'status' || trait === 'timer') return true;
  if (
    /\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test(
      (cls || '').toLowerCase(),
    )
  )
    return true;
  return false;
}

// The RAW value-role of an Appium element for the Layer-2 value-class (docs/
// signature.md "Value-state"), derived from a11y traits + tag/class, NEVER from
// text. Distinct from roleOfEl: it returns one of the value-role names
// (status/log/progressbar/meter/timer/output) for the matching a11y roles and
// "textfield" for text-entry controls, so the canonical is_value_bearing test
// sees the RAW role the oracle expects. A live-region (polite/assertive) maps to
// "status" so a counter/stopwatch readout is value-bearing WITHOUT opt-in.
// Returns null for chrome and for password fields (never read).
function valueRoleOfEl(tag, get, role) {
  const trait = (get('accessibility-role') || get('role') || '').toLowerCase();
  if (
    trait === 'status' ||
    trait === 'log' ||
    trait === 'progressbar' ||
    trait === 'meter' ||
    trait === 'timer' ||
    trait === 'output'
  ) {
    return trait;
  }
  const live = (get('aria-live') || get('live-region') || '').toLowerCase();
  if (live === 'polite' || live === 'assertive') return 'status';
  // Text-entry controls hold an editable value: they are textfield value-roles.
  // A secure (password) field is never read.
  if (role === 'textfield') {
    if (tag === 'XCUIElementTypeSecureTextField') return null;
    if (get('password') === 'true') return null;
    return 'textfield';
  }
  return null;
}

// The displayed data value of a value-role element, NEVER from a password. For
// text-entry controls and status/output/live nodes Appium surfaces the current
// content under `value` (iOS) / `text` (Android) / content-desc; we read those
// stable attributes only. The raw value never enters the canonical key (it is
// bucketed to a value-class), and it feeds the Layer-1 content fingerprint.
function valueOfEl(get) {
  const v = get('value');
  if (v != null && v !== '') return String(v);
  const t = get('text');
  if (t != null && t !== '') return String(t);
  const cd = get('content-desc');
  if (cd != null && cd !== '') return String(cd);
  return '';
}

// Display-only accessible name (label/content-desc/text). Never in the hash.
function nameOfEl(get) {
  return (get('label') || get('content-desc') || get('text') || get('value') || '')
    .trim()
    .split('\n')[0]
    .trim();
}

// The element's on-screen frame as {l,t,r,b} in device pixels, or null when no
// geometry is exposed. Appium surfaces bounds in two platform shapes, both of
// which the page source carries as plain attributes (no extra round-trip):
//   Android (UiA2): bounds="[left,top][right,bottom]"
//   iOS (XCUITest): x="..", y="..", width="..", height=".."
// This is the same geometry an evidence screenshot crops to; we read it from the
// page source so evidence and interaction checks reuse the same geometry.
function rectOfEl(get) {
  const b = get('bounds');
  if (b) {
    const m = b.match(/^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$/);
    if (m) {
      const l = parseInt(m[1], 10),
        t = parseInt(m[2], 10);
      const r = parseInt(m[3], 10),
        bot = parseInt(m[4], 10);
      if ([l, t, r, bot].every(Number.isFinite)) return { l, t, r, b: bot };
    }
  }
  const xs = get('x'),
    ys = get('y'),
    ws = get('width'),
    hs = get('height');
  if (xs !== '' && ys !== '' && ws !== '' && hs !== '') {
    const x = parseFloat(xs),
      y = parseFloat(ys),
      w = parseFloat(ws),
      h = parseFloat(hs);
    if ([x, y, w, h].every(Number.isFinite)) return { l: x, t: y, r: x + w, b: y + h };
  }
  return null;
}

// CONTENT-BUG classifier (deterministic, label-based). Byte-identical rule to
// the web runner's reasonOf (runners/web/runner.mjs): fires ONLY on a GROUND-TRUTH
// artifact impossible to render as legitimate copy, matched on STRUCTURE (a literal
// token), never on natural language. Two classes, first match wins so a label
// carries one reason:
//   [object Object]     -> object-object       (an object coerced to a string)
//   {{ .. }} / ${ .. }  -> unrendered-template  (the binding never evaluated)
// The bare words undefined/null/NaN are NOT matched: they occur in real copy and
// code samples ("undefined behavior", a "Null Island" pin), so keying on them
// false-positived. We scan the displayed text the runner already gathers (the same
// value/text/content-desc nameOfEl reads).
// Prose guard for BOTH artifact kinds: a real leaked artifact IS the label (bare,
// or a short field-name prefix like "Price: X"); prose that merely MENTIONS the
// token ("[object Object]" or the "{{ }}" syntax) inside a sentence is legitimate
// copy. Fire only when, with the artifact(s) removed, the remainder is a SHORT
// label with no sentence structure.
function contentBugReason(text) {
  if (!text) return null;
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
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
}

// The raw displayed text of an element for the content-bug scan: label /
// content-desc / text / value, NEVER from a password field (it would leak the
// secret AND the masked dots are not a content bug). Full string (not clipped /
// not first-line-only like nameOfEl) so a multi-line "[object Object]" embedded
// past a newline is still caught.
function displayTextOfEl(tag, get, role) {
  if (role === 'textfield') {
    if (tag === 'XCUIElementTypeSecureTextField') return '';
    if (get('password') === 'true') return '';
  }
  for (const key of ['label', 'content-desc', 'text', 'value']) {
    const v = get(key);
    if (v != null && v !== '') return String(v);
  }
  return '';
}

// HOST-SIDE pure reducer: collected (key, reason, text) content-bug tuples -> the
// sorted EXPLORE:CONTENTBUG `items` array (byte-identical shape to the web runner
// / the Rust map.rs parser: each item is {key, reason, text}). Deduped on
// key|reason, sorted by key then reason, text clipped to 80 chars (display
// detail; the key+reason are the stable identity). Pure + deterministic.
function contentBugItems(raw) {
  const out = [];
  const seen = new Set();
  for (const it of raw || []) {
    if (!it || !it.key || !it.reason) continue;
    const k = it.key + '|' + it.reason;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ key: it.key, reason: it.reason, text: String(it.text || '').slice(0, 80) });
  }
  out.sort((a, b) =>
    a.key < b.key ? -1 : a.key > b.key ? 1 : a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0,
  );
  return out;
}

// BROKEN-ASSET classifier, tofu only on native: a rendered U+FFFD, the
// replacement character an encoding failure paints as tofu. The web runner's
// other reasons (img/font) interrogate DOM subresources that do not exist in a
// native view tree, so they stay web-only and the native `reason` vocabulary is
// a strict subset of the web one (runners/web/hygiene-oracles.mjs
// brokenAssetScan). Pure substring test over the displayed text the runner
// already gathers (never a password: displayTextOfEl blanks secure fields), so
// ordinary text never trips it and the control stays silent when clean.
function tofuReason(text) {
  return text && text.includes('�') ? 'tofu' : null;
}

// Provenance ledger for the broken-asset (tofu) oracle: every value the fuzzer
// types is recorded so a fuzzer-injected U+FFFD (an emoji / RTL probe the app
// echoes back) is not mistaken for an app encoding bug. Mirrors the web runner's
// brokenAssetScan provenance guard. Native RN has no <img>/favicon subresources,
// so tofu is the only broken-asset signal and the only one needing provenance.
const INJECTED_VALUES = new Set();
// A reflected fuzzer value (a probe the app echoes back) is not the app's own
// content: shared by the tofu (broken-asset) AND content-bug oracles. Native RN
// renders reflected text intact (no HTML parsing), so the direct substring test in
// both directions is sufficient (no artifact-fragment handling needed as on web).
function fromFuzzInjection(text) {
  const n = String(text == null ? '' : text).toLowerCase();
  if (!n) return false;
  for (const raw of INJECTED_VALUES) {
    const v = String(raw).toLowerCase();
    if (!v) continue;
    if (v.indexOf(n) !== -1 || (v.length >= 3 && n.indexOf(v) !== -1)) return true;
  }
  return false;
}

// HOST-SIDE pure reducer: collected (key, detail) tofu tuples -> the sorted
// EXPLORE:BROKENASSET `items` array (same shape the web runner emits / the Rust
// map.rs parser reads: each item is {key, reason, detail}). Deduped on key,
// sorted by key, detail trimmed + clipped to 60 chars (display detail; the
// key+reason are the stable identity). Pure + deterministic, so it is
// unit-testable in Node without a device.
function brokenAssetItems(raw) {
  const out = [];
  const seen = new Set();
  for (const it of raw || []) {
    if (!it || !it.key || !it.reason) continue;
    const k = it.key + '|' + it.reason;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({
      key: it.key,
      reason: it.reason,
      detail: String(it.detail || '')
        .trim()
        .slice(0, 60),
    });
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  return out;
}

// HOST-SIDE pure reducer: collected tappable frames + device safe-area insets ->
// the EXPLORE:SAFEAREA `items` array (same shape as the Flutter explorer / the
// Rust map.rs parser: each item is {key, edge, by}). A tappable whose frame
// intersects an inset band is drawn under the status bar / notch (top), the home
// indicator (bottom), or a landscape notch / rounded corner (left/right), so it
// is obscured or hard to tap. `insets` is {top,bottom,left,right} in the SAME px
// space as the frames and screenRect (Android: physical px from getSystemBars();
// iOS: the XCUITest driver does not expose safe-area insets, so this is called
// with zero insets and stays silent -- use the Flutter path for iOS safe-area
// ground truth). A device with NO insets on every edge yields [] (nothing to
// collide with). An intrusion of 1px or less is flush-adjacent rounding, not a
// collision. Deduped by key|edge, capped at 20, sorted by key then edge so the
// marker is byte-identical run to run. Pure + deterministic (no device needed to
// test).
function safeAreaItems(tapRects, insets, screenRect) {
  if (!insets || !screenRect) return [];
  const top = insets.top || 0,
    bottom = insets.bottom || 0;
  const left = insets.left || 0,
    right = insets.right || 0;
  if (top <= 0 && bottom <= 0 && left <= 0 && right <= 0) return [];
  const H = screenRect.b - screenRect.t,
    W = screenRect.r - screenRect.l;
  const els = (tapRects || []).filter(
    (e) => e && e.rect && e.rect.r - e.rect.l > 0 && e.rect.b - e.rect.t > 0,
  );
  const out = [];
  const seen = new Set();
  const add = (key, edge, overlap) => {
    if (overlap <= 1) return; // flush-adjacent rounding, not a collision
    const dedup = key + '|' + edge;
    if (seen.has(dedup)) return;
    seen.add(dedup);
    if (out.length < 20) out.push({ key, edge, by: Math.round(overlap) });
  };
  for (const e of els) {
    const r = e.rect;
    if (top > 0) add(e.key, 'top', Math.min(r.b, screenRect.t + top) - r.t);
    if (bottom > 0) {
      const bandTop = screenRect.b - bottom;
      add(e.key, 'bottom', r.b - Math.max(r.t, bandTop));
    }
    if (left > 0) add(e.key, 'left', Math.min(r.r, screenRect.l + left) - r.l);
    if (right > 0) {
      const bandLeft = screenRect.r - right;
      add(e.key, 'right', r.r - Math.max(r.l, bandLeft));
    }
  }
  // H/W are referenced for clarity of the band model; guard against a degenerate
  // frame so a zero-size screen never manufactures a collision.
  if (!(H > 0 && W > 0)) return [];
  out.sort((x, y) =>
    x.key < y.key ? -1 : x.key > y.key ? 1 : x.edge < y.edge ? -1 : x.edge > y.edge ? 1 : 0,
  );
  return out;
}

// HOST-SIDE pure predicate: the BLANK-SCREEN (white-screen-of-death) verdict
// over facts the tree walk already gathered (mirrors runners/web/
// hygiene-oracles.mjs blankScreenScan). Blank iff the page source shows ZERO
// visible text labels AND ZERO tappables AND no text field / image (a
// media-only or input-only screen is NOT blank, the web scan's media check)
// while the window frame is non-zero. A driver that exposed no window geometry
// yields [] (cannot confirm the viewport, never guess-and-flag). Returns one
// [{key:"root", w, h}] record naming the scanned root and the window frame, or
// [] when any content is visible. The CALLER additionally confirms a blank
// verdict against a second settled snapshot before emitting, so a
// transiently-empty a11y tree (app boot) never false-positives.
function blankScreenItems(labels, elements, roleSeen, screenRect) {
  if (!screenRect) return [];
  const w = screenRect.r - screenRect.l,
    h = screenRect.b - screenRect.t;
  if (!(w > 0 && h > 0)) return [];
  if ((labels && labels.length) || (elements && elements.length)) return [];
  if (roleSeen && ((roleSeen.textfield || 0) > 0 || (roleSeen.image || 0) > 0)) return [];
  // A visible LOADING / progress / status indicator (a native ActivityIndicator
  // normalizes to the `progress` role; a live status region to `status`) means the
  // screen is MID-LOAD, not a permanently-blank WSOD -- never fire while one shows.
  if (roleSeen && ((roleSeen.progress || 0) > 0 || (roleSeen.status || 0) > 0)) return [];
  return [{ key: 'root', w: Math.round(w), h: Math.round(h) }];
}

// Interactive: a tappable role, or an explicit clickable/enabled-button flag.
function isTappableEl(get, role) {
  if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
    return true;
  if (get('clickable') === 'true') return true;
  return false;
}

// The canonical roles that, when present on an element, expose a real semantic
// role to assistive tech (a screen reader announces "button", "link", ...). A
// clickable element whose canonical role is NOT one of these (it normalized to
// the generic `group`/`node`, i.e. an Android android.view.ViewGroup with no
// accessibilityRole) is operable by finger but role-less to AT: the WCAG 4.1.2
// no_role gap. This is the host-readable native equivalent of the fiber probe's
// `accessibilityRole == null` test, used by the native-fallback groundtruth.
const AT_ROLES = {
  button: 1,
  link: 1,
  menuitem: 1,
  tab: 1,
  checkbox: 1,
  switch: 1,
  radio: 1,
  slider: 1,
  menu: 1,
  textfield: 1,
  listitem: 1,
};
function exposesAtRole(role) {
  return !!AT_ROLES[role];
}

// Does the element actually carry a press affordance natively? On Android RN an
// operable Pressable surfaces clickable="true"; a real <Button> widget is also
// clickable. We require the native clickable flag (not merely a tappable ROLE)
// so a decorative element that merely normalized to `button` by class never
// counts, and only genuinely pointer-operable nodes become gap candidates.
function isPointerOperable(get) {
  return get('clickable') === 'true' || get('long-clickable') === 'true';
}

// Clip an accessible name to the display label cap (display only; never hashed).
function clipLabel(name) {
  if (name.length <= MAX_LABEL_LEN) return name;
  const suffix = '#' + fnv1a(name);
  return name.slice(0, MAX_LABEL_LEN - suffix.length) + suffix;
}

// Jetpack Compose can expose one control twice through UiAutomator2: a keyed,
// clickable generic semantics wrapper and an unkeyed actionable child occupying
// the same hit rectangle. Treat that pair as one control. The stable key comes
// from the wrapper while role/name come from the semantic child.
function reconcileComposeControls(elements, nativeCandidates) {
  const input = (elements || []).map((e) => ({ ...e }));
  const removed = new Set();
  const generic = new Set(['node', 'group']);
  const sameBounds = (a, b) =>
    Array.isArray(a) &&
    Array.isArray(b) &&
    a.length === 4 &&
    a.every((v, i) => Math.abs(Number(v) - Number(b[i])) <= 1);

  for (let i = 0; i < input.length; i++) {
    const keyed = input[i];
    if (!keyed.key || !generic.has(keyed.role) || !keyed.bounds) continue;
    for (let j = 0; j < input.length; j++) {
      const semantic = input[j];
      if (
        i === j ||
        semantic.key ||
        generic.has(semantic.role) ||
        !sameBounds(keyed.bounds, semantic.bounds)
      )
        continue;
      keyed.role = semantic.role;
      if (!keyed.label && semantic.label) keyed.label = semantic.label;
      keyed.sel = `key:${keyed.key}`;
      keyed.nokey = false;
      removed.add(j);
      break;
    }
  }

  // Removing an id-less duplicate must not leave holes in role:<role># indexes.
  const perRole = {};
  const controls = input
    .filter((_, i) => !removed.has(i))
    .map((e) => {
      if (e.key) return e;
      const idx = perRole[e.role] || 0;
      perRole[e.role] = idx + 1;
      return { ...e, sel: `role:${e.role}#${idx}` };
    });

  const byId = new Map(
    (nativeCandidates || []).filter((c) => c && c.id != null).map((c) => [c.id, { ...c }]),
  );
  for (const e of controls) {
    if (!e.key || !byId.has(e.key)) continue;
    const c = byId.get(e.key);
    if (exposesAtRole(e.role)) c.rolePresent = true;
    if (e.label) c.namePresent = true;
  }
  return { elements: controls, nativeCandidates: [...byId.values()] };
}

export { reconcileComposeControls };

// ---- a tiny, dependency-free XML tree parser ------------------------------
// Appium page source is well-formed XML. We tokenize tags (open / self-close /
// close) and build a nesting tree of { tag, attrs, children }. Text nodes are
// ignored (all signal lives in attributes), which is exactly what we want since
// localized text never enters the signature.
function parseXml(xml) {
  const tagRe = /<(\/)?([A-Za-z_][\w.\-]*)((?:\s+[\w:.\-]+="[^"]*")*)\s*(\/?)>/g;
  const attrRe = /([\w:.\-]+)="([^"]*)"/g;
  const root = { tag: '#root', attrs: {}, children: [] };
  const stack = [root];
  let m;
  while ((m = tagRe.exec(xml))) {
    const closing = m[1] === '/';
    const tag = m[2];
    const rawAttrs = m[3] || '';
    const selfClose = m[4] === '/';
    if (closing) {
      if (stack.length > 1) stack.pop();
      continue;
    }
    const attrs = {};
    let a;
    while ((a = attrRe.exec(rawAttrs))) attrs[a[1]] = decodeXmlEntities(a[2]);
    const node = { tag, attrs, children: [] };
    stack[stack.length - 1].children.push(node);
    if (!selfClose) stack.push(node);
  }
  return root;
}
function decodeXmlEntities(s) {
  return (
    s
      .replace(/&lt;/g, '<')
      .replace(/&gt;/g, '>')
      .replace(/&quot;/g, '"')
      .replace(/&apos;/g, "'")
      // Numeric character references: serializers escape non-ASCII / control
      // chars this way (e.g. Android's &#10; newlines, a tofu U+FFFD as
      // &#xFFFD;). Decoded BEFORE &amp; so a literal "&amp;#65;" stays "&#65;".
      .replace(/&#x([0-9a-fA-F]+);/g, (_, h) => String.fromCodePoint(parseInt(h, 16)))
      .replace(/&#([0-9]+);/g, (_, d) => String.fromCodePoint(parseInt(d, 10)))
      .replace(/&amp;/g, '&')
  );
}

// Test one element's stable id / canonical role against the active Layer-3
// value-node selectors (docs/signature.md "Value-state"). key:<id> compares the
// node's stable id; role:<role>#<idx> matches the idx-th element of that
// canonical role in document order (out.roleSeen supplies the running index).
function matchesValueNode(out, id, role, myRoleIndex) {
  const sels = out.valueNodeSelectors || [];
  if (!sels.length) return false;
  for (const sel of sels) {
    if (!sel) continue;
    if (sel.indexOf('key:') === 0) {
      const want = sel.slice(4);
      if (want && id != null && id === want) return true;
    } else if (sel.indexOf('role:') === 0) {
      const hash = sel.indexOf('#');
      if (hash < 0) continue;
      const wantRole = sel.slice(5, hash);
      const idx = parseInt(sel.slice(hash + 1), 10);
      if (!(idx >= 0)) continue;
      if (role === wantRole && myRoleIndex === idx) return true;
    }
  }
  return false;
}

// A stable, locale-invariant key for an oracle finding (overflow / content-bug),
// in reproit's selector grammar so it lines up with the elements list and the
// `key:<id>` replay selectors. id-bearing nodes are addressed by their developer
// id; others by their canonical role + document-order index (out.roleSeen[role]
// already counts this exactly). NEVER the visible text, so a translated string
// does not change a finding's identity (matches the web runner's keyOf intent).
function oracleKeyOf(id, role, roleIndex) {
  if (id != null) return 'key:' + id;
  return 'role:' + role + '#' + roleIndex;
}

// Build the canonical Node tree from a parsed XML element subtree. Invisible
// elements (visible="false") are skipped but their visible descendants are
// hoisted, matching the SDKs. The display labels / elements list are collected
// along the way. Returns an array of canonical Node children. `parentRect` is
// the on-screen frame of the nearest visible ancestor (for the overflow SPILL
// test); null at the root.
function buildNodes(xmlEl, out, parentRect) {
  const nodes = [];
  for (const child of xmlEl.children) {
    appendNode(child, out, nodes, parentRect);
  }
  return nodes;
}
function appendNode(xmlEl, out, into, parentRect) {
  const attrs = xmlEl.attrs;
  const get = (name) => (attrs[name] != null ? attrs[name] : '');
  if (get('visible') === 'false') {
    // hoist visible descendants of an invisible wrapper (keep the same parent
    // frame: an invisible wrapper has no contributing geometry of its own)
    for (const child of xmlEl.children) appendNode(child, out, into, parentRect);
    return;
  }
  const tag = xmlEl.tag;
  const cls = get('class') || tag;
  const role = roleOfEl(tag, get);
  const id = idOfEl(get);
  // Document-order index of this element among same-canonical-role peers, for a
  // Layer-3 role:<role>#<idx> value-node selector. Incremented for every element.
  const myRoleIndex = out.roleSeen[role] || 0;
  out.roleSeen[role] = myRoleIndex + 1;
  // On-screen frame (page-source geometry), used for
  // parent frame passed to children. Null when no geometry is exposed.
  const rect = rectOfEl(get);
  const okey = oracleKeyOf(id, role, myRoleIndex);
  // DFS enter index (every element consumes a slot), recorded on each tappable
  // frame so host-side reducers can interval-compare ancestor/descendant pairs.
  const enterSeq = out.walkSeq++;
  let tapRec = null;

  // Value-state (Layer 2): a value-role element (by trait/tag, or a live region)
  // or a Layer-3 opt-in node is value-bearing. Value-bearing WINS over the
  // transient heuristic, so a role=status / live-region counter that the
  // transient heuristic would otherwise drop is kept as a value node instead,
  // and its updates produce DISTINCT value-states.
  const vrole = valueRoleOfEl(tag, get, role);
  const optIn = matchesValueNode(out, id, role, myRoleIndex);
  const valueBearing = !!vrole || optIn;
  const transient = !valueBearing && isTransientEl(get, role, cls);
  const node = { role };
  if (id != null) node.id = id;
  const type = typeOfEl(tag, get, role);
  if (type != null) node.type = type;
  const icon = iconOfEl(get);
  if (icon != null) node.icon = icon;
  if (valueBearing) {
    node.value = valueOfEl(get);
    // The flag makes the canonical is_value_bearing accept the node even when
    // roleOfEl normalized its raw value-role (status/output/...) to "node".
    node.value_node = true;
    // Layer-1 content fingerprint: a value node's stable key + its raw value.
    const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
    out.textNodes.push([fkey, node.value]);
  }
  if (transient) {
    node.transient = true;
    into.push(node);
    return;
  }

  // CONTENT-BUG oracle (deterministic, label scan): a rendered label carrying a
  // stringify/template artifact ([object Object], whole-word undefined/null/NaN,
  // an unrendered {{..}}/${..}). Scans the displayed text the runner already
  // gathers; addressed by the same stable locale-invariant key, so a clean app
  // stays silent. Skips secure fields (never read a password).
  const dtext = displayTextOfEl(tag, get, role);
  const cbReason = contentBugReason(dtext);
  // Skip a reflected fuzzer probe (e.g. a typed "{{7*7}}" the app echoes back).
  if (cbReason && !fromFuzzInjection(dtext))
    out.contentBugs.push({ key: okey, reason: cbReason, text: dtext });

  // BROKEN-ASSET oracle (tofu only on native): a rendered U+FFFD is an encoding
  // failure leaked to the screen. Same displayed text and stable key as the
  // content-bug scan; silent when every label decodes cleanly.
  const baReason = tofuReason(dtext);
  // Provenance: skip tofu the fuzzer itself typed (a reflected U+FFFD probe), not
  // an app encoding bug.
  if (baReason && !fromFuzzInjection(dtext))
    out.brokenAssets.push({ key: okey, reason: baReason, detail: dtext });

  // Layer-1 content fingerprint over keyed text-bearing nodes (runner-local, NOT
  // canonical): a keyed text/static element's own value contributes (stable-key,
  // text). This catches a display whose text changes without any structural move
  // (a calculator/counter) so the action is seen as EFFECTIVE even when the node
  // was not detected as a value-role. The raw text never enters the canonical key.
  if (id != null && !valueBearing && (role === 'text' || role === 'header')) {
    const own = valueOfEl(get);
    if (own) out.textNodes.push(['text:' + id, own]);
  }

  // display labels + elements list (never in the hash)
  const name = nameOfEl(get);
  if (name) {
    const lbl = clipLabel(name);
    if (!out.seenLabel.has(lbl)) {
      out.seenLabel.add(lbl);
      out.labels.push(lbl);
    }
  }
  if (isTappableEl(get, role)) {
    const display = name ? clipLabel(name) : '';
    const idx = out.perRole[role] || 0;
    out.perRole[role] = idx + 1;
    const sel = id != null ? `key:${id}` : `role:${role}#${idx}`;
    const bounds = rect
      ? [
          Math.round(rect.l),
          Math.round(rect.t),
          Math.round(rect.r - rect.l),
          Math.round(rect.b - rect.t),
        ]
      : null;
    const purpose = inputPurposeOfEl(tag, get, role);
    out.elements.push({ sel, role, label: display, bounds, key: id, nokey: id == null, purpose });
    // Tappable frame + its DFS interval, consumed by the SAFE-AREA reducer in
    // snapshot(). Zero-area frames are skipped.
    if (rect && rect.r - rect.l > 0 && rect.b - rect.t > 0) {
      tapRec = { key: okey, rect, enter: enterSeq, exit: 0 };
      out.tapRects.push(tapRec);
    }
  }
  if (name && rect) {
    out.texts.push({
      text: clipLabel(name),
      bounds: [
        Math.round(rect.l),
        Math.round(rect.t),
        Math.round(rect.r - rect.l),
        Math.round(rect.b - rect.t),
      ],
    });
  }

  // NATIVE-FALLBACK GROUNDTRUTH candidate (graph-1 from graph 2). The fiber probe
  // (graph 1) is the primary operability oracle, but the uiautomator2 driver has
  // NO JS transport into the RN runtime on a real device, so on Android it yields
  // nothing. The native a11y tree the runner already reads ALREADY encodes the
  // gap: a finger-operable Pressable that exposed an accessibilityRole renders as
  // an android.widget.Button (role `button`); one that did NOT (or set
  // accessible={false}) renders as a bare android.view.ViewGroup (role `group`).
  // So we collect every pointer-operable element that carries a STABLE id (the
  // join key the developer can address + fix) and record whether it exposes a
  // real AT role / name. The id requirement also filters dev-build chrome (the
  // RN "Open debugger" warning bubble is clickable but id-less). When the fiber
  // probe is empty, groundtruthFromNative() turns these into the same elements
  // list the engine parses: role-less operable -> no_role (+ pointer_only).
  if (id != null && isPointerOperable(get)) {
    out.nativeCandidates.push({
      id,
      rolePresent: exposesAtRole(role),
      namePresent: !!name,
    });
  }

  node.children = buildNodes(xmlEl, out, rect || parentRect);
  // DFS exit index: closes this element's interval AFTER its whole subtree
  // consumed enter slots, so `enter < other.enter < exit` iff `other` is a
  // descendant (host-side reducers' ancestor/descendant wrapping exclusion).
  const exitSeq = out.walkSeq++;
  if (tapRec) tapRec.exit = exitSeq;
  into.push(node);
}

// The screen anchor: the foreground activity (Android) or the app bundle/window
// (iOS), when observable. The route/activity is the canonical anchor prefix.
//
// DEEP-LINK PARITY is EXCLUDED on React Native / native iOS / Android (ground
// truth, not effort). That oracle reopens each visited route's URL COLD (a deep
// link) and diffs the structure, so it needs a URL the harness can read off the
// current screen and re-open. This anchor is a foreground activity / bundle /
// window name, NOT a per-screen address: a native screen reached by tapping
// exposes no URL, and Appium can only fire a deep link (`mobile: deepLink` /
// openURL) for a scheme+path the app declared in its manifest -- a private
// mapping the fuzzer cannot infer from a tapped screen. So there is no
// derivable deep link for an arbitrary reached screen. Web, where the address
// bar IS the route, is where this oracle applies.
function anchorFrom(xmlRoot, activity) {
  if (activity && String(activity).trim()) return String(activity).trim();
  // Fall back to the top window/application element's name if it is an id-like
  // token (avoids folding a localized window title into the anchor).
  const top = xmlRoot.children[0];
  if (top) {
    const name = top.attrs.name || '';
    if (name && !/\s/.test(name)) return name;
  }
  return null;
}

