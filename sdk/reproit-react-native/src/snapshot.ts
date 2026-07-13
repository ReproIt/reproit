/**
 * Live state snapshot for React Native.
 *
 * RN has no DOM and no global "accessibility tree" object an app can read
 * synchronously the way Flutter exposes its semantics tree. What it DOES have at
 * runtime is the React fiber tree of the mounted component hierarchy. We walk
 * that tree into a CANONICAL Node tree (role + id + type + icon + children) and
 * hash it with the canonical structural signature (see `signature.ts`), so the
 * production signature is byte-identical to the Rust oracle, the web SDK, and the
 * runners. Localized text NEVER enters the hash (rule 1); it is kept only as a
 * display-only `labels` field for `map --show`.
 *
 * This mirrors `runners/rn/runner.mjs`, which builds the same canonical Node tree
 * out of Appium's accessibility XML at TEST time, so prod nodes == test nodes and
 * the signatures align 1:1.
 *
 * RN trait -> canonical role mapping (`accessibilityRole` / `role` / native
 * traits / host component type), most-specific first:
 *   header / heading                                  -> header
 *   button / imagebutton / togglebutton               -> button
 *   link                                              -> link
 *   search / searchbox / combobox / textfield (input) -> textfield
 *   image / img                                       -> image
 *   switch                                            -> switch
 *   checkbox                                          -> checkbox
 *   radio                                             -> radio
 *   adjustable / slider                               -> slider
 *   tab                                               -> tab
 *   tablist / menubar / toolbar                       -> menu
 *   menu                                              -> menu
 *   menuitem                                          -> menuitem
 *   list / tablist (container)                        -> list
 *   listitem / cell                                   -> listitem
 *   alert / dialog                                    -> dialog
 *   text / summary / static-text host                 -> text
 *   progressbar / spinner / activityindicator         -> progress (transient)
 *   (RN root container)                               -> screen
 *   anything else                                     -> node
 *
 * LIMITATIONS (honest, see README): the fiber tree is internal React state. We
 * read it defensively and degrade to an empty snapshot if React's internals
 * change shape. We cannot cheaply test on-screen occlusion the way the web SDK
 * uses getBoundingClientRect; React Navigation typically unmounts inactive
 * screens, which keeps this close to correct, but tab navigators that keep tabs
 * mounted are best-effort.
 */
import { signatureOf, type Node } from './signature';
import type { ResolvedConfig } from './types';

/**
 * A selector that marks EXTRA value-bearing nodes (Layer 3, docs/signature.md
 * "Value-state"). Same grammar as `value_nodes:` in reproit.yaml:
 *   key:<id>          -> testID / nativeID / accessibilityIdentifier / id / name
 *   role:<role>#<idx> -> the idx-th node of that canonical role (document order)
 * Installed via {@link setValueNodeSelectors}; consulted while building nodes.
 */
let VALUE_NODE_SELECTORS: string[] = [];

/** @internal Install the Layer-3 opt-in value-node selectors (from config). */
export function setValueNodeSelectors(list: string[] | null | undefined): void {
  VALUE_NODE_SELECTORS = Array.isArray(list) ? list.slice() : [];
}

/** An addressable element in the elements list (selector + display metadata). */
export interface SnapElement {
  /** `key:<id>` (stable id) or `role:<role>#<idx>` (structural index). */
  sel: string;
  /** Canonical role. */
  role: string;
  /** Display-only accessible name (never folded into the hash). */
  label: string;
  /** True when no stable id was available (warns the developer to add one). */
  nokey: boolean;
}

export interface Snapshot {
  /** Canonical structural signature (anchor + normalized Node tree). */
  sig: string;
  /** Screen anchor (route name) when known; null otherwise. */
  anchor: string | null;
  /** Display-only accessible names (for `map --show`), never in the hash. */
  labels: string[];
  /** Distinct tappable accessible names (used for tap-label hit resolution). */
  tappables: string[];
  /** Addressable elements with structural selectors + nokey metadata. */
  elements: SnapElement[];
  /** Whether any host node was found at all (null tree => skip snapshot). */
  any: boolean;
}

/** Roles that mean "this node is interactive" (mirrors web SDK + runner). */
const TAPPABLE_ROLES = new Set([
  'button',
  'link',
  'menuitem',
  'tab',
  'checkbox',
  'switch',
  'radio',
]);

