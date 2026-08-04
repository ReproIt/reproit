/*!
 * reproit-web, production telemetry SDK (v1.0.1)
 *
 * Drop this into a production web app and it emits the SAME marker protocol the
 * ReproIt runner uses, but driven by REAL users instead of the fuzzer. Each
 * screen a user lands on is hashed to a state signature; each navigation is an
 * edge. The result is a live usage graph that aligns 1:1 with your test app
 * map, because the signature function here is byte-identical to the runner's.
 *
 * Why it matters:
 *   • shows which states/paths real users actually hit -> tells you what to test
 *   • a production error ships with the exact graph PATH that led to it, so it
 *     becomes a deterministic repro test instead of a "cannot reproduce" ticket
 *   • new/changed screens (a deploy) show up as graph drift -> what to re-test
 *
 * Privacy by design: signatures are STRUCTURAL (a hash of which controls exist),
 * not user data. With redactLabels:true, only the hashes leave the browser.
 * On an error we also attach PII-safe input FINGERPRINTS under context.fingerprint:
 * derived FEATURES of on-screen text fields ({field,len,charset,hasEmoji,isEmpty,
 * isRtl}), never the raw values, so the cloud can build a property-matched replay
 * fixture without storing PII. Password/hidden fields are never read.
 *
 * Usage (script tag):
 *   <script src="reproit-web.js"></script>
 *   <script>
 *     ReproIt.init({
 *       appId: "app_...",
 *       endpoint: "https://ingest.reproit.com/v1/events",
 *       key: "pk_live_..."
 *     })
 *   </script>
 *
 * Or as a module:  import "./reproit-web.js"; ReproIt.init({...})
 *
 * Electron / Tauri:
 *   This IS the production SDK for Electron and Tauri apps too. Both render
 *   their UI in a webview (Electron = Chromium, Tauri = the system WebView),
 *   so the same DOM walk applies and the signature is byte-identical to what
 *   the reproit electron/tauri runners compute (parity-gated in
 *   runners/signature_test.mjs). Load it in the renderer/frontend exactly as
 *   above; no Electron/Tauri-specific build is needed.
 *     - Electron: include it in your renderer HTML, or import it from the
 *       renderer entry. Do NOT load it in the main process (no DOM there).
 *     - Tauri: import it from your frontend bundle like any web dependency.
 *   See sdk/reproit-web.README.md for the full embedding guide.
 */
