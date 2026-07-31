
export {
  signatureOf,
  descriptorOf,
  valueClass,
  snapshot,
  gtCollect,
  gtTabOrder,
  detectContentBugs,
  typeInto,
  loadInputs,
  inputValueFor,
  classifyFrameIntervals,
  drawFindingBoxes,
  tap,
  settleForSignature,
  isCoverageWalkConfig,
  detectBotWall,
};

// Snapshot the DOM: a STRUCTURAL, locale-invariant signature plus display-only
// labels and the structural selectors for each tappable. Mirrors
// Flutter explorer scaffold: the signature is a hash of the tag/role tree shape +
// stable developer identifiers (data-testid, name, aria role, input type) +
// structural position, with ALL user-facing text excluded. Visible text is kept
// only as a display label for `map show`, never folded into the hash or into a
// selector. Elements are addressed by stable selector preference
// (data-testid > name > aria-role + structural index); a tappable lacking
// any explicit author key falls back to role+index and is flagged `nokey`.
// A raw DOM `id` is an implementation-local reference, not a stability contract:
// frameworks and applications routinely allocate it per render or per process.
// A single snapshot cannot distinguish an allocator id from a human-readable but
// still generated id without site/library heuristics. Canonical identity therefore
// uses explicit author contracts (`data-testid` / `data-test-id` / `name`) and
// otherwise falls back to role + structural position. Raw ids remain available to
// authored CSS and ARIA in the page, but never enter a state hash or saved replay.