/** Loosely-typed fiber node; React internals are not part of any public API. */
type Fiber = {
  tag?: number;
  type?: unknown;
  memoizedProps?: Record<string, unknown> | null;
  pendingProps?: Record<string, unknown> | null;
  stateNode?: unknown;
  child?: Fiber | null;
  sibling?: Fiber | null;
} | null;

/** A live text field's on-screen value + a stable label, for fingerprinting. */
export interface FieldValue {
  field: string;
  value: string;
}

/** First non-empty line of a string, trimmed. */
function firstLine(s: string): string {
  return s.trim().split('\n')[0].trim();
}

// ---- canonical Node-tree extraction from the fiber tree -------------------

/**
 * Native host component type names that map onto a canonical role regardless of
 * any explicit accessibilityRole. Covers the components RN renders for common
 * widgets so an app that uses plain RN components (no manual a11y role) still
 * produces a meaningful structural signature.
 */
function hostTypeRole(type: string): string | null {
  switch (type) {
    case 'RCTText':
    case 'RCTVirtualText':
    case 'AndroidTextView':
      return 'text';
    case 'RCTImageView':
    case 'RCTImage':
      return 'image';
    case 'RCTTextInput':
    case 'RCTSinglelineTextInputView':
    case 'RCTMultilineTextInputView':
    case 'AndroidTextInput':
      return 'textfield';
    case 'RCTSwitch':
    case 'AndroidSwitch':
      return 'switch';
    case 'RCTSlider':
    case 'ReactSlider':
      return 'slider';
    case 'RCTActivityIndicatorView':
    case 'AndroidProgressBar':
    case 'RCTActivityIndicatorViewManager':
      return 'progress';
    case 'RCTScrollView':
    case 'AndroidHorizontalScrollView':
      return 'list';
    default:
      return null;
  }
}

/**
 * Map an RN `accessibilityRole` / ARIA `role` string to the fixed canonical
 * vocabulary. Returns null when the trait does not name a known role (the caller
 * falls back to the host-component type, else `node`).
 */
function roleFromTrait(trait: string): string | null {
  switch (trait) {
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
    case 'spinbutton':
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
    case 'staticText':
      return 'text';
    case 'progressbar':
    case 'spinbutton-progress':
      return 'progress';
    case 'none':
    case 'group':
      return 'group';
    default:
      return null;
  }
}

/** Read the raw a11y role / ARIA role string from props (lowercased). */
function rawTrait(props: Record<string, unknown> | null): string {
  if (!props) return '';
  const r = props.accessibilityRole ?? props.role;
  if (typeof r === 'string') return r.trim();
  return '';
}

/**
 * Canonical role for a host fiber. accessibilityRole/role wins when it names a
 * known role; otherwise the host component type; otherwise `node`. The role
 * is NEVER derived from visible text.
 */
function roleOfFiber(fiber: NonNullable<Fiber>, props: Record<string, unknown> | null): string {
  const trait = rawTrait(props);
  if (trait) {
    const r = roleFromTrait(trait);
    if (r) return r;
  }
  if (typeof fiber.type === 'string') {
    const r = hostTypeRole(fiber.type);
    if (r) return r;
  }
  return 'node';
}

/** Optional input-type refinement, only for textfield-ish controls. */
function typeOfFiber(props: Record<string, unknown> | null, role: string): string | null {
  if (role !== 'textfield' || !props) return null;
  // Password first: secureTextEntry is the RN way to mark a password input.
  if (props.secureTextEntry === true) return 'password';
  // ARIA-style explicit type, if the app set one.
  const it = props.inputType ?? props['type'];
  if (typeof it === 'string') {
    const t = it.toLowerCase();
    if (['text', 'password', 'email', 'number', 'search'].includes(t)) return t;
  }
  // RN keyboardType maps onto our input-type vocabulary.
  const kt = props.keyboardType;
  if (typeof kt === 'string') {
    const k = kt.toLowerCase();
    if (k === 'email-address') return 'email';
    if (k === 'numeric' || k === 'number-pad' || k === 'decimal-pad' || k === 'phone-pad') return 'number';
    if (k === 'web-search') return 'search';
  }
  return 'text';
}