(function (global) {
  'use strict';

  var DEFAULTS = {
    appId: 'app',
    endpoint: null, // POST target; if null, events go to opts.onEvent / console
    key: null, // write-only project key (pk_live_...); sent as `Authorization: Bearer`
    reportAutomation: false, // report webdriver-driven sessions (test rigs opt in)
    onEvent: null, // callback(event), dev hook / custom transport
    sampleRate: 1.0, // fraction of sessions that report (0..1)
    maxLabels: 24, // labels per state signature (matches the runner)
    maxLabelLen: 40,
    pathCap: 60, // how much of the graph trail to keep for repros
    flushMs: 5000, // batch flush interval
    redactLabels: false, // true => send only signatures, never control text
    debounceMs: 350, // settle window after an interaction before snapshotting
    valueNodes: [], // Layer-3 opt-in selectors marking EXTRA value-bearing nodes
    build: null, // developer-provided { version, commit }; stamped as context.build
    context: null, // optional bounded, PII-safe session context stamped on every event
    testerCaptureShortcut: false, // debug builds: Alt+Shift+B marks the current state
  };

  // Keep only the provided string fields of a developer-supplied build identity
  // ({version, commit}). Returns null when neither is a non-empty string, so no
  // build object is stamped into the batch context.
  function normalizeBuild(build) {
    if (!build || typeof build !== 'object') return null;
    var out = {};
    if (typeof build.version === 'string' && build.version.length) out.version = build.version;
    if (typeof build.commit === 'string' && build.commit.length) out.commit = build.commit;
    return out.version || out.commit ? out : null;
  }

  function environmentContext() {
    var nav = typeof navigator !== 'undefined' ? navigator : {};
    var win = typeof window !== 'undefined' ? window : {};
    var doc = typeof document !== 'undefined' ? document : {};
    var root = doc.documentElement || {};
    var ua = nav.userAgent || '';
    var browser = 'Other';
    var browserMajor = '';
    var match;
    if ((match = ua.match(/Edg\/(\d+)/))) {
      browser = 'Edge';
      browserMajor = match[1];
    } else if ((match = ua.match(/Firefox\/(\d+)/))) {
      browser = 'Firefox';
      browserMajor = match[1];
    } else if ((match = ua.match(/(?:Chrome|CriOS)\/(\d+)/))) {
      browser = 'Chrome';
      browserMajor = match[1];
    } else if ((match = ua.match(/Version\/(\d+).+Safari/))) {
      browser = 'Safari';
      browserMajor = match[1];
    }

    var os = 'Other';
    if (/Windows NT/.test(ua)) os = 'Windows';
    else if (/Android/.test(ua)) os = 'Android';
    else if (/iPhone|iPad|iPod/.test(ua)) os = 'iOS';
    else if (/Mac OS X/.test(ua)) os = 'macOS';
    else if (/Linux/.test(ua)) os = 'Linux';

    var width = Math.round(win.innerWidth || root.clientWidth || 0);
    var height = Math.round(win.innerHeight || root.clientHeight || 0);
    var device = /Mobi|Android|iPhone|iPod/.test(ua)
      ? 'mobile'
      : /iPad/.test(ua) || (width > 0 && width < 1024)
        ? 'tablet'
        : 'desktop';
    return {
      platform: 'web',
      browser: browser,
      browserMajor: browserMajor || undefined,
      os: os,
      device: device,
      locale: nav.language || undefined,
      viewport: {
        width: width,
        height: height,
        dpr: Number((win.devicePixelRatio || 1).toFixed(2)),
      },
    };
  }

  // Layer-3 opt-in (docs/signature.md "Value-state"): a list of selectors that
  // mark EXTRA DOM nodes as value-bearing even when their role is not a value-
  // role. Selectors use the same grammar as `value_nodes:` in reproit.yaml:
  //   key:<id>          -> data-testid / id / name == <id>
  //   role:<role>#<idx> -> the idx-th node of that canonical role (document order)
  //   <css>             -> any other string is treated as a raw CSS selector
  // The matcher is module-level so domToNode (called from snapshot) can consult
  // it without threading config through every recursion. setValueNodeSelectors
  // installs the active list; matchesValueNode tests a single element against it.
  var VALUE_NODE_SELECTORS = [];
  function setValueNodeSelectors(list) {
    VALUE_NODE_SELECTORS = Array.isArray(list) ? list.slice() : [];
  }
  function matchesValueNode(el) {
    if (!VALUE_NODE_SELECTORS.length) return false;
    for (var i = 0; i < VALUE_NODE_SELECTORS.length; i++) {
      if (elMatchesSelector(el, VALUE_NODE_SELECTORS[i])) return true;
    }
    return false;
  }
  // Test one element against one value-node selector (key:/role:/raw CSS).
  function elMatchesSelector(el, sel) {
    if (!sel || typeof sel !== 'string') return false;
    if (sel.indexOf('key:') === 0) {
      var id = sel.slice(4);
      if (!id) return false;
      var got =
        el.getAttribute('data-testid') ||
        el.getAttribute('data-test-id') ||
        el.getAttribute('id') ||
        el.getAttribute('name') ||
        '';
      return got.trim() === id;
    }
    if (sel.indexOf('role:') === 0) {
      var hash = sel.indexOf('#');
      if (hash < 0) return false;
      var role = sel.slice(5, hash);
      var idx = parseInt(sel.slice(hash + 1), 10);
      if (!(idx >= 0)) return false;
      // Resolve the idx-th element of this canonical role in document order.
      var root = document.body || document.documentElement;
      if (!root) return false;
      var seen = -1,
        target = null;
      (function walk(node) {
        if (target) return;
        if (roleOf(node) === role) {
          seen++;
          if (seen === idx) {
            target = node;
            return;
          }
        }
        var kids = node.children || [];
        for (var k = 0; k < kids.length; k++) walk(kids[k]);
      })(root);
      return target === el;
    }
    // raw CSS selector
    try {
      return el.matches && el.matches(sel);
    } catch (e) {
      return false;
    }
  }

  // ====================================================================
  //  CANONICAL STRUCTURAL SIGNATURE
  //  Byte-identical to the Rust oracle (crates/reproit/src/model/signature.rs)
  //  and to runners/web/runner.mjs. Spec: docs/signature.md. Proven against
  //  signature_vectors.json by sdk/test/signature_test.js.
  //
  //  A signature hashes STRUCTURE (roles + ids + types + icons + tree shape),
  //  never localized text, so an EN and a DE render of the same screen hash
  //  identically. The descriptor is:
  //      "A:" + anchor + "\n" + tokens.join(";")
  //  where each retained node emits one pre-order token:
  //      <depth>:<role>[:<type>][#<icon>][@<id>]   (plus "*" if collapsed)
  //  hashed with FNV-1a 32-bit -> 8 hex chars.
  // ====================================================================

  // Fixed, language-independent role vocabulary. Anything else -> "node".
  var ROLES = {
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
  // Roles that flicker in/out and are dropped before hashing (rule 2).
  // "progress" is the role name for spinner/progress.
  var TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };

  // Value-role set (docs/signature.md "Value-state", Layer 2). A node is
  // value-bearing iff it has a `value` AND either its RAW role is one of these OR
  // it carries the opt-in `value_node` flag (Layer 3). Several of these roles
  // (status, log, progressbar, meter, timer, output) are NOT in the structural
  // ROLES vocabulary, so they normalize to "node" in the token body; the
  // value-role test deliberately uses the RAW role, not the normalized one.
  // Chrome roles (button/label/header/text/link) are NEVER value-bearing, so the
  // chrome-text exclusion (rule 1) is preserved exactly.
  var VALUE_ROLES = {
    textfield: 1,
    status: 1,
    log: 1,
    progressbar: 1,
    meter: 1,
    timer: 1,
    output: 1,
  };

  function normalizeRole(role) {
    return ROLES[role] ? role : 'node';
  }
  function isTransientNode(node) {
    return !!node.transient || !!TRANSIENT_ROLES[node.role];
  }
  // True if this node carries a canonical value-class in the V: section: it has a
  // `value` AND it is value-bearing (raw role is a value-role, or value_node-
  // flagged). Mirrors the oracle's is_value_bearing exactly.
  function isValueBearing(node) {
    return node.value != null && (!!VALUE_ROLES[node.role] || !!node.value_node);
  }

  // The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
  // descriptor and V: keys can carry non-ASCII (a localized route in the anchor,
  // a non-ASCII developer id, an emoji icon), so we MUST fold the UTF-8 BYTES of
  // the string, exactly like the Rust oracle's `desc.as_bytes()`. Hashing the
  // UTF-16 code units instead silently diverged on any non-ASCII descriptor.
  var REPROIT_UTF8 = new TextEncoder();

  // FNV-1a 32-bit over the UTF-8 BYTES of the descriptor -> 8 hex. Byte-for-byte
  // identical to the Rust oracle's fnv1a32_hex (offset basis 0x811c9dc5, prime
  // 0x01000193) over `descriptor.as_bytes()`.
  function fnv1a32hex(s) {
    var bytes = REPROIT_UTF8.encode(s);
    var h = 0x811c9dc5;
    for (var i = 0; i < bytes.length; i++) {
      h ^= bytes[i];
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return ('0000000' + (h >>> 0).toString(16)).slice(-8);
  }

  // Lexicographic comparison of two strings by their UTF-8 byte sequence, to
  // match Rust's `String::cmp` (which compares bytes). JS `<` compares UTF-16
  // code units, which diverges from byte order for astral vs high-BMP keys, so
  // the V: section MUST sort with this instead.
  function reproitCmpUtf8(a, b) {
    var ab = REPROIT_UTF8.encode(a);
    var bb = REPROIT_UTF8.encode(b);
    var n = ab.length < bb.length ? ab.length : bb.length;
    for (var i = 0; i < n; i++) {
      if (ab[i] !== bb[i]) return ab[i] < bb[i] ? -1 : 1;
    }
    return ab.length === bb.length ? 0 : ab.length < bb.length ? -1 : 1;
  }

  // Rules 1, 2, 4: exclude text (no text field exists), drop transient
  // subtrees, keep document order. Returns null if the node itself is transient.
  function normalizeNode(node) {
    if (isTransientNode(node)) return null;
    var kids = [];
    var children = node.children || [];
    for (var i = 0; i < children.length; i++) {
      var n = normalizeNode(children[i]);
      if (n) kids.push(n);
    }
    return {
      role: normalizeRole(node.role),
      type: node.type != null ? node.type : null,
      icon: node.icon != null ? node.icon : null,
      id: node.id != null ? node.id : null,
      children: kids,
    };
  }

  // Token body after "<depth>:", without the repeat marker:
  //   <role>[:<type>][#<icon>][@<id>]
  function tokenBody(n) {
    var s = n.role;
    if (n.type != null) s += ':' + n.type;
    if (n.icon != null) s += '#' + n.icon;
    if (n.id != null) s += '@' + n.id;
    return s;
  }

  // Subtree key for collapse comparison (rule 3): pre-order token list with
  // depth re-based to 0 so sibling subtrees compare regardless of absolute depth.
  function subtreeKey(n) {
    var tokens = [];
    (function walk(node, depth) {
      tokens.push(depth + ':' + tokenBody(node));
      for (var i = 0; i < node.children.length; i++) walk(node.children[i], depth + 1);
    })(n, 0);
    return tokens.join(';');
  }

  function serializeNode(n, depth, repeated, tokens) {
    var tok = depth + ':' + tokenBody(n);
    if (repeated) tok += '*';
    tokens.push(tok);
    serializeChildren(n.children, depth + 1, tokens);
  }
  // Collapse maximal runs of >= 2 consecutive identical sibling subtrees into a
  // single "*"-marked emission (count dropped).
  function serializeChildren(children, depth, tokens) {
    var i = 0;
    while (i < children.length) {
      var key = subtreeKey(children[i]);
      var j = i + 1;
      while (j < children.length && subtreeKey(children[j]) === key) j++;
      serializeNode(children[i], depth, j - i >= 2, tokens);
      i = j;
    }
  }

  // ---- Layer 2: value-class identity (canonical) --------------------------
  // Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, one or more ASCII digits,
  // optionally a period and one or more ASCII digits. No grouping separators, no
  // exponent, no leading/trailing dot. Locale-safe by construction. Mirrors the
  // oracle's is_strict_decimal byte-for-byte.
  function isStrictDecimal(s) {
    var i = 0;
    var n = s.length;
    if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) i++; // + or -
    var intStart = i;
    while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
    if (i === intStart) return false; // need at least one integer digit
    if (i < n && s.charCodeAt(i) === 46) {
      // '.'
      i++;
      var fracStart = i;
      while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
      if (i === fracStart) return false; // trailing dot with no fraction
    }
    return i === n;
  }

  // Map a value string to a bounded, deterministic, locale-safe value-class
  // token (docs/signature.md "Value-state"). Identical rule to the Rust oracle's
  // value_class: EMPTY / strict-decimal -> ZERO|NEG|POS1|POS2|POS3|POSL / else
  // NONEMPTY. Anything ambiguously formatted (grouped/locale numbers, currency,
  // exponent, non-ASCII digits) falls to NONEMPTY because we do not guess locale.
  function valueClass(s) {
    var t = (s == null ? '' : String(s)).replace(/^\s+|\s+$/g, '');
    if (t.length === 0) return 'EMPTY';
    if (isStrictDecimal(t)) {
      var num = parseFloat(t);
      var a = Math.abs(num);
      if (num === 0) return 'ZERO';
      if (num < 0) return 'NEG';
      if (a < 10) return 'POS1';
      if (a < 100) return 'POS2';
      if (a < 1000) return 'POS3';
      return 'POSL';
    }
    return 'NONEMPTY';
  }

  // The V:-section key for a value-bearing node: its stable `id` rendered as
  // key:<id> if present, else the structural fallback role:<role>#<idx> using the
  // NORMALIZED role and the per-parent structural index among same-role
  // non-transient siblings (matching the selector grammar). Mirrors value_key.
  function valueKeyOf(node, structuralIndex) {
    if (node.id != null) return 'key:' + node.id;
    return 'role:' + normalizeRole(node.role) + '#' + structuralIndex;
  }

  // Collect (value_key, value_class) pairs for every value-bearing node in the
  // tree, pre-order, skipping transient subtrees (rule 2) so the V: section is
  // consistent with the structural body. The root gets index 0 (no peers); each
  // keyless child gets its position among same-normalized-role non-transient
  // siblings under the same parent. Mirrors collect_values + collect_values_children.
  function collectValues(node, out) {
    if (isTransientNode(node)) return;
    if (isValueBearing(node)) out.push([valueKeyOf(node, 0), valueClass(node.value)]);
    collectValuesChildren(node, out);
  }
  function collectValuesChildren(node, out) {
    var roleCounts = {};
    var children = node.children || [];
    for (var i = 0; i < children.length; i++) {
      var child = children[i];
      if (isTransientNode(child)) continue;
      var role = normalizeRole(child.role);
      var idx = roleCounts[role] || 0;
      roleCounts[role] = idx + 1;
      if (isValueBearing(child)) out.push([valueKeyOf(child, idx), valueClass(child.value)]);
      collectValuesChildren(child, out);
    }
  }

  // Build the V: section suffix. Returns "" when there are NO value-bearing
  // nodes, which keeps the descriptor (and hash) byte-identical to a pre-value-
  // state tree. Otherwise returns "\nV:" + sorted key=class entries joined by ";".
  function valueSection(root) {
    var pairs = [];
    collectValues(root, pairs);
    if (pairs.length === 0) return '';
    pairs.sort(function (a, b) {
      return reproitCmpUtf8(a[0], b[0]);
    });
    var body = pairs
      .map(function (p) {
        return p[0] + '=' + p[1];
      })
      .join(';');
    return '\nV:' + body;
  }

  // The exact UTF-8 descriptor string that gets hashed. The V: section (Layer 2)
  // is appended only when at least one value-bearing node exists; otherwise it is
  // "" and the descriptor is byte-identical to a pre-value-state tree.
  function descriptorOf(anchor, root) {
    var tokens = [];
    var norm = normalizeNode(root);
    if (norm) serializeNode(norm, 0, false, tokens);
    return 'A:' + (anchor == null ? '' : anchor) + '\n' + tokens.join(';') + valueSection(root);
  }

  // Canonical structural signature: FNV-1a over the descriptor.
  function signatureOf(anchor, root) {
    return fnv1a32hex(descriptorOf(anchor, root));
  }

  // ---- DOM -> canonical Node tree -----------------------------------------
  // Map a live DOM element to a canonical role. Derived from tag + aria role +
  // input type, NEVER from visible text. Most-specific first.