async function snapshot(page, valueNodeSelectors) {
  const snap = await page.evaluate(
    async ({ maxLen, valueNodeSelectors }) => {
      const labels = []; // DISPLAY-ONLY visible text
      const rawTaps = []; // tappable nodes in document order
      const extraTaps = []; // keyed pointer-operable nodes interactive() drops
      // Positional selectors live in a viewport-independent index space. The
      // production SDK records role indexes across every style-visible control,
      // including controls reached after scrolling. Keep that same index here
      // while still offering only currently reachable controls to the fuzzer.
      const visiblePerRole = {};
      // Parent registry: a stable per-container index so sibling tappables can be
      // grouped (a button-cluster choice picker). Plus a selected-state read, so a
      // mutually-exclusive choice group (exactly one selected) is distinguishable
      // from a row of action buttons (none selected). Used by detectChoiceGroups.
      const parentReg = new Map();
      let parentIdx = 0;
      const groupOf = (el) => {
        const par = el.parentElement;
        if (!par) return -1;
        if (!parentReg.has(par)) parentReg.set(par, parentIdx++);
        return parentReg.get(par);
      };
      // Owning-container id for a choice option: the CLOSEST ARIA choice container
      // (tablist / radiogroup / menu(bar)) or a <fieldset>, registered to a stable
      // id in DOM order. This scopes the choice-anomaly oracle per component so two
      // INDEPENDENT tablists/radiogroups on one page are not compared as one (which
      // produced false outliers). A radio with no container still groups by its
      // `name`. null when nothing owns it (the oracle then falls back to bare role).
      const choiceReg = new Map();
      let choiceIdx = 0;
      const choiceContainerOf = (el) => {
        const cont =
          el.closest &&
          el.closest('[role=tablist],[role=radiogroup],[role=menu],[role=menubar],fieldset');
        if (cont) {
          if (!choiceReg.has(cont)) choiceReg.set(cont, 'c' + choiceIdx++);
          return choiceReg.get(cont);
        }
        const tag = el.tagName ? el.tagName.toLowerCase() : '';
        if (tag === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio') {
          const nm = el.getAttribute('name');
          if (nm) return 'name:' + nm;
        }
        return null;
      };
      const selectedState = (el) => {
        const a = (n) => (el.getAttribute(n) || '').toLowerCase();
        if (a('aria-pressed') === 'true' || a('aria-selected') === 'true') return true;
        if (a('aria-checked') === 'true' || el.getAttribute('aria-current') != null) return true;
        const ds = a('data-state');
        if (['active', 'selected', 'on', 'checked', 'open'].includes(ds)) return true;
        return false;
      };
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

      // Stable author contract: data-testid > name (for the descriptor token).
      const idOf = (el) => {
        const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
        if (testid && testid.trim()) return testid.trim();
        const name = el.getAttribute('name');
        if (name && name.trim()) return name.trim();
        return null;
      };

      // Selector KEY (for replay): kind-tagged so tap() can resolve it. Same
      // Raw DOM ids are intentionally skipped: their lifetime is not knowable from
      // one capture, so they cannot support a deterministic saved replay.
      const keyOf = (el) => {
        const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
        if (testid && testid.trim()) return 'testid:' + testid.trim();
        const name = el.getAttribute('name');
        if (name && name.trim()) return 'name:' + name.trim();
        return null;
      };

      // Elements running an INFINITE animation (a spinner/pulse/marquee that never
      // settles), computed ONCE per snapshot from a single document.getAnimations()
      // call. A per-node el.getAnimations() made every snapshot O(nodes) on a large
      // DOM (a code editor renders thousands of line nodes) and dominated the crawl;
      // this precompute + Set lookup is O(animations).
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
        // two renders of the same page diverge on it: exclude it. Finite animations
        // are already settled by settleForSignature before a parity capture.
        if (infiniteAnimEls.has(el)) return true;
        return false;
      };

      // RAW value-role (docs/signature.md "Value-state"): the value-role name for
      // a value-bearing DOM element, NEVER from text. role=status/log/progressbar/
      // meter/timer pass through; <output>/role=output -> output; an aria-live
      // region (polite/assertive) -> status (so a live counter is value-bearing
      // WITHOUT opt-in); text form fields -> textfield. null for chrome / non-text
      // inputs (password is never read).
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
        // Text fields ARE actionable: the explorer drives them with a "type"
        // action. Without this, form-gated apps (login, search, TodoMVC new-todo)
        // map to a single dead state because their only control is undrivable.
        if (tag === 'input' || tag === 'textarea') return true;
        if (role === 'textfield') return true;
        if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
          return true;
        if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
        return false;
      };

      // A link that navigates OFF the app-under-test's origin (a team member's
      // LinkedIn, a "View on GitHub" footer). Tapping it leaves the app, so the
      // explorer must not offer it as an action: the destination is a foreign
      // site, not a state of the app, and recording it produces phantom states +
      // spurious dead ends. mailto:/tel:/javascript: are not external navigation.
      const isExternalLink = (el) => {
        const a = el.closest && el.closest('a[href]');
        if (!a) return false;
        let href;
        try {
          href = new URL(a.getAttribute('href'), location.href);
        } catch (e) {
          return false;
        }
        if (href.protocol !== 'http:' && href.protocol !== 'https:') return false;
        return href.origin !== location.origin;
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
      // REACHABLE: a real user can hit this element. Style-visible is NOT enough,
      // an offstage control (positioned outside the viewport) or one fully occluded
      // by another element is style-visible but un-tappable. The floor test is the
      // SAME hit-test used by the framebuffer probe (runFramebufferProbe ~L1052):
      // the element's center must lie inside the viewport AND a hit-test there must
      // resolve to the element or a descendant (so a button whose deepest painted
      // node is an inner <span> still counts). Used to gate tap candidacy AND the
      // role+index assignment so an unreachable control is neither offered as an
      // action nor given an index a replay could resolve to.
      const reachable = (el) => {
        if (!visible(el)) return false;
        const r = el.getBoundingClientRect();
        const cx = r.left + r.width / 2;
        const cy = r.top + r.height / 2;
        const vw = window.innerWidth || document.documentElement.clientWidth;
        const vh = window.innerHeight || document.documentElement.clientHeight;
        if (cx < 0 || cy < 0 || cx >= vw || cy >= vh) return false;
        const hit = document.elementFromPoint(cx, cy);
        if (!hit) return false;
        return hit === el || el.contains(hit);
      };
      const boundsOf = (el) => {
        try {
          const r = el.getBoundingClientRect();
          if (!r || r.width <= 0 || r.height <= 0) return null;
          return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)];
        } catch (_) {
          return null;
        }
      };
      // Pointer-operable but OUTSIDE interactive()'s tappable grammar: a control a
      // pointer user can drive (cursor:pointer, or an ARIA-interactive role /
      // focusable tabindex delegation marker) that interactive() does not take.
      // The operability ground truth (EXPLORE:GROUNDTRUTH) already counts these as
      // operable; mirroring that predicate here lets the explorer actually TAP
      // them, so an SPA built from delegated-click <div role=option> elements no
      // longer maps to a single state. Kept deliberately conservative (and the
      // caller adds ONLY keyed elements) so it expands coverage without flooding
      // the candidate set with decorative cursor:pointer chrome.
      const ARIA_OPERABLE = {
        button: 1,
        link: 1,
        checkbox: 1,
        radio: 1,
        switch: 1,
        tab: 1,
        menuitem: 1,
        menuitemcheckbox: 1,
        menuitemradio: 1,
        option: 1,
        slider: 1,
      };
      const pointerOperable = (el) => {
        // cursor:pointer is INHERITED, so only count an element that INTRODUCES it
        // (its parent is not already pointer), matching the ground-truth guard so a
        // clickable parent does not paint every descendant as a candidate.
        const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
        if (getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer') return true;
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (ARIA_OPERABLE[ariaRole]) return true;
        const ti = el.getAttribute('tabindex');
        if (ti !== null && parseInt(ti, 10) >= 0) return true;
        return false;
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
          // Tap candidacy requires REACHABILITY, not just interactivity: an
          // offstage / occluded control is interactive in the DOM but a user can't
          // reach it, so the explorer must not offer it as an action and ddmin must
          // not be able to minimize a repro through it. Gating here means such a
          // control also never consumes a role+index slot (the index is assigned
          // from rawTaps below), so no replay selector can resolve to it.
          const isInteractive = interactive(el, role);
          let structuralIndex = -1;
          if (isInteractive) {
            structuralIndex = visiblePerRole[role] || 0;
            visiblePerRole[role] = structuralIndex + 1;
          }
          if (isInteractive && reachable(el)) {
            const ac = ((el.getAttribute && el.getAttribute('autocomplete')) || '').toLowerCase();
            const it = ((el.getAttribute && el.getAttribute('type')) || '').toLowerCase();
            const purpose =
              ac === 'one-time-code'
                ? 'otp'
                : ac === 'current-password' || ac === 'new-password' || it === 'password'
                  ? 'password'
                  : ac === 'username'
                    ? 'username'
                    : ac === 'email' || it === 'email'
                      ? 'email'
                      : ac === 'tel' || ac === 'tel-national' || it === 'tel'
                        ? 'phone'
                        : null;
            rawTaps.push({
              role,
              key: keyOf(el),
              index: structuralIndex,
              label: name ? clipLabel(name) : '',
              bounds: boundsOf(el),
              external: isExternalLink(el),
              grp: groupOf(el),
              cgrp: choiceContainerOf(el),
              selected: selectedState(el),
              purpose,
            });
          } else if (reachable(el) && pointerOperable(el)) {
            // Only KEYED extras: a stable `key:<id>` selector is reproducible and
            // does NOT consume a role+index slot, so existing role:<role>#<idx>
            // selectors and the canonical signature are untouched. A pointer-
            // operable element with no stable id is exactly one a repro could not
            // address anyway, so dropping it here loses nothing replayable.
            const k = keyOf(el);
            if (k) {
              extraTaps.push({
                role,
                key: k,
                label: name ? clipLabel(name) : '',
                bounds: boundsOf(el),
              });
            }
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

      // Structural selectors for replay (key, else viewport-independent role
      // index). `index` was assigned across every style-visible interactive,
      // while rawTaps contains only the subset a user can reach right now.
      const tappables = rawTaps.map((tn) => {
        const idx = tn.index;
        const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
        return {
          sel,
          role: tn.role,
          index: idx,
          key: tn.key,
          label: tn.label,
          bounds: tn.bounds || null,
          external: !!tn.external,
          grp: tn.grp,
          cgrp: tn.cgrp != null ? tn.cgrp : null,
          selected: !!tn.selected,
          purpose: tn.purpose || null,
        };
      });
      // Append the keyed pointer-operable extras (keyed selector only; no role
      // index, so nothing above shifts). Dedup against selectors already present
      // so an element can never appear twice in the candidate set.
      const present = new Set(tappables.map((t) => t.sel));
      for (const tn of extraTaps) {
        const sel = 'key:' + tn.key;
        if (present.has(sel)) continue;
        present.add(sel);
        tappables.push({
          sel,
          role: tn.role,
          index: -1,
          key: tn.key,
          label: tn.label,
          bounds: tn.bounds || null,
        });
      }

      const texts = [];
      const seenTextBoxes = new Set();
      for (const el of Array.from(document.querySelectorAll('body *'))) {
        if (!visible(el)) continue;
        let text = '';
        for (const c of el.childNodes) {
          if (c.nodeType === 3) text += c.textContent || '';
        }
        text = text.replace(/\s+/g, ' ').trim();
        if (!text) continue;
        const bounds = boundsOf(el);
        if (!bounds) continue;
        const key = text + '|' + bounds.join(',');
        if (seenTextBoxes.has(key)) continue;
        seenTextBoxes.add(key);
        texts.push({ text: clipLabel(text), bounds });
        if (texts.length >= 48) break;
      }

      // Anchor: route of the current screen = normalized path + query + SPA hash.
      // Both query-routed pages (`/login?returnTo=/download`) and hash-routed
      // pages (#/a vs #/b) can expose distinct navigation frontiers. Keeping the
      // evidence-safe URL identity keeps ordinary routing values while redacting
      // secrets; the exact query remains internal to navigation/status lookup.
      let anchor = null;
      let path = null;
      try {
        if (location && location.pathname) {
          let pth = location.pathname;
          // Trailing-slash route normalization (mirrors host-side normalizePathname):
          // /docs/ and /docs are the SAME screen, so a 301 that toggles the slash is
          // not a distinct route (else the same screen double-counts / a benign
          // redirect reads as a broken route).
          if (pth.length > 1) pth = pth.replace(/\/+$/, '') || '/';
          path = pth + (location.search || '');
          const safe = new URLSearchParams();
          const sensitive =
            /(auth|code|credential|jwt|key|nonce|password|secret|session|sig|state|ticket|token)/i;
          const tracking = /^(utm_.+|fbclid|gclid|dclid|msclkid|mc_[ce]id)$/i;
          for (const [key, value] of new URLSearchParams(location.search || '')) {
            if (tracking.test(key)) continue;
            safe.append(key, sensitive.test(key) ? '<redacted>' : value);
          }
          const query = safe.toString();
          let hash = location.hash || '';
          const hashQuery = hash.indexOf('?');
          if (hashQuery >= 0) hash = hash.slice(0, hashQuery);
          anchor = pth + (query ? '?' + query : '') + hash;
        }
      } catch (e) {}

      // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
      // value + keyed-text nodes. Sorted here so it is order-independent.
      textNodes.sort((a, b) =>
        a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0,
      );

      // Return `tree` as a JSON STRING, not the live object. Playwright's evaluate
      // serializer caps object-graph DEPTH (~100 nested refs) and throws "object
      // reference chain is too long" on a deeply nested DOM (e.g. docs sites with
      // many wrapper divs) -- which killed observe()/the whole crawl before any
      // state-present oracle (choice-anomaly, overflow) could run. A string has no
      // object graph, so it serializes regardless of DOM depth; parsed back below.
      return {
        tree: JSON.stringify(tree),
        anchor,
        path,
        labels: [...new Set(labels)],
        tappables,
        texts,
        textNodes,
      };
    },
    { maxLen: MAX_LABEL_LEN, valueNodeSelectors: valueNodeSelectors || [] },
  );
  // Reparse the canonical tree (stringified in-page to dodge the serializer's
  // depth cap) back into the object signatureOf/descriptorOf consume.
  snap.tree = JSON.parse(snap.tree);

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

// DOM QUIESCENCE settle before a STRUCTURAL-SIGNATURE capture. It waits for the
// page to STOP changing so that two independent renders of the same URL converge:
//   1. network idle (no in-flight requests for a settle window),
//   2. no DOM mutation for a stable window (a MutationObserver quiet period),
//   3. running CSS transitions / Web Animations settled, then two clean frames.
// The blank-screen oracle applies this before re-checking a candidate-blank state,
// so a still-hydrating mid-load frame is not mistaken for a white-screen-of-death.
// Every wait is HARD-CAPPED, so a page that never idles (an infinite spinner /
// poll) still returns. Best-effort: any failure is ignored and the caller falls
// back to whatever is on screen.
async function settleForSignature(page) {