/**
 * Language-independent icon identity. We read only stable, non-localized
 * attributes: an explicit `data-icon` / `icon` / `iconName` prop, or a vector
 * icon's `name` when the component is flagged as an icon. Never visible text.
 */
function iconOfFiber(props: Record<string, unknown> | null): string | null {
  if (!props) return null;
  for (const key of ['data-icon', 'icon', 'iconName']) {
    const v = props[key];
    if (typeof v === 'string' && v.trim()) return v.trim();
  }
  return null;
}

/** Stable developer identifier: testID > nativeID > id > name. Omitted if none. */
function idOfFiber(props: Record<string, unknown> | null): string | null {
  if (!props) return null;
  for (const key of ['testID', 'accessibilityIdentifier', 'nativeID', 'id', 'name']) {
    const v = props[key];
    if (typeof v === 'string' && v.trim()) return v.trim();
  }
  return null;
}

/** True if this host fiber is a flickering/transient node (dropped from hash). */
function isTransientFiber(props: Record<string, unknown> | null, role: string): boolean {
  if (role === 'progress') return true; // spinner / activity indicator / progressbar
  if (!props) return false;
  // aria-live regions and alert/status roles announce transient content.
  const live = props['aria-live'] ?? props.accessibilityLiveRegion;
  if (typeof live === 'string') {
    const l = live.toLowerCase();
    if (l === 'assertive' || l === 'polite') return true;
  }
  const trait = rawTrait(props).toLowerCase();
  if (trait === 'alert' || trait === 'status' || trait === 'progressbar' || trait === 'timer') return true;
  // Explicit opt-out for app-marked transient nodes (toast/snackbar/badge etc).
  if (props['data-transient'] === true || props.reproitTransient === true) return true;
  return false;
}

/**
 * The RAW value-role of a host fiber for the Layer-2 value-class (docs/signature
 * .md "Value-state"), derived from a11y trait + host component type, NEVER from
 * text. Distinct from roleOfFiber: it returns one of the value-role names
 * (status/log/progressbar/meter/timer/output) for the matching a11y roles and
 * "textfield" for text-entry controls, so the canonical is_value_bearing test
 * sees the RAW role the oracle expects. An accessibilityLiveRegion (polite/
 * assertive) maps to "status" (a value-role) so a live region (a counter, a
 * stopwatch readout) is value-bearing WITHOUT any opt-in. Returns null for
 * chrome and for non-text inputs (a password field is never read).
 */
function valueRoleOfFiber(
  fiber: NonNullable<Fiber>,
  props: Record<string, unknown> | null,
): string | null {
  const trait = rawTrait(props).toLowerCase();
  if (
    trait === 'status' || trait === 'log' || trait === 'progressbar' ||
    trait === 'meter' || trait === 'timer' || trait === 'output'
  ) {
    return trait;
  }
  // accessibilityLiveRegion / aria-live (polite|assertive) -> a status value-role.
  const live = props ? (props['aria-live'] ?? props.accessibilityLiveRegion) : null;
  if (typeof live === 'string') {
    const l = live.toLowerCase();
    if (l === 'polite' || l === 'assertive') return 'status';
  }
  // Native text-entry controls hold an editable value: they are textfield
  // value-roles. A secureTextEntry (password) field is never read.
  if (typeof fiber.type === 'string' && hostTypeRole(fiber.type) === 'textfield') {
    if (props && props.secureTextEntry === true) return null;
    return 'textfield';
  }
  // An a11y-trait textfield with no host text-input component still holds a value.
  const r = trait ? roleFromTrait(trait) : null;
  if (r === 'textfield') {
    if (props && props.secureTextEntry === true) return null;
    return 'textfield';
  }
  return null;
}

/**
 * The displayed data value of a value-role fiber. For text-entry controls it is
 * the field's text/value (the same source as fingerprinting). For status/live/
 * output nodes it is RN's `accessibilityValue.text` (or `.now`) when set, else
 * the node's own direct string text children. Never a password (excluded above).
 */
