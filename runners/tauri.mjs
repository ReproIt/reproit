import{mkdirSync as K,writeFileSync as se}from"node:fs";import{join as W}from"node:path";import{signatureOf as ce,descriptorOf as le,valueClass as Oe,fnv1a as _e,loadValueNodes as Re}from"./shared/signature.mjs";import{loadFuzz as Ae,rng as Ce,INJECTED_VALUES as Z,expandEnv as Te}from"./shared/fuzz.mjs";import{execFileSync as Ne,spawn as ue}from"node:child_process";import{platform as Q}from"node:os";import{CHOICE_ANOMALY_IN_PAGE_SRC as Ie,CHOICE_OUTLIER_RATIO as Le,CHOICE_MIN_MAGNITUDE as Pe,CHOICE_ROLES as Fe}from"./web/choice-oracle.mjs";import{occlusionScan as de,confirmOcclusions as Je,securityScan as De,focusLossArm as Ue,focusLossCheck as Me,blankScreenScan as fe,brokenAssetScan as We,zoomTappableKeys as He,zoomReflowScan as Be,scrollRoundTripScan as je}from"./web/hygiene-oracles.mjs";import{layoutOverflowScan as he,confirmLayoutOverflow as qe}from"./web/overflow-oracle.mjs";import{zeroContrastScan as Ye}from"./web/zero-contrast-oracle.mjs";import{inspectPlatformStep as ze}from"./inspect-control.mjs";const Ge=`
  var __reproitChoiceFn = ${Ie};
  var __reproitDone = arguments[arguments.length - 1];
  __reproitChoiceFn({
    settleMs: 600,
    ratio: ${Le},
    minMag: ${Pe},
    choiceRoles: ${JSON.stringify(Fe)},
  }).then(function (findings) { __reproitDone(findings || []); })
    .catch(function () { __reproitDone([]); });
`,$e=`
  var __srtFn = ${je.toString()};
  var __srtDone = arguments[arguments.length - 1];
  Promise.resolve(__srtFn()).then(function (items) { __srtDone(items || []); })
    .catch(function () { __srtDone([]); });
`,H=process.env.REPROIT_APP,Xe=process.env.REPROIT_WEBDRIVER_URL||"http://127.0.0.1:4444",B=process.env.REPROIT_VIDEO_DIR||void 0,j=process.env.REPROIT_PROBE==="1",Ve=36,Yt=40,Ke=8;function o(t){process.stdout.write(t+`
`)}async function ge(t,r){const i=process.env.REPROIT_SHOTS_DIR;if(i)try{K(i,{recursive:!0});const e=await t.takeScreenshot();se(W(i,r+".png"),Buffer.from(e,"base64"))}catch{}o("SHOOT:"+r)}import{snapshotJs as Ze}from"./tauri-snapshot.mjs";async function Qe(t,r){const i=await t.execute(Ze(r||[]));i.sig=ce(i.anchor,i.tree);const e=le(i.anchor,i.tree),s=e.indexOf(`
V:`);return i.vsection=s>=0?e.slice(s+3):"",i.structuralSig=s>=0?_e(e.slice(0,s)):i.sig,i.content=i.sig+"|"+i.textNodes.map(a=>a[0]+"="+a[1]).join(";"),i}async function et(t){try{await t.executeAsync(r=>{const i=()=>new Promise(e=>requestAnimationFrame(()=>requestAnimationFrame(e)));(async()=>{await new Promise(e=>{let s=null,a=null;const l=()=>{if(a&&clearTimeout(a),p&&clearTimeout(p),s)try{s.disconnect()}catch{}e()},g=()=>{a&&clearTimeout(a),a=setTimeout(l,400)},p=setTimeout(l,1800);try{s=new MutationObserver(g),s.observe(document.documentElement,{subtree:!0,childList:!0,attributes:!0,characterData:!0})}catch{}g()});try{const e=(document.getAnimations?document.getAnimations():[]).filter(s=>s.playState==="running");await Promise.race([Promise.allSettled(e.map(s=>s.finished)),new Promise(s=>setTimeout(s,800))])}catch{}await i(),r()})()})}catch{}}async function tt(t){try{return await t.execute(()=>{const r=(document.title||"").toLowerCase(),i=(document.body&&document.body.innerText||"").toLowerCase(),e=s=>s.test(r)||s.test(i);return document.querySelector('#challenge-running, #cf-challenge-running, #challenge-form, .cf-turnstile, [id^="cf-chl"], script[src*="challenge-platform"], iframe[src*="challenges.cloudflare.com"]')?{vendor:"Cloudflare",marker:"challenge-platform"}:e(/just a moment/)||e(/checking your browser before/)||e(/performing (a )?security verification/)||e(/enable javascript and cookies to continue/)?{vendor:"Cloudflare",marker:"interstitial"}:e(/attention required/)&&e(/cloudflare/)?{vendor:"Cloudflare",marker:"attention-required"}:document.querySelector('#px-captcha, .px-block, [class*="perimeterx"]')?{vendor:"PerimeterX",marker:"px-captcha"}:/ray id:/.test(i)&&i.length<1200?{vendor:"Cloudflare",marker:"ray-id-block"}:null})}catch{return null}}const nt=`
  const ROLES = {
    screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
    icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
    slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
  };
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
  const interactive = (el, role) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select'].includes(tag)) return true;
    if (tag === 'input' || tag === 'textarea') return true;
    if (role === 'textfield') return true;
    if (
      ['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)
    ) return true;
    if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
    return false;
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
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
  const nativeInteractive = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'summary'].includes(tag)) return true;
    if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      return t !== 'hidden';
    }
    if (el.isContentEditable) return true;
    return false;
  };
  // Roles that name a region or a piece of document structure, NOT an operable
  // widget. A landmark (search/navigation/banner/...) or a structural/live role
  // is something a pointer user reads, not something they "operate", so it must
  // not count as a delegation marker, else it is promoted to operable by a
  // page-wide document click handler and surfaces as a phantom gap.
  const NON_INTERACTIVE_ROLES = new Set([
    'banner', 'complementary', 'contentinfo', 'form', 'main', 'navigation',
    'region', 'search',
    'article', 'definition', 'directory', 'document', 'feed', 'figure', 'group',
    'heading', 'img', 'list', 'listitem', 'math', 'none', 'note', 'presentation',
    'separator', 'table', 'term', 'toolbar', 'tooltip', 'caption', 'rowgroup',
    'row', 'cell', 'columnheader', 'rowheader',
    'dialog', 'alertdialog', 'alert', 'log', 'marquee', 'status', 'timer',
    'application',
  ]);
  const hasDelegationMarker = (el) => {
    const role = (el.getAttribute('role') || '').trim().toLowerCase();
    if (role && !NON_INTERACTIVE_ROLES.has(role)) return true;
    if (el.hasAttribute('tabindex')) return true;
    return false;
  };
  // aria-activedescendant: an item operated via a focusable composite widget (a
  // listbox/menu/tree/grid/combobox whose CONTAINER holds focus and moves a
  // roving "active" item). Such items are keyboard-reachable AND activatable even
  // with tabindex=-1, because the container handles the keys.
  const adManaged = (el) => {
    const isFocusable = (c) => {
      const ti = c.getAttribute('tabindex');
      return (ti !== null && parseInt(ti, 10) >= 0) || nativeInteractive(c);
    };
    if (el.hasAttribute('aria-activedescendant') && isFocusable(el)) return true;
    const c = el.closest('[aria-activedescendant]');
    if (c && c !== el && isFocusable(c)) return true;
    const id = el.getAttribute('id');
    if (id) {
      const q = window.CSS && CSS.escape ? CSS.escape(id) : id;
      const ref = document.querySelector('[aria-activedescendant="' + q + '"]');
      if (ref && isFocusable(ref)) return true;
    }
    return false;
  };
  // reachable: on-screen AND hit-testable, so a real pointer user can operate it.
  // The operable gate below uses this so an off-screen/occluded control is not a
  // phantom pointer-only/keyboard gap.
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
  const rolePresent = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'input', 'summary'].includes(tag)) return true;
    if (/^h[1-6]$/.test(tag)) return true;
    const ar = (el.getAttribute('role') || '').trim().toLowerCase();
    if (!ar) return false;
    return !['none', 'presentation', 'generic'].includes(ar);
  };
  const namePresent = (el) => {
    const aria = el.getAttribute('aria-label'); if (aria && aria.trim()) return true;
    const lb = el.getAttribute('aria-labelledby'); if (lb && lb.trim()) return true;
    const title = el.getAttribute('title'); if (title && title.trim()) return true;
    const alt = el.getAttribute('alt'); if (alt && alt.trim()) return true;
    const ph = el.getAttribute('placeholder'); if (ph && ph.trim()) return true;
    const text = (el.innerText || el.textContent || '').trim();
    return text.length > 0;
  };
  const gestureKindOf = (el, role, native, deleg) => {
    if (role === 'textfield') return 'field';
    if (native) return 'button';
    if (deleg) return 'delegated';
    return 'tap';
  };
  // No CDP: approximate the document-level delegated-click pattern by reading
  // an inline document.onclick / body.onclick handler (the only listener kind
  // visible to script). Real addEventListener handlers are invisible here, so
  // Tauri's delegated detection is best-effort and conservative.
  const docDelegates = !!(document.onclick || (document.body && document.body.onclick));

  const out = [];
  const perRole = {};
  const root = document.body || document.documentElement;
  const walk = (el, isRoot) => {
    if (!isRoot && !visible(el)) { for (const c of el.children) walk(c, false); return; }
    if (!isRoot) {
      const role = roleOf(el);
      const inWalk = interactive(el, role);
      const native = nativeInteractive(el);
      const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
      const cursor = getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer';
      const deleg = hasDelegationMarker(el);
      const ownInline = !!el.onclick || el.hasAttribute('onclick');
      const candidate = inWalk || native || cursor || deleg || ownInline;
      let sel;
      if (inWalk) {
        const idx = perRole[role] || 0; perRole[role] = idx + 1;
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#' + idx;
      } else if (candidate) {
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#gt' + out.length;
      }
      if (candidate) {
        // operable is graph 1: an element a pointer can ACTUALLY operate now. An
        // off-screen/occluded control is not pointer-operable, so it cannot be a
        // pointer-only/keyboard gap either; gate on reachability to align the two
        // graphs (matches the web runner).
        const operable = reachable(el) && (
          native || cursor || ownInline || (docDelegates && deleg)
        );
        // inTabOrder: sequential-focus reachability. An element is in the Tab
        // sequence iff it is focusable AND its tabIndex is >= 0. A tabindex=-1
        // element is script/pointer focusable but NOT reachable by Tab (the
        // motivating <div role=option tabindex=-1> case). An aria-activedescendant
        // item is reachable + activatable via its focusable composite container.
        const adm = adManaged(el);
        const focusable = native || el.tabIndex >= 0 ||
          (el.hasAttribute('tabindex') && el.tabIndex >= 0) || adm;
        const inTabOrder = (el.tabIndex >= 0 && focusable) || adm;
        const a11y = {
          rolePresent: rolePresent(el),
          namePresent: namePresent(el),
          inTabOrder: inTabOrder,
          focusable: focusable,
        };
        if (operable) {
          if (!inTabOrder && !native) {
            a11y.keyboardActivatable = false;
          } else {
            // keyboardActivatable, derived WITHOUT firing the control. We must
            // NOT synthesize Enter/Space (even via dispatchEvent): a bubbling
            // keydown fires the app's real handler (a navigation, or a crash) as
            // a side effect, polluting the crash oracle. A Tauri webview has no
            // CDP, so we cannot enumerate addEventListener key handlers; the most
            // we can read cheaply is the native semantics and inline on* handlers.
            // A native control, or one with an inline key handler, is keyboard-
            // activatable. Otherwise, since the element is focusable and in the
            // Tab order, we assume activatable rather than flag a gap we cannot
            // prove (matches the web runner's no-CDP fallback; it means Tauri
            // under-reports the click-only-div case the CDP path catches).
            const inlineKey = !!(el.onkeydown || el.onkeypress || el.onkeyup);
            a11y.keyboardActivatable = native || inlineKey || focusable;
          }
        }
        out.push({
          id: sel,
          operable: operable,
          gestureKind: gestureKindOf(el, role, native, deleg),
          a11y,
        });
      }
    }
    for (const c of el.children) walk(c, false);
  };
  if (root) walk(root, true);
  // Focus trap detection needs a real Tab traversal, which the webview can't do
  // from script; report false (a missing/false focusTrap is the safe default).
  return { elements: out, focusTrap: false };
`;async function it(t,r){let i;try{i=await t.execute(nt)}catch{return}i&&o("EXPLORE:GROUNDTRUTH "+JSON.stringify({sig:r,focusTrap:!!i.focusTrap,elements:i.elements||[]}))}const rt=`
  // Fuzzer provenance (mirrors the web tier): a reflected fuzzer probe is not the
  // app's own broken content. arguments[0] is the injected-values array passed by
  // browser.execute(DETECT_CONTENTBUG_JS, [...INJECTED_VALUES]).
  const injected = (Array.isArray(arguments[0]) ? arguments[0] : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    if (injected.some(
      (v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1),
    )) return true;
    // Fragmented reflection: the browser parsed markup out of the probe, so the
    // visible text is a fragment; check the specific artifact tokens for provenance.
    const arts = [];
    const tm = n.match(/\\{\\{[^}]*\\}\\}/g); if (tm) arts.push(...tm);
    const dm = n.match(/\\$\\{[^}]*\\}/g); if (dm) arts.push(...dm);
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
    return t.replace(/\\s+/g, ' ').trim();
  };
  // Prose guard for BOTH artifact kinds: fire only when the artifact IS the label,
  // never when docs prose merely mentions "[object Object]" or the "{{ }}" syntax.
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text.replace(/\\[object Object\\]/g, ' ').replace(/\\s+/g, ' ').trim();
      if (dominates(s)) return 'object-object';
    }
    if (/\\{\\{[^}]*\\}\\}/.test(text) || /\\$\\{[^}]*\\}/.test(text)) {
      const s = text
        .replace(/\\{\\{[^}]*\\}\\}/g, ' ')
        .replace(/\\$\\{[^}]*\\}/g, ' ')
        .replace(/\\s+/g, ' ')
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
  out.sort((a, b) => (
    a.key < b.key ? -1 : a.key > b.key ? 1 :
      (a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0)
  ));
  return out;
`,q=200,Y=2e3,at=`
  try {
    if (!window.__reproitLongTaskHooked) {
      window.__reproitLongTaskHooked = true;
      window.__reproitLongTasks = [];
      const obs = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
      });
      obs.observe({ entryTypes: ['longtask'] });
    }
  } catch (_) { /* no Long Tasks API: jank/hang silent on this webview */ }
  return true;
`,ot="try { window.__reproitLongTasks = []; } catch (_) {} return true;",st=`
  const t = window.__reproitLongTasks || [];
  window.__reproitLongTasks = [];
  return t;
`;async function me(t){try{await t.execute(at)}catch{}}async function ct(t){let r=[];try{r=await t.execute(st)}catch{return null}if(!r||!r.length)return null;const i=Math.max(...r);return i>=Y?{kind:"hang",bucket:Y,count:r.length}:i>=q?{kind:"jank",bucket:q,count:r.length}:null}const pe=100,lt=2,ut=350;function be(t){if(!t||!t.length)return null;let r=0;for(const a of t)a>=Y&&r++;if(r>0)return{kind:"hang",bucket:Y,count:r};let i=0,e=0;const s=t.length;for(;e<s;){if(t[e]<pe){e++;continue}let a=e,l=0,g=0;for(;a<s&&t[a]>=pe;)l+=t[a],t[a]>g&&(g=t[a]),a++;const p=a-e,S=g>=ut,E=p>=lt&&l>=q;(S||E)&&i++,e=a}return i>0?{kind:"jank",bucket:q,count:i}:null}const dt=`
  try {
    if (!window.__reproitFrameHooked) {
      window.__reproitFrameHooked = true;
      window.__reproitFrameIntervals = [];
      let last = -1;
      const tick = (now) => {
        if (last >= 0) {
          const d = now - last;
          const buf = window.__reproitFrameIntervals;
          if (buf.length < 4096) buf.push(Math.round(d));
        }
        last = now;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }
  } catch (_) { /* no rAF: cross-engine jank/hang silent (never a false positive) */ }
  return true;
`,ft="try { window.__reproitFrameIntervals = []; } catch (_) {} return true;",ht=`
  const t = window.__reproitFrameIntervals || [];
  window.__reproitFrameIntervals = [];
  return t;
`;async function we(t){try{await t.execute(dt)}catch{}}async function gt(t){let r=[];try{r=await t.execute(ht)}catch{return null}return be(r)}async function mt(t){const r=await ct(t);return r||gt(t)}const pt=`
  try {
    if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
      return performance.memory.usedJSHeapSize;
    }
  } catch (_) {}
  return null;
`;function N(t,r){try{const i=Ne(t,r,{encoding:"utf8",stdio:["ignore","pipe","ignore"],timeout:5e3});return i==null?null:String(i)}catch{return null}}function ye(t){if(!t)return null;if(Q()==="win32"){const s=t.split(/[\\/]/).pop()||t,a=N("tasklist",["/FI","IMAGENAME eq "+s,"/FO","CSV","/NH"]);if(a==null)return null;const l=[];for(const g of a.split(/\r?\n/)){const p=g.match(/^"[^"]*","(\d+)"/);p&&l.push(parseInt(p[1],10))}return l.length!==1||!Number.isFinite(l[0])||l[0]<=0?null:l[0]}const i=N("ps",["-axww","-o","pid=,comm="]);if(i==null)return null;const e=[];for(const s of i.split(`
`)){const a=s.match(/^\s*(\d+)\s+(.*)$/);a&&a[2].trim()===t&&e.push(parseInt(a[1],10))}return e.length!==1||!Number.isFinite(e[0])||e[0]<=0?null:e[0]}function bt(t){if(!(t>0))return null;if(Q()==="win32"){const e=N("tasklist",["/FI","PID eq "+t,"/FO","CSV","/NH"]);if(e==null)return null;const s=e.match(/"([\d.,]+)\s*K"/);if(!s)return null;const a=parseInt(s[1].replace(/[.,]/g,""),10);return!Number.isFinite(a)||a<=0?null:a*1024}const r=N("ps",["-o","rss=","-p",String(t)]);if(r==null)return null;const i=parseInt(r.trim(),10);return!Number.isFinite(i)||i<=0?null:i*1024}async function ee(t,r,i){if(i&&(i.tried||(i.tried=!0,i.pid=ye(H)),i.pid>0)){const s=bt(i.pid);if(s!=null){o("MEMORY:SAMPLE "+JSON.stringify({t_ms:r,heap_used:s}));return}}let e=null;try{e=await t.execute(pt)}catch{e=null}e!=null&&o("MEMORY:SAMPLE "+JSON.stringify({t_ms:r,heap_used:e}))}const wt=`
  if (!window.__reproit_hooked) {
    window.__reproit_hooked = true;
    window.__reproit_errors = [];
    window.addEventListener('error', (ev) => {
      try {
        const e = ev.error;
        window.__reproit_errors.push({
          message: (e && e.message) || ev.message || String(e || ev),
          source: ev.filename || '',
          line: ev.lineno || 0,
          stack: (e && e.stack) ? String(e.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    window.addEventListener('unhandledrejection', (ev) => {
      try {
        const r = ev.reason;
        window.__reproit_errors.push({
          message: (r && r.message) ? r.message : ('Unhandled rejection: ' + String(r)),
          source: '',
          line: 0,
          stack: (r && r.stack) ? String(r.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    // We intentionally do NOT also set window.onerror: in WebKitGTK both the
    // 'error' event listener above and window.onerror fire for the same
    // uncaught error, which would emit the block twice. The 'error' event is
    // the reliable single source (same as the web runner's page.on('pageerror')).
  }
  return true;
`;async function z(t){try{await t.execute(wt)}catch{}}function yt(t){o("EXCEPTION CAUGHT BY TAURI WEBVIEW"),o("The following error was thrown:"),o(String(t&&t.message?t.message:t));const r=t&&t.stack?String(t.stack):"";for(const i of r.split(`
`).slice(0,8))i&&o(i);o("\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550")}async function G(t){let r=[];try{r=await t.execute(()=>{const i=window.__reproit_errors||[];return window.__reproit_errors=[],i})}catch{return}if(Array.isArray(r))for(const i of r)yt(i)}const vt=`
  const s = arguments[0];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const cssEscape = (v) => (
    window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&')
  );

  const doClick = (el) => {
    // Stash the clicked element for the post-tap oracle probes (the focus-loss
    // guards read it in-page). A window ref only, never a DOM mutation, so the
    // signature/content/mutation oracles are untouched.
    try {
      window.__reproitLastTap = el;
      // FOCUS-LOSS probe: a real user click gives the control keyboard focus
      // before activating it; el.click() alone does not. When the walk armed
      // the probe pre-tap (focusLossArm), focus first (no scroll, so the
      // viewport-dependent snapshot is untouched) so the oracle can observe
      // whether the app's re-render then drops focus back to <body>.
      if (window.__reproitFocusProbe) {
        try { el.focus({ preventScroll: true }); } catch (_) {}
        window.__reproitTapFocused = document.activeElement === el;
      }
    } catch (_) {}
    el.click();
    return true;
  };

  if (s.startsWith('key:')) {
    const body = s.slice(4);
    const ci = body.indexOf(':');
    if (ci < 0) return false;
    const kind = body.slice(0, ci);
    const val = body.slice(ci + 1);
    let el = null;
    if (kind === 'testid') {
      el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
        || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
    } else if (kind === 'id') {
      el = document.getElementById(val);
    } else if (kind === 'name') {
      el = document.querySelector('[name="' + cssEscape(val) + '"]');
    }
    if (!el) return false;
    return doClick(el);
  }

  if (s.startsWith('role:')) {
    const hash = s.indexOf('#');
    if (hash < 0) return false;
    const role = s.slice('role:'.length, hash);
    const idx = parseInt(s.slice(hash + 1), 10);
    if (!(idx >= 0)) return false;
    const ROLES = {
      screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
      icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
      slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
    };
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
    const interactive = (el, r) => {
      const tag = el.tagName.toLowerCase();
      if (['a', 'button', 'select'].includes(tag)) return true;
      if (tag === 'input') {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        return !['text', 'password', 'email', 'number', 'search'].includes(t);
      }
      if (
        ['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r)
      ) return true;
      if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
      return false;
    };
    let seen = -1, target = null;
    const walk = (el) => {
      if (target) return;
      if (!visible(el)) { for (const c of el.children) walk(c); return; }
      const r = roleOf(el);
      if (interactive(el, r) && r === role) { seen++; if (seen === idx) { target = el; return; } }
      for (const c of el.children) walk(c);
    };
    const root = document.body || document.documentElement;
    if (root) walk(root);
    if (!target) return false;
    return doClick(target);
  }

  return false;
`;async function ve(t,r){try{return!!await t.execute(vt,r)}catch{return!1}}const St=`
  const s = arguments[0];
  const done = arguments[arguments.length - 1];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const cssEscape = (v) => (
    window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&')
  );
  let el = null;
  if (s.startsWith('key:')) {
    const body = s.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid') {
      el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
        || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
    } else if (kind === 'id') {
      el = document.getElementById(val);
    } else if (kind === 'name') {
      el = document.querySelector('[name="' + cssEscape(val) + '"]');
    }
  } else if (s.startsWith('role:')) {
    const hash = s.indexOf('#');
    if (hash >= 0) {
      const role = s.slice('role:'.length, hash);
      const idx = parseInt(s.slice(hash + 1), 10);
      const ROLES = {
        screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
        icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
        slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
      };
      const roleOf = (n) => {
        const tag = n.tagName.toLowerCase();
        const ariaRole = (n.getAttribute('role') || '').toLowerCase();
        if (ariaRole) {
          if (
            ariaRole === 'textbox' || ariaRole === 'searchbox' ||
            ariaRole === 'combobox'
          ) return 'textfield';
          if (ariaRole === 'heading') return 'header';
          if (ariaRole === 'img') return 'image';
          if (ariaRole === 'switch') return 'switch';
          if (ariaRole === 'link') return 'link';
          if (ariaRole === 'button') return 'button';
          if (ROLES[ariaRole]) return ariaRole;
        }
        if (tag === 'input') {
          const t = (n.getAttribute('type') || 'text').toLowerCase();
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
      const interactive = (n, r) => {
        const tag = n.tagName.toLowerCase();
        if (['a', 'button', 'select'].includes(tag)) return true;
        if (tag === 'input') {
          const t = (n.getAttribute('type') || 'text').toLowerCase();
          return !['text', 'password', 'email', 'number', 'search'].includes(t);
        }
        if (
          ['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r)
        ) return true;
        if (n.hasAttribute('onclick') || n.tabIndex >= 0) return true;
        return false;
      };
      let seen = -1;
      const walk = (n) => {
        if (el) return;
        if (!visible(n)) { for (const c of n.children) walk(c); return; }
        const r = roleOf(n);
        if (interactive(n, r) && r === role) { seen++; if (seen === idx) { el = n; return; } }
        for (const c of n.children) walk(c);
      };
      const root = document.body || document.documentElement;
      if (root && idx >= 0) walk(root);
    }
  }
  if (!el) { done(null); return; }
  // Scroll INSTANTLY (not smooth): a smooth animation is still moving when we
  // measure, so the rect would diverge from the settled frame the video holds.
  try { el.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' }); }
  catch (_) { try { el.scrollIntoView({ block: 'center', inline: 'center' }); } catch (__) {} }
  let lastY = -1, stable = 0, i = 0;
  const tick = () => {
    const y = window.scrollY;
    if (y === lastY) { stable++; } else { stable = 0; lastY = y; }
    i++;
    if (stable >= 2 || i >= 20) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) { done(null); return; }
      const vw = window.innerWidth || document.documentElement.clientWidth || 1;
      const vh = window.innerHeight || document.documentElement.clientHeight || 1;
      const ins = 4;
      const left = Math.min(Math.max(r.left - 2, ins), Math.max(ins, vw - ins - 8));
      const top = Math.min(Math.max(r.top - 2, ins), Math.max(ins, vh - ins - 8));
      const w = Math.max(8, Math.min(r.width + 4, vw - left - ins));
      const h = Math.max(8, Math.min(r.height + 4, vh - top - ins));
      done({ x: left, y: top, w, h, videoW: vw, videoH: vh });
      return;
    }
    setTimeout(tick, 50);
  };
  setTimeout(tick, 50);
`;async function kt(t,r){try{return await t.executeAsync(St,r)}catch{return null}}function xt(t,r){try{K(W(r,".."),{recursive:!0})}catch{}const i=Q();try{if(i==="linux"){const e=process.env.DISPLAY||":0";let s=(N("xdotool",["search","--pid",String(t),"--onlyvisible"])||"").trim().split(/\s+/).filter(Boolean).pop();if(!s)return null;const a=N("xdotool",["getwindowgeometry","--shell",s])||"",l={};for(const E of a.split(`
`)){const I=E.match(/^(\w+)=(-?\d+)/);I&&(l[I[1]]=parseInt(I[2],10))}if(!(l.WIDTH>0&&l.HEIGHT>0))return null;const g=l.WIDTH-l.WIDTH%2,p=l.HEIGHT-l.HEIGHT%2;return ue("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","x11grab","-framerate","15","-video_size",`${g}x${p}`,"-i",`${e}+${l.X||0},${l.Y||0}`,"-c:v","libx264","-pix_fmt","yuv420p",r],{stdio:["pipe","ignore","ignore"]})}if(i==="win32"){const s=(N("tasklist",["/FI","PID eq "+t,"/FO","CSV","/NH","/V"])||"").match(/^"[^"]*","\d+","[^"]*","[^"]*","[^"]*","([^"]*)"/),a=s&&s[1]&&s[1]!=="N/A"?s[1]:null;return a?ue("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","gdigrab","-framerate","15","-i","title="+a,"-c:v","libx264","-pix_fmt","yuv420p",r],{stdio:["pipe","ignore","ignore"]}):null}if(i==="darwin")return null}catch{}return null}async function Et(t){!t||t.exitCode!==null||await new Promise(r=>{let i=!1;const e=()=>{i||(i=!0,r())};t.once("exit",e);try{t.stdin&&t.stdin.writable&&t.stdin.write("q")}catch{}try{t.kill("SIGINT")}catch{}setTimeout(e,4e3)})}const Ot=`
  const s = arguments[0];
  const value = arguments[1];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const cssEscape = (v) => (
    window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&')
  );
  let el = null;
  if (s.startsWith('key:')) {
    const body = s.slice(4);
    const ci = body.indexOf(':');
    if (ci < 0) return false;
    const kind = body.slice(0, ci);
    const val = body.slice(ci + 1);
    if (kind === 'testid') {
      el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
        || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
    } else if (kind === 'id') {
      el = document.getElementById(val);
    } else if (kind === 'name') {
      el = document.querySelector('[name="' + cssEscape(val) + '"]');
    }
  } else if (s.startsWith('role:')) {
    const hash = s.indexOf('#');
    if (hash < 0) return false;
    const role = s.slice('role:'.length, hash);
    const idx = parseInt(s.slice(hash + 1), 10);
    if (!(idx >= 0)) return false;
    const roleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (
        ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox'
      ) return 'textfield';
      if (tag === 'input') {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        if (
          ['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image'].includes(t)
        ) return t;
        return 'textfield';
      }
      if (tag === 'textarea' || tag === 'select') return 'textfield';
      return ariaRole || tag;
    };
    let seen = -1, target = null;
    const walk = (el) => {
      if (target) return;
      if (!visible(el)) { for (const c of el.children) walk(c); return; }
      if (roleOf(el) === role) { seen++; if (seen === idx) { target = el; return; } }
      for (const c of el.children) walk(c);
    };
    const root = document.body || document.documentElement;
    if (root) walk(root);
    el = target;
  }
  if (!el || !visible(el)) return false;
  const tag = el.tagName.toLowerCase();
  const isText = tag === 'textarea'
    || (el.getAttribute &&
      (el.getAttribute('role') || '').toLowerCase().match(/textbox|searchbox|combobox/))
    || el.isContentEditable
    || (tag === 'input' && !['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image']
      .includes((el.getAttribute('type') || 'text').toLowerCase()));
  if (!isText) return false;
  try { el.focus(); } catch (e) {}
  if (el.isContentEditable && !('value' in el)) {
    el.textContent = value;
  } else {
    const proto = tag === 'textarea' ? window.HTMLTextAreaElement : window.HTMLInputElement;
    const desc = proto && Object.getOwnPropertyDescriptor(proto.prototype, 'value');
    if (desc && desc.set) desc.set.call(el, value); else el.value = value;
  }
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
`,_t=`
  const finder = arguments[0];
  const esc = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&'));
  let sel = finder;
  if (finder.startsWith('key:')) {
    const body = finder.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid') {
      sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    }
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try { els = document.querySelectorAll(sel); } catch (_) { return -1; }
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  let n = 0;
  for (const el of els) if (visible(el)) n++;
  return n;
`;async function Rt(t,r,i){if(o("FUZZ:ACT "+i+" "+r),r.startsWith("shoot:")){await ge(t,r.slice(6));return}if(r.startsWith("assert:")){const a=r.slice(7);if(a.startsWith("text=")){const l=a.slice(5);let g=!1;try{g=await t.execute("return !!(document.body && document.body.innerText.includes(arguments[0]))",l)}catch{}o("FUZZ:ASSERT "+(g?"pass":"fail")+" text="+JSON.stringify(l)+" actor="+i)}else if(a.startsWith("count:")){const l=a.slice(6),g=l.lastIndexOf("="),p=g>=0?l.slice(0,g):l,S=g>=0?parseInt(l.slice(g+1),10):0;let E=-1;try{E=await t.execute(_t,p)}catch{}o("FUZZ:ASSERT "+(E===S?"pass":"fail")+" count "+p+" want="+S+" got="+E+" actor="+i)}else o("FUZZ:ASSERT fail unsupported "+a+" actor="+i);await t.pause(300);return}if(r==="back"){await t.back().catch(()=>{}),await t.pause(400);return}if(r.startsWith("auth:")){o("JOURNEY[a] step: auth-restore unsupported on tauri runner; use login() for "+r),await t.pause(200);return}if(r.startsWith("type:")){const a=r.slice(5),l=a.lastIndexOf("="),g=l>=0?a.slice(0,l):a,p=Te(l>=0?a.slice(l+1):"");p!=null&&String(p).length>0&&Z.add(String(p));let S=!1;try{S=await t.execute(Ot,g,p)}catch{}S||o("FUZZ:MISS "+i+" "+r),await t.pause(900);return}const e=r.slice(4);await ve(t,e)||o("FUZZ:MISS "+i+" "+r),await t.pause(900)}async function At(t){const r=process.env.REPROIT_SCENARIO_BARRIER;let i=process.env.REPROIT_DEVICE;if(!i){try{i=(await(await fetch(r+"/claim")).text()).trim()}catch{i=""}(!i||i.startsWith("ERR"))&&(i="a")}o("JOURNEY claimed role="+i),await t.pause(1500),await z(t);const e=s=>new Promise(a=>setTimeout(a,s));for(let s=0;s<1e5;s++){let a="WAIT";try{a=(await(await fetch(r+"/next?device="+i)).text()).trim()}catch{await e(100);continue}if(a==="DONE")break;if(a==="WAIT"){await e(40);continue}const l=a.startsWith("ACT	")?a.slice(4):a;await Rt(t,l,i),await z(t),await G(t);try{await fetch(r+"/done?device="+i,{method:"POST"})}catch{}}await G(t),o("JOURNEY DONE"),o("All tests passed")}async function Ct(){H||(o("EXCEPTION CAUGHT BY REPROIT"),o("REPROIT_APP (executable path) required"),o("\u2550".repeat(8)),process.exit(0));const t=Ae(),{remote:r}=await import("webdriverio"),i=new URL(Xe),e=await r({hostname:i.hostname,port:Number(i.port||4444),path:i.pathname||"/",capabilities:{"tauri:options":{application:H}}});if(process.env.REPROIT_SCENARIO_BARRIER){o("JOURNEY[a] step: scenario actor="+(process.env.REPROIT_DEVICE||"a")),await At(e),await e.deleteSession();return}o("JOURNEY claimed role=a"),await e.pause(1500);try{await e.setTimeout({script:3e4})}catch{}await z(e),await me(e),await we(e);const s=await tt(e);if(s){const n=`target is behind a ${s.vendor} bot-challenge (${s.marker}); reproit could not reach the app.`;o("EXPLORE:UNSCANNABLE "+JSON.stringify({reason:"bot-wall",vendor:s.vendor,marker:s.marker,diagnostic:n})),o("JOURNEY[a] step: UNSCANNABLE - "+n),o("JOURNEY DONE"),o("All tests passed"),await e.deleteSession();return}const a=new Set,l=new Set,g=Ce(t.seed||0),p=Re();p.length&&o(`JOURNEY[a] step: value_nodes=${p.length}`);const S=new Map,E=new Set;function I(n){if(E.has(n.structuralSig))return n.structuralSig;if(n.vsection){let u=S.get(n.structuralSig);if(u||(u=new Set,S.set(n.structuralSig,u)),u.add(n.vsection),u.size>Ke)return E.add(n.structuralSig),o(`JOURNEY[a] step: value-cap hit (${n.structuralSig})`),n.structuralSig}return n.sig}const L=async()=>{await z(e),await me(e),await we(e),await G(e);const n=await Qe(e,p);if(n.sig=I(n),o("FUZZ:OBS "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},labels:n.labels.slice(0,24),elements:n.tappables.slice(0,24).map(u=>({role:u.role}))})),!a.has(n.sig)){a.add(n.sig),o("EXPLORE:STATE "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},labels:n.labels.slice(0,24),elements:n.tappables.slice(0,24).map(c=>{const O={sel:c.sel,role:c.role,label:c.label};return c.key||(O.nokey=!0),O})}));let u=null,d=null;try{u=await e.execute(he),await e.pause(120),d=await e.execute(he)}catch{}const h=qe(u,d);(h.checks.length||!h.complete)&&o("EXPLORE:OVERFLOW "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},...h})),await it(e,n.sig);let y=null;try{y=await e.execute(rt,[...Z])}catch{}y&&y.length&&o("EXPLORE:CONTENTBUG "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:y}));let R=null;try{const c=await e.execute(de);if(R=c,c&&c.length){await e.pause(300);const O=await e.execute(de);R=Je(c,O||[])}}catch{}R&&R.length&&o("EXPLORE:OCCLUSION "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:R}));let _=null;try{_=await e.execute(Ye)}catch{}_&&_.length&&o("EXPLORE:ZEROCONTRAST "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:_}));let C=null;try{C=await e.execute(De)}catch{}C&&C.length&&o("EXPLORE:SECURITY "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:C}));let b=null;try{b=await e.execute(fe)}catch{}if(b&&b.length){await et(e);try{b=await e.execute(fe)}catch{}}b&&b.length&&o("EXPLORE:BLANKSCREEN "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:b}));let m=null;try{m=await e.execute(()=>{const c=window.__reproit_invariants||[],O=[];for(let F=0;F<c.length;F++){const J=c[F];if(!J||typeof J.test!="function")continue;let T=!0,w="";try{const k=J.test();k&&typeof k=="object"?(T=!!k.ok,w=k.message?String(k.message):""):T=!!k}catch(k){T=!1,w=k&&k.message?String(k.message):String(k)}T||O.push({id:String(J.id),message:w})}return O})}catch{}m&&m.length&&o("EXPLORE:INVARIANT "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:m}));let x=null;try{x=await e.execute(We,[...Z])}catch{}if(x&&x.length&&o("EXPLORE:BROKENASSET "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:x})),!j){let c=[];try{c=await e.executeAsync($e)}catch{c=[]}c&&c.length&&o("EXPLORE:SCROLLROUNDTRIP "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:c}))}}return n};let f=await L(),A=0;const D=t.prefix||null,v=t.replay||null;let te=!1;const ne=D?D.length:0,Se=v?v.length:(t.budget||Ve)+ne,ie=new Set,U=new Set;async function re(n,u){let d=null;try{if(d=await e.getWindowSize(),!d||!(d.width>0&&d.height>0)){d=null;return}const h=await e.execute(He);await e.setWindowSize(Math.round(d.width/2),Math.round(d.height/2)),await e.pause(350);let y=null;try{y=await e.execute(Be,h)}catch{y=null}y&&y.length&&o("EXPLORE:ZOOMREFLOW "+JSON.stringify({sig:n,...u?{route:u}:{},items:y}))}catch{}finally{if(d)try{await e.setWindowSize(d.width,d.height),await e.pause(350)}catch{}}}!v&&!j&&f.anchor&&!U.has(f.anchor)&&(U.add(f.anchor),await re(f.sig,f.anchor));const ae=new Set,oe=new Set;async function ke(n){const u=n.structuralSig;let d=null;try{d=await e.getWindowSize(),!d||!(d.width>0&&d.height>0)?d=null:(await e.setWindowSize(d.height,d.width),await e.pause(350))}catch{}if(d)try{await e.setWindowSize(d.width,d.height),await e.pause(350)}catch{}const h=await L();return n.tappables&&n.tappables.length>0&&h.structuralSig!==u&&o("EXPLORE:ROTATION "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},expected:u,got:h.structuralSig})),h}async function xe(n){const u=n.structuralSig;try{await e.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"hidden"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!0})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pagehide")),window.dispatchEvent(new Event("blur"))}),await e.pause(300),await e.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"visible"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!1})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pageshow")),window.dispatchEvent(new Event("focus"))}),await e.pause(300)}catch{}const d=await L();return n.tappables&&n.tappables.length>0&&d.structuralSig!==u&&o("EXPLORE:BGRESTORE "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},expected:u,got:d.structuralSig})),d}const $=Date.now(),X={pid:null,tried:!1};v&&await ee(e,0,X);const P=t.clip&&typeof t.clip.sel=="string"?t.clip:null,V=!!(B&&v&&P),Ee=V?W(B,"clip.mov"):null;let M=null;if(V){const n=ye(H);n&&(M=xt(n,Ee)),M&&await e.pause(400)}for(let n=0;n<Se&&A<3;n++){if(v&&n>0&&await ee(e,Date.now()-$,X),!v&&!j&&(ae.has(f.sig)||(ae.add(f.sig),f=await ke(f)),oe.has(f.sig)||(oe.add(f.sig),f=await xe(f))),!v&&!ie.has(f.sig)){ie.add(f.sig);let m=[];try{m=await e.executeAsync(Ge)}catch{m=[]}let x=!1;for(const c of m||[])x=!0,o("EXPLORE:CHOICEBUG "+JSON.stringify({from:f.sig,role:c.role,outlier:c.outlier,magnitude:c.magnitude,siblingMedian:c.siblingMedian}));if(x){f=await L();continue}}let u;if(v)u=v[n];else if(D&&n<ne)u=D[n];else if(t.seed){const m=f.tappables.map(w=>w.sel).sort(),x=t.edgeWeights&&t.edgeWeights[f.sig]||{},c=m.map(w=>"tap:"+w).concat(["back"]),O=new Set(t.contractActions||[]),F=c.map(w=>(O.has(w)?4:1)/(1+(x[w]||0))),J=F.reduce((w,k)=>w+k,0);let T=g(1<<20)/(1<<20)*J;u=c[c.length-1];for(let w=0;w<c.length;w++)if(T-=F[w],T<=0){u=c[w];break}}else{u=null;for(const m of f.tappables)if(!l.has(f.sig+"|"+m.sel)){u="tap:"+m.sel;break}u=u||"back"}if(v&&!te&&process.env.REPROIT_INSPECT==="1"){const m=f.tappables.find(c=>`tap:${c.sel}`===u);te=await ze({action:u,step:n+1,total:v.length,target:m?.label||m?.sel||null})==="continue"}if(o("FUZZ:ACT "+u),u.startsWith("shoot:")){await ge(e,u.slice(6));continue}if(u==="back"){const m=f.sig,x=f.content;await e.back().catch(()=>{}),await e.pause(600);const c=await L();c.sig!==m?(o("EXPLORE:EDGE "+JSON.stringify({from:m,action:"back",to:c.sig})),A=0):c.content!==x?A=0:A++,f=c;continue}const d=u.slice(4);l.add(f.sig+"|"+d);const h=f.sig,y=f.content,R=f.anchor;try{await e.execute(ot)}catch{}try{await e.execute(ft)}catch{}try{await e.execute(Ue)}catch{}if(!await ve(e,d)){o("FUZZ:MISS "+u),A++;continue}await e.pause(700);const _=await mt(e);_&&o("EXPLORE:"+(_.kind==="hang"?"HANG":"JANK")+" "+JSON.stringify({from:h,action:"tap:"+d,bucket:_.bucket,count:_.count}));let C=!1;try{C=await e.execute(Me)}catch{}const b=await L();C&&(b.sig===h||b.anchor&&b.anchor===R)&&o("EXPLORE:FOCUSLOSS "+JSON.stringify({from:h,action:"tap:"+d})),b.sig!==h?(o("EXPLORE:EDGE "+JSON.stringify({from:h,action:"tap:"+d,to:b.sig})),A=0,!v&&!j&&b.anchor&&!U.has(b.anchor)&&(U.add(b.anchor),await re(b.sig,b.anchor))):b.content!==y&&(A=0),f=b}if(v&&await ee(e,Date.now()-$,X),await G(e),V){await e.pause(300);const n=M?await kt(e,P.sel):null;let u=!1;if(n){const d=Math.max(0,(Date.now()-$)/1e3-.2),h={videoW:n.videoW,videoH:n.videoH,boxes:[{x:n.x,y:n.y,w:n.w,h:n.h,tStart:d,tEnd:1e9,label:P.label||P.oracle||"finding",color:"red"}]};try{K(B,{recursive:!0}),se(W(B,"box-spec.json"),JSON.stringify(h)),u=!0}catch{u=!1}await e.pause(900)}await Et(M),o("FINDING:BOXED "+JSON.stringify({oracle:P.oracle||null,sel:P.sel,drew:u}))}o(`JOURNEY[a] step: explored ${a.size} states`),o("JOURNEY DONE"),o("All tests passed"),await e.deleteSession()}const Tt=process.argv[1]&&import.meta.url===new URL(`file://${process.argv[1]}`).href;Tt&&Ct().catch(t=>{o("EXCEPTION CAUGHT BY TAURI RUNNER"),o(String(t&&t.stack?t.stack:t)),o("Some tests failed"),process.exit(0)});export{be as classifyFrameIntervals,le as descriptorOf,ce as signatureOf,Oe as valueClass};
