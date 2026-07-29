  function roleOf(el) {
    var tag = el.tagName.toLowerCase();
    var ariaRole = (el.getAttribute('role') || '').toLowerCase();
    // explicit aria role wins when it is in (or maps into) the vocabulary
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
      var t = (el.getAttribute('type') || 'text').toLowerCase();
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'range') return 'slider';
      if (['button', 'submit', 'reset', 'image'].indexOf(t) >= 0) return 'button';
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
    if (ariaRole) return 'node'; // an aria role outside vocabulary
    return 'node';
  }

  // Optional input type refinement, only for textfield-ish controls.
  function typeOf(el, role) {
    if (role !== 'textfield') return null;
    var tag = el.tagName.toLowerCase();
    if (tag !== 'input') return null;
    var t = (el.getAttribute('type') || 'text').toLowerCase();
    var allowed = { text: 1, password: 1, email: 1, number: 1, search: 1 };
    return allowed[t] ? t : 'text';
  }

  // Language-independent icon identity: an icon-font codepoint or an svg <use>
  // href / data-icon asset name. Never localized text.
  function iconOf(el) {
    var di = el.getAttribute && (el.getAttribute('data-icon') || el.getAttribute('data-icon-name'));
    if (di && di.trim()) return di.trim();
    // svg <use xlink:href="#icon-x"> / <use href="#icon-x">
    var use = el.querySelector ? el.querySelector('use[href], use[xlink\\:href]') : null;
    if (use) {
      var href =
        use.getAttribute('href') ||
        use.getAttributeNS('http://www.w3.org/1999/xlink', 'href') ||
        use.getAttribute('xlink:href');
      if (href && href.trim()) return href.trim().replace(/^#/, '');
    }
    // icon-font convention: <i class="material-icons">codepoint/name</i> with a
    // data attribute, or a ligature. We only read a stable data-attr, not text.
    return null;
  }

  // Stable developer identifier: data-testid > id > name. Omitted if none.
  function idOf(el) {
    var testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return testid.trim();
    var id = el.getAttribute('id');
    if (id && id.trim()) return id.trim();
    var name = el.getAttribute('name');
    if (name && name.trim()) return name.trim();
    return null;
  }

  // Runner-native replay selector for an element. This is deliberately more
  // specific than `idOf`: web replay resolves key:testid:/key:id:/key:name:
  // selectors differently, so preserve the source kind.
  function actionKeyOf(el) {
    var testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return 'key:testid:' + testid.trim();
    var id = el.getAttribute('id');
    if (id && id.trim()) return 'key:id:' + id.trim();
    var name = el.getAttribute('name');
    if (name && name.trim()) return 'key:name:' + name.trim();
    return null;
  }

  // Heuristic: is this element a transient node (toast/snackbar/spinner/
  // progress/tooltip/badge) by role, aria-live, or class name? Dropped from hash.
  function isTransientEl(el) {
    var ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (TRANSIENT_ROLES[ariaRole]) return true;
    if (ariaRole === 'alert' || ariaRole === 'status') return true;
    var live = (el.getAttribute('aria-live') || '').toLowerCase();
    if (live === 'assertive' || live === 'polite') return true;
    var cls = (el.getAttribute('class') || '').toLowerCase();
    if (/\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test(cls)) return true;
    if (el.hasAttribute && el.hasAttribute('data-transient')) return true;
    return false;
  }

  // The RAW value-role of a DOM element for the Layer-2 value-class, derived from
  // tag + aria role, NEVER from text. This is intentionally distinct from roleOf:
  // it returns one of the value-role names (status/log/progressbar/meter/timer/
  // output) for the matching ARIA roles, and "textfield" for form fields, so the
  // canonical is_value_bearing test sees the RAW role the oracle expects. An
  // aria-live region (polite/assertive) maps to "status" (a value-role) so a live
  // region becomes value-bearing WITHOUT any opt-in. Returns null for chrome.
  function valueRoleOf(el) {
    var tag = el.tagName.toLowerCase();
    var ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (
      ariaRole === 'status' ||
      ariaRole === 'log' ||
      ariaRole === 'progressbar' ||
      ariaRole === 'meter' ||
      ariaRole === 'timer'
    ) {
      return ariaRole;
    }
    if (tag === 'output' || ariaRole === 'output') return 'output';
    // aria-live region (polite/assertive) -> a value-role status node.
    var live = (el.getAttribute('aria-live') || '').toLowerCase();
    if (live === 'polite' || live === 'assertive') return 'status';
    // form fields hold a .value: they are textfield value-roles.
    if (tag === 'input') {
      var t = (el.getAttribute('type') || 'text').toLowerCase();
      // Non-text inputs (checkbox/radio/range/buttons) are not text value fields.
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
        ].indexOf(t) >= 0
      )
        return null;
      return 'textfield';
    }
    if (tag === 'textarea' || tag === 'select') return 'textfield';
    if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
      return 'textfield';
    return null;
  }

  // The displayed data value of a value-role element: the field's `.value` for
  // form controls, else the trimmed textContent for output/status/live nodes.
  // Password fields are never read (valueRoleOf already excludes them).
  function valueOf(el) {
    var tag = el.tagName.toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select') {
      return el.value != null ? String(el.value) : '';
    }
    return (el.textContent != null ? el.textContent : '').trim();
  }

  // Build the canonical Node tree from a DOM root. Invisible elements are
  // skipped but their children are hoisted (matches structure regardless of
  // wrapper visibility). Transient subtrees carry transient:true so the shared
  // normalizer drops them. The root node's role is forced to "screen".
  //
  // Value-state (Layer 2): a value-bearing element (a value-role by tag/aria, OR
  // an opt-in value_node) gets `value` + `value_node` set on its Node so the
  // canonical descriptor folds its value-class into the V: section. Value-bearing
  // WINS over the transient heuristic: a role=status / aria-live node that the
  // transient heuristic would otherwise drop is kept as a value node instead, so
  // a counter/stopwatch live region produces distinct value-states.
  function domToNode(root, isRoot) {
    var role = isRoot ? 'screen' : roleOf(root);
    var vrole = isRoot ? null : valueRoleOf(root);
    var optIn = !isRoot && typeof matchesValueNode === 'function' && matchesValueNode(root);
    var valueBearing = !isRoot && (!!vrole || optIn);
    var transient = !isRoot && !valueBearing && isTransientEl(root);
    var node = {
      role: role,
      id: idOf(root) || undefined,
      type: typeOf(root, role) || undefined,
      icon: iconOf(root) || undefined,
      transient: transient,
      children: [],
    };
    if (valueBearing) {
      node.value = valueOf(root);
      // An opt-in node whose role is NOT a value-role needs the flag so the
      // canonical is_value_bearing accepts it; a value-role node is accepted by
      // role alone but the flag is harmless and keeps the two paths uniform.
      node.value_node = true;
    }
    if (transient) return node; // subtree dropped anyway; do not recurse
    var kids = root.children || [];
    for (var i = 0; i < kids.length; i++) {
      var el = kids[i];
      if (!visible(el)) {
        // hoist visible descendants of an invisible wrapper
        collectVisibleInto(el, node.children);
        continue;
      }
      node.children.push(domToNode(el, false));
    }
    return node;
  }
  function collectVisibleInto(el, out) {
    var kids = el.children || [];
    for (var i = 0; i < kids.length; i++) {
      var c = kids[i];
      if (!visible(c)) {
        collectVisibleInto(c, out);
        continue;
      }
      out.push(domToNode(c, false));
    }
  }

  // The screen anchor: path + SPA hash route, query EXCLUDED -- byte-identical to
  // the runner (runners/web/runner.mjs). Hash routers put the real route in
  // location.hash (#/a vs #/b on one pathname), so it MUST be in the anchor or the
  // SDK collapses distinct screens that the runner keeps separate, breaking the
  // "byte-identical to the runner's signature" contract on every hash-router SPA.
  function anchorOf() {
    try {
      if (typeof location !== 'undefined' && location.pathname) {
        var hash = location.hash || '';
        var q = hash.indexOf('?');
        if (q >= 0) hash = hash.slice(0, q);
        return location.pathname + hash;
      }
    } catch (e) {}
    return null;
  }

  function interactive(el) {
    var tag = el.tagName.toLowerCase();
    var role = el.getAttribute('role') || '';
    if (['a', 'button', 'input', 'select', 'textarea'].indexOf(tag) >= 0) return true;
    if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch'].indexOf(role) >= 0) return true;
    return el.hasAttribute('onclick') || el.tabIndex >= 0;
  }

  function actionSelectorOf(el) {
    var key = actionKeyOf(el);
    if (key) return key;
    var role = roleOf(el);
    var root = document.body || document.documentElement;
    if (!root) return null;
    var idx = -1;
    var found = false;
    (function walk(node) {
      if (found) return;
      if (node !== root && visible(node) && interactive(node) && roleOf(node) === role) {
        idx++;
        if (node === el) {
          found = true;
          return;
        }
      }
      var kids = node.children || [];
      for (var k = 0; k < kids.length; k++) walk(kids[k]);
    })(root);
    return found ? 'role:' + role + '#' + idx : null;
  }

  function nameOf(el) {
    var a =
      el.getAttribute &&
      (el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('alt'));
    if (a && a.trim()) return a.trim();
    var t = (el.innerText || el.textContent || '').trim().split('\n')[0].trim();
    return t;
  }

  function visible(el) {
    var r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    var st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  }

  // ---- PII-safe input fingerprinting (tier-3 context) ---------------------
  // On an error we capture DERIVED FEATURES of on-screen text-field values, never
  // the values themselves, so the cloud can property-match a replay fixture (a
  // 312-char name, an emoji, a Turkish "i", an empty/RTL field) WITHOUT storing
  // PII. fingerprintValue is the load-bearing pure function: identical shape and
  // rules across all five SDKs and host-unit-tested in each.

  // RTL detection: any char in the Hebrew/Arabic/Syriac/Thaana/N'Ko + Arabic
  // presentation-form ranges marks the string as right-to-left.
  function reproitIsRtl(str) {
    for (var i = 0; i < str.length; i++) {
      var c = str.charCodeAt(i);
      if (
        (c >= 0x0590 && c <= 0x05ff) || // Hebrew
        (c >= 0x0600 && c <= 0x06ff) || // Arabic
        (c >= 0x0700 && c <= 0x074f) || // Syriac
        (c >= 0x0780 && c <= 0x07bf) || // Thaana
        (c >= 0x07c0 && c <= 0x07ff) || // N'Ko
        (c >= 0x08a0 && c <= 0x08ff) || // Arabic Extended-A
        (c >= 0xfb1d && c <= 0xfb4f) || // Hebrew presentation forms
        (c >= 0xfb50 && c <= 0xfdff) || // Arabic presentation forms-A
        (c >= 0xfe70 && c <= 0xfeff) // Arabic presentation forms-B
      ) {
        return true;
      }
    }
    return false;
  }

  // Emoji detection: scan code points for the common emoji/pictographic blocks
  // and regional indicators (flags). Code-point aware so surrogate pairs count.
  function reproitHasEmoji(str) {
    for (var i = 0; i < str.length; i++) {
      var c = str.codePointAt(i);
      if (c > 0xffff) i++; // skip the low surrogate of an astral code point
      if (
        (c >= 0x1f000 && c <= 0x1faff) || // pictographs, emoji, symbols, etc.
        (c >= 0x1f1e6 && c <= 0x1f1ff) || // regional indicators (flags)
        (c >= 0x2600 && c <= 0x27bf) || // misc symbols + dingbats
        c === 0x2764 || // heavy black heart
        c === 0xfe0f || // variation selector-16 (emoji style)
        c === 0x200d || // zero-width joiner (emoji sequences)
        (c >= 0x2190 && c <= 0x21ff && false) // (arrows: not emoji) placeholder
      ) {
        return true;
      }
    }
    return false;
  }

  // Fingerprint schema version for the byte/script/combining/zero-width/
  // newline/edge-whitespace features below.
  var FP_VERSION = 2;

  // UTF-8 byte length, computed per code point so it's identical across SDKs
  // regardless of the host's native string encoding. Catches the byte-limit
  // (DB varchar) overflow class that code-point `len` alone misses.
  function reproitByteLen(str) {
    var bytes = 0;
    for (var i = 0; i < str.length; i++) {
      var c = str.codePointAt(i);
      if (c > 0xffff) i++; // astral: skip the low surrogate
      if (c < 0x80) bytes += 1;
      else if (c < 0x800) bytes += 2;
      else if (c < 0x10000) bytes += 3;
      else bytes += 4;
    }
    return bytes;
  }

  // Zero-width / invisible code points (injection + normalization breakers).
  function reproitHasZeroWidth(str) {
    for (var i = 0; i < str.length; i++) {
      var c = str.charCodeAt(i);
      if (c === 0x200b || c === 0x200c || c === 0x200d || c === 0x2060 || c === 0xfeff) {
        return true;
      }
    }
    return false;
  }

  // Combining marks (a base char + combining accent renders differently than a
  // precomposed one; a classic normalization/layout breaker).
  function reproitHasCombining(str) {
    for (var i = 0; i < str.length; i++) {
      var c = str.charCodeAt(i);
      if (
        (c >= 0x0300 && c <= 0x036f) ||
        (c >= 0x1ab0 && c <= 0x1aff) ||
        (c >= 0x1dc0 && c <= 0x1dff) ||
        (c >= 0x20d0 && c <= 0x20ff) ||
        (c >= 0xfe20 && c <= 0xfe2f)
      ) {
        return true;
      }
    }
    return false;
  }

  function reproitIsCombiningCp(c) {
    return (
      (c >= 0x0300 && c <= 0x036f) ||
      (c >= 0x1ab0 && c <= 0x1aff) ||
      (c >= 0x1dc0 && c <= 0x1dff) ||
      (c >= 0x20d0 && c <= 0x20ff) ||
      (c >= 0xfe20 && c <= 0xfe2f)
    );
  }

  function reproitGraphemeCount(str) {
    var n = 0;
    var joined = false;
    for (var _i = 0, chars = Array.from(str); _i < chars.length; _i++) {
      var c = chars[_i].codePointAt(0);
      if (c === 0x200d) {
        joined = true;
        continue;
      }
      if (reproitIsCombiningCp(c) || (c >= 0xfe00 && c <= 0xfe0f)) continue;
      if (joined) {
        joined = false;
        continue;
      }
      n += 1;
    }
    return n;
  }

  // The Unicode SCRIPTS present, as a sorted unique list of coarse bucket names.
  // Mixed-script (e.g. ["Arabic","Latin"]) is what bidi bugs need, which `isRtl`
  // alone can't express. Ranges are fixed and shared verbatim across all SDKs.
  function reproitScripts(str) {
    var found = {};
    for (var i = 0; i < str.length; i++) {
      var c = str.charCodeAt(i);
      if (
        (c >= 0x41 && c <= 0x5a) ||
        (c >= 0x61 && c <= 0x7a) ||
        (c >= 0xc0 && c <= 0x24f) ||
        (c >= 0x1e00 && c <= 0x1eff)
      )
        found['Latin'] = 1;
      else if (c >= 0x370 && c <= 0x3ff) found['Greek'] = 1;
      else if (c >= 0x400 && c <= 0x4ff) found['Cyrillic'] = 1;
      else if (c >= 0x590 && c <= 0x5ff) found['Hebrew'] = 1;
      else if (
        (c >= 0x600 && c <= 0x6ff) ||
        (c >= 0x750 && c <= 0x77f) ||
        (c >= 0x8a0 && c <= 0x8ff)
      )
        found['Arabic'] = 1;
      else if (c >= 0x900 && c <= 0x97f) found['Devanagari'] = 1;
      else if (c >= 0xe00 && c <= 0xe7f) found['Thai'] = 1;
      else if (
        (c >= 0x3040 && c <= 0x30ff) ||
        (c >= 0x3400 && c <= 0x9fff) ||
        (c >= 0xac00 && c <= 0xd7a3) ||
        (c >= 0xf900 && c <= 0xfaff)
      )
        found['CJK'] = 1;
    }
    return Object.keys(found).sort();
  }

  // Pure fingerprint of a single value. Captures FEATURES, never the value.
  //   len          : Unicode code-point count (so "José🎉" -> 5)
  //   bytes        : UTF-8 byte length
  //   graphemes    : approximate user-visible cluster count
  //   charset      : "numeric" (all ASCII digits) | "ascii" | "unicode"
  //   scripts      : sorted Unicode script buckets present (mixed-script bidi)
  //   hasEmoji     : contains an emoji/pictographic code point
  //   isEmpty      : empty or whitespace-only
  //   isRtl        : contains a right-to-left script char
  //   hasCombiningMarks / hasZeroWidth / hasNewline / leadingTrailingWhitespace
  function fingerprintValue(str) {
    var s = str == null ? '' : String(str);
    // Code-point length (Array.from splits on code points, not UTF-16 units).
    var len = Array.from(s).length;
    var trimmed = s.replace(/^\s+|\s+$/g, '');
    var isEmpty = trimmed.length === 0;
    var hasUnicode = false;
    var allDigits = !isEmpty;
    var hasNewline = false;
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c > 0x7f) hasUnicode = true;
      if (c < 0x30 || c > 0x39) allDigits = false;
      if (c === 0x0a || c === 0x0d) hasNewline = true;
    }
    var charset = hasUnicode ? 'unicode' : allDigits ? 'numeric' : 'ascii';
    // Edge whitespace: a fixed whitespace set (parity-safe, not locale trim).
    function isWs(cc) {
      return (
        cc === 0x09 ||
        cc === 0x0a ||
        cc === 0x0b ||
        cc === 0x0c ||
        cc === 0x0d ||
        cc === 0x20 ||
        cc === 0xa0
      );
    }
    var edgeWs = s.length > 0 && (isWs(s.charCodeAt(0)) || isWs(s.charCodeAt(s.length - 1)));
    return {
      len: len,
      bytes: reproitByteLen(s),
      graphemes: reproitGraphemeCount(s),
      charset: charset,
      scripts: reproitScripts(s),
      hasEmoji: reproitHasEmoji(s),
      isEmpty: isEmpty,
      isRtl: reproitIsRtl(s),
      hasCombiningMarks: reproitHasCombining(s),
      hasZeroWidth: reproitHasZeroWidth(s),
      hasNewline: hasNewline,
      leadingTrailingWhitespace: edgeWs,
    };
  }

  // A stable label for a field: prefer an explicit name/aria-label/id, else the
  // associated <label> text, else fall back to a positional index. Never derived
  // from the field's VALUE.
  function fieldLabel(el, index) {
    var lbl =
      (el.getAttribute &&
        (el.getAttribute('aria-label') ||
          el.getAttribute('name') ||
          el.getAttribute('id') ||
          el.getAttribute('placeholder'))) ||
      '';
    lbl = lbl && lbl.trim();
    if (!lbl && el.labels && el.labels.length) {
      lbl = (el.labels[0].innerText || el.labels[0].textContent || '').trim();
    }
    return lbl || '#' + index;
  }

  // Walk visible text inputs/fields, fingerprinting each VALUE then discarding
  // it. Returns an array of {field, len, charset, hasEmoji, isEmpty, isRtl}.
  function collectFieldFingerprints() {
    var out = [];
    if (typeof document === 'undefined') return out;
    var nodes = document.querySelectorAll(
      "input, textarea, [contenteditable='true'], [contenteditable='']",
    );
    var skipTypes = { password: 1, hidden: 1, file: 1, submit: 1, button: 1, image: 1, reset: 1 };
    for (var i = 0; i < nodes.length; i++) {
      var el = nodes[i];
      var tag = el.tagName.toLowerCase();
      if (tag === 'input') {
        var type = (el.getAttribute('type') || 'text').toLowerCase();
        // Never even READ password fields; skip non-text controls.
        if (skipTypes[type]) continue;
      }
      if (!visible(el)) continue;
      var value =
        tag === 'input' || tag === 'textarea' ? el.value : el.innerText || el.textContent || '';
      var fp = fingerprintValue(value);
      // Explicitly drop the raw value before it can leave this function.
      value = null;
      out.push({
        field: fieldLabel(el, i),
        len: fp.len,
        charset: fp.charset,
        hasEmoji: fp.hasEmoji,
        isEmpty: fp.isEmpty,
        isRtl: fp.isRtl,
      });
    }
    return out;
  }

  // Snapshot the live DOM into the CANONICAL structural signature. The sig is a
  // hash of the canonical Node tree (role + id + type + icon + shape), anchored
  // on the route, byte-identical to the runner and the Rust oracle. Localized
  // text never enters the hash; it is kept only as display-only `labels`.