function valueOfFiber(props: Record<string, unknown> | null): string {
  if (!props) return '';
  // RN accessibilityValue: { text?, now?, min?, max? } (or, loosely, a string).
  const av = props.accessibilityValue ?? props['aria-valuetext'] ?? props['aria-valuenow'];
  if (typeof av === 'string' && av.trim()) return av;
  if (typeof av === 'number') return String(av);
  if (av && typeof av === 'object') {
    const o = av as Record<string, unknown>;
    if (typeof o.text === 'string') return o.text;
    if (typeof o.now === 'string') return o.now;
    if (typeof o.now === 'number') return String(o.now);
  }
  // Form-control value (text input / controlled value).
  for (const key of ['text', 'value', 'defaultValue']) {
    const v = props[key];
    if (typeof v === 'string') return v;
    if (typeof v === 'number') return String(v);
  }
  // Fall back to the node's own direct string text children (a status readout).
  return nameOfProps(props);
}

/**
 * Test one host fiber against the active Layer-3 value-node selectors
 * (docs/signature.md "Value-state"). key:<id> compares the node's stable id;
 * role:<role>#<idx> matches the idx-th node of that canonical role in document
 * order across the mounted roots (computed lazily on first role:-selector use).
 */
function matchesValueNodeFiber(
  props: Record<string, unknown> | null,
  role: string,
  roleIndex: () => number,
): boolean {
  if (!VALUE_NODE_SELECTORS.length) return false;
  const id = idOfFiber(props);
  for (const sel of VALUE_NODE_SELECTORS) {
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
      if (role === wantRole && roleIndex() === idx) return true;
    }
  }
  return false;
}

/** True if this fiber corresponds to a real host (native) component. */
function isHost(fiber: NonNullable<Fiber>): boolean {
  // HostComponent fibers carry a string `type` (e.g. "RCTView", "RCTText").
  // Composite components have a function/object type; hosts have a string type.
  return typeof fiber.type === 'string';
}

/**
 * The accessible name of a fiber's props: accessibilityLabel, else the node's
 * own direct string text children. Used ONLY for the display-only labels list
 * and the elements list; never for the role and never folded into the hash.
 */
function nameOfProps(props: Record<string, unknown> | null | undefined): string {
  if (!props) return '';
  const a = props.accessibilityLabel ?? props['aria-label'];
  if (typeof a === 'string' && a.trim()) return firstLine(a);
  const children = props.children;
  if (typeof children === 'string') return firstLine(children);
  if (typeof children === 'number') return firstLine(String(children));
  if (Array.isArray(children)) {
    const parts: string[] = [];
    for (const c of children) {
      if (typeof c === 'string') parts.push(c);
      else if (typeof c === 'number') parts.push(String(c));
    }
    if (parts.length) return firstLine(parts.join(''));
  }
  return '';
}

function isTappable(props: Record<string, unknown> | null | undefined, role: string): boolean {
  if (!props) return TAPPABLE_ROLES.has(role);
  if (typeof props.onPress === 'function') return true;
  if (typeof props.onClick === 'function') return true;
  if (TAPPABLE_ROLES.has(role)) return true;
  const traits = props.accessibilityTraits;
  if (typeof traits === 'string' && TAPPABLE_ROLES.has(traits.toLowerCase())) return true;
  return false;
}

/** True if a node's props mark it (and its subtree) as accessibility-hidden. */
function isHiddenProps(props: Record<string, unknown> | null): boolean {
  return !!props && (props.accessibilityElementsHidden === true || props['aria-hidden'] === true);
}

interface BuildOut {
  labels: string[];
  tappables: string[];
  elements: SnapElement[];
  seenLabel: Set<string>;
  perRole: Record<string, number>;
  /**
   * Document-order count of host nodes seen per canonical role, used only to
   * resolve a Layer-3 `role:<role>#<idx>` value-node selector. Counts every host
   * node (not just tappables), matching the snapshot()-side resolution.
   */
  roleSeen: Record<string, number>;
  any: boolean;
}

/**
 * Recursively build the canonical child Node list for a fiber's subtree.
 * Non-host (composite) fibers are transparent: they are skipped but their host
 * descendants are hoisted, so the structural shape matches regardless of how
 * many composite wrappers an app uses (parallels the web SDK hoisting invisible
 * wrappers). Accessibility-hidden subtrees are dropped entirely.
 */
function buildChildren(
  fiber: NonNullable<Fiber>,
  cfg: ResolvedConfig,
  out: BuildOut,
): Node[] {
  const nodes: Node[] = [];
  let child = fiber.child ?? null;
  while (child) {
    appendNode(child, cfg, out, nodes);
    child = child.sibling ?? null;
  }
  return nodes;
}

/** Build a canonical Node for one fiber (if it is a host) and append it. */
function appendNode(
  fiber: NonNullable<Fiber>,
  cfg: ResolvedConfig,
  out: BuildOut,
  into: Node[],
): void {
  out.any = true;
  const props = fiber.memoizedProps ?? fiber.pendingProps ?? null;
  if (isHiddenProps(props)) return; // drop hidden node and its subtree

  if (!isHost(fiber)) {
    // Composite wrapper: transparent. Hoist host descendants into `into`.
    let child = fiber.child ?? null;
    while (child) {
      appendNode(child, cfg, out, into);
      child = child.sibling ?? null;
    }
    return;
  }

  const role = roleOfFiber(fiber, props);
  // Document-order index of this host node among same-canonical-role peers, for a
  // Layer-3 role:<role>#<idx> value-node selector. Incremented for every host.
  const myRoleIndex = out.roleSeen[role] || 0;
  out.roleSeen[role] = myRoleIndex + 1;

  // Value-state (Layer 2): a value-role node (by trait/host type, or an
  // accessibilityLiveRegion) or a Layer-3 opt-in node is value-bearing. Value-
  // bearing WINS over the transient heuristic, so a role=status / live-region
  // counter that the transient heuristic would otherwise drop is kept as a value
  // node instead, and its updates produce DISTINCT value-states.
  const vrole = valueRoleOfFiber(fiber, props);
  const optIn = matchesValueNodeFiber(props, role, () => myRoleIndex);
  const valueBearing = !!vrole || optIn;
  const transient = !valueBearing && isTransientFiber(props, role);
  const id = idOfFiber(props);
  const node: Node = { role };
  if (id != null) node.id = id;
  const type = typeOfFiber(props, role);
  if (type != null) node.type = type;
  const icon = iconOfFiber(props);
  if (icon != null) node.icon = icon;
  if (valueBearing) {
    node.value = valueOfFiber(props);
    // The flag makes the canonical is_value_bearing accept the node even when
    // roleOfFiber normalized its raw value-role (status/output/...) to node.
    node.value_node = true;
  }
  if (transient) {
    node.transient = true;
    into.push(node); // shared normalizer drops it (and its subtree)
    return;
  }

  // display-only labels + elements list (never in the hash)
  const name = nameOfProps(props);
  if (name && name.length <= cfg.maxLabelLen && !out.seenLabel.has(name)) {
    out.seenLabel.add(name);
    out.labels.push(name);
  }
  if (isTappable(props, role)) {
    const display = name && name.length <= cfg.maxLabelLen ? name : '';
    if (display) out.tappables.push(display);
    const idx = out.perRole[role] || 0;
    out.perRole[role] = idx + 1;
    const sel = id != null ? `key:${id}` : `role:${role}#${idx}`;
    out.elements.push({ sel, role, label: display, nokey: id == null });
  }

  node.children = buildChildren(fiber, cfg, out);
  into.push(node);
}

/**
 * Locate the root fiber for a mounted React Native app via the React DevTools
 * global hook (always registered by the RN renderer). Library-safe: avoids
 * private import paths that shift between RN versions.
 */
function findRoots(): NonNullable<Fiber>[] {
  const roots: NonNullable<Fiber>[] = [];
  const g = globalThis as unknown as {
    __REACT_DEVTOOLS_GLOBAL_HOOK__?: {
      getFiberRoots?: (rendererId: number) => Set<{ current?: Fiber }> | undefined;
      renderers?: Map<number, unknown>;
    };
  };
  const hook = g.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!hook || typeof hook.getFiberRoots !== 'function' || !hook.renderers) {
    return roots;
  }
  for (const rendererId of hook.renderers.keys()) {
    const set = hook.getFiberRoots(rendererId);
    if (!set) continue;
    for (const r of set) {
      const current = r && r.current;
      if (current) roots.push(current);
    }
  }
  return roots;
}

// ---- anchor (route) -------------------------------------------------------

/**
 * The current screen anchor (route name) set by the provider's navigation
 * listener (ReproIt.noteRoute). Stored on a module-global so snapshot() can read
 * it synchronously without a circular import on the singleton. Null when no
 * route is known.
 */
let currentAnchor: string | null = null;

/** @internal Set the current route anchor (called by the SDK on nav changes). */
export function setAnchor(route: string | null): void {
  currentAnchor = route && route.length ? route : null;
}

/** @internal The current route anchor. */
export function getAnchor(): string | null {
  return currentAnchor;
}

/**
 * Take a snapshot of the current mounted UI as a canonical structural signature.
 * Returns `any:false` when no fiber root is reachable (before first render, or
 * React internals unavailable), in which case the caller should skip emitting.
 */
export function snapshot(cfg: ResolvedConfig): Snapshot {
  const out: BuildOut = {
    labels: [],
    tappables: [],
    elements: [],
    seenLabel: new Set<string>(),
    perRole: {},
    roleSeen: {},
    any: false,
  };
  // The canonical root is a single `screen` node; every reachable fiber root's
  // host subtree hangs under it (parallels the runners forcing the root role to
  // "screen").
  const screen: Node = { role: 'screen', children: [] };
  for (const root of findRoots()) {
    const kids = buildChildren(root, cfg, out);
    for (const k of kids) (screen.children as Node[]).push(k);
  }
  const anchor = getAnchor();
  return {
    sig: signatureOf(anchor, screen),
    anchor,
    labels: out.labels.slice(0, cfg.maxLabels),
    tappables: [...new Set(out.tappables)],
    elements: out.elements,
    any: out.any,
  };
}

// ---- field fingerprinting (PII-safe, unchanged contract) ------------------

/**
 * True if this host fiber is a text-entry control. We fingerprint these on
 * error; we NEVER fingerprint secureTextEntry (password) fields.
 */
function isTextField(fiber: NonNullable<Fiber>, props: Record<string, unknown> | null): boolean {
  if (typeof fiber.type !== 'string') return false;
  const t = fiber.type;
  if (t !== 'RCTTextInput' && t !== 'RCTSinglelineTextInputView' && t !== 'RCTMultilineTextInputView' && t !== 'AndroidTextInput') {
    return false;
  }
  if (props && props.secureTextEntry === true) return false;
  return true;
}

/** A stable, value-independent label for a field (never derived from value). */
function fieldLabelOf(props: Record<string, unknown> | null, index: number): string {
  if (props) {
    for (const key of ['accessibilityLabel', 'aria-label', 'testID', 'nativeID', 'placeholder', 'name']) {
      const v = props[key];
      if (typeof v === 'string' && v.trim()) return firstLine(v);
    }
  }
  return `#${index}`;
}

/** The current text of a text-field fiber: prefer `text`/`value`, else default. */
function fieldValueOf(props: Record<string, unknown> | null): string {
  if (!props) return '';
  for (const key of ['text', 'value', 'defaultValue']) {
    const v = props[key];
    if (typeof v === 'string') return v;
    if (typeof v === 'number') return String(v);
  }
  return '';
}

/**
 * Walk the mounted fiber tree and collect on-screen text-field {field, value}
 * pairs (for PII-safe fingerprinting at error time). Password fields are skipped
 * and never read. Best-effort: returns [] if React internals are unreachable.
 */
export function collectFields(): FieldValue[] {
  const out: FieldValue[] = [];
  let index = 0;
  for (const root of findRoots()) {
    const stack: NonNullable<Fiber>[] = [root];
    while (stack.length) {
      const fiber = stack.pop()!;
      const props = fiber.memoizedProps ?? fiber.pendingProps ?? null;
      if (isHiddenProps(props)) continue;
      if (isTextField(fiber, props)) {
        out.push({ field: fieldLabelOf(props, index++), value: fieldValueOf(props) });
      }
      let child = fiber.child ?? null;
      while (child) {
        stack.push(child);
        child = child.sibling ?? null;
      }
    }
  }
  return out;
}

/**
 * Build a snapshot directly from a canonical Node tree (and optional anchor).
 * Documented escape hatch for screens the fiber walk can't see (e.g. content
 * rendered into a native module / WebView): the caller supplies the structural
 * tree itself. The signature is over the canonical descriptor, identical to the
 * fiber-walk path.
 */
export function snapshotFromTree(root: Node, anchor?: string | null): Snapshot {
  return {
    sig: signatureOf(anchor ?? null, root),
    anchor: anchor ?? null,
    labels: [],
    tappables: [],
    elements: [],
    any: true,
  };
}
