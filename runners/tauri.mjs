import{readFileSync as fe,existsSync as he,mkdirSync as Z,writeFileSync as ge}from"node:fs";import{resolve as We,join as W}from"node:path";import{execFileSync as He,spawn as me}from"node:child_process";import{platform as K}from"node:os";import{CHOICE_ANOMALY_IN_PAGE_SRC as Be,CHOICE_OUTLIER_RATIO as je,CHOICE_MIN_MAGNITUDE as ze,CHOICE_ROLES as qe}from"./web/choice-oracle.mjs";import{occlusionScan as pe,confirmOcclusions as $e,securityScan as Ye,focusLossArm as Ge,focusLossCheck as Ve,blankScreenScan as be,brokenAssetScan as Xe,zoomTappableKeys as Ze,zoomReflowScan as Ke,scrollRoundTripScan as Qe}from"./web/hygiene-oracles.mjs";import{layoutOverflowScan as we,confirmLayoutOverflow as et}from"./web/overflow-oracle.mjs";import{zeroContrastScan as tt}from"./web/zero-contrast-oracle.mjs";import{inspectPlatformStep as nt}from"./inspect-control.mjs";const it=`
  var __reproitChoiceFn = ${Be};
  var __reproitDone = arguments[arguments.length - 1];
  __reproitChoiceFn({
    settleMs: 600,
    ratio: ${je},
    minMag: ${ze},
    choiceRoles: ${JSON.stringify(qe)},
  }).then(function (findings) { __reproitDone(findings || []); })
    .catch(function () { __reproitDone([]); });
`,rt=`
  var __srtFn = ${Qe.toString()};
  var __srtDone = arguments[arguments.length - 1];
  Promise.resolve(__srtFn()).then(function (items) { __srtDone(items || []); })
    .catch(function () { __srtDone([]); });
`,H=process.env.REPROIT_APP,ot=process.env.REPROIT_WEBDRIVER_URL||"http://127.0.0.1:4444",B=process.env.REPROIT_VIDEO_DIR||void 0,j=process.env.REPROIT_PROBE==="1",at=36,un=40,st=8;function s(e){process.stdout.write(e+`
`)}function ct(){const e=process.env.REPROIT_FUZZ_CONFIG;if(!e)return{};try{return JSON.parse(fe(e,"utf8"))}catch{return{}}}async function ye(e,n){const i=process.env.REPROIT_SHOTS_DIR;if(i)try{Z(i,{recursive:!0});const t=await e.takeScreenshot();ge(W(i,n+".png"),Buffer.from(t,"base64"))}catch{}s("SHOOT:"+n)}function lt(){let e=(process.env.REPROIT_CONFIG||"").trim();if(!e){const i=We(process.cwd(),"reproit.yaml");he(i)&&(e=i)}if(!e||!he(e))return[];let n="";try{n=fe(e,"utf8")}catch{return[]}return ut(n)}function ut(e){const n=e.split(/\r?\n/),i=[],t=a=>{let o=a.trim();const c=o.indexOf("#");return c>=0&&(o=o.slice(0,c).trim()),(o.startsWith('"')&&o.endsWith('"')||o.startsWith("'")&&o.endsWith("'"))&&(o=o.slice(1,-1)),o.trim()};for(let a=0;a<n.length;a++){const o=n[a].match(/^(\s*)value_nodes\s*:(.*)$/);if(!o)continue;const c=o[1].length,g=o[2].trim();if(g.startsWith("[")){const h=g.replace(/^\[/,"").replace(/\].*$/,"");for(const b of h.split(",")){const v=t(b);v&&i.push(v)}return i}for(let h=a+1;h<n.length;h++){const b=n[h];if(!b.trim()||b.trim().startsWith("#"))continue;if(b.length-b.replace(/^\s*/,"").length<=c)break;const _=b.trim();if(!_.startsWith("-"))break;const R=t(_.slice(1));R&&i.push(R)}return i}return i}function dt(e){let n=e>>>0||1;return i=>(n^=n<<13,n>>>=0,n^=n>>17,n^=n<<5,n>>>=0,(n&2147483647)%i)}const Q=new TextEncoder;function ve(e){const n=Q.encode(e);let i=2166136261;for(let t=0;t<n.length;t++)i^=n[t],i=Math.imul(i,16777619)>>>0;return(i>>>0).toString(16).padStart(8,"0")}function ft(e,n){const i=Q.encode(e),t=Q.encode(n),a=Math.min(i.length,t.length);for(let o=0;o<a;o++)if(i[o]!==t[o])return i[o]<t[o]?-1:1;return i.length===t.length?0:i.length<t.length?-1:1}const ht={screen:1,header:1,text:1,button:1,link:1,textfield:1,image:1,icon:1,list:1,listitem:1,tab:1,switch:1,checkbox:1,radio:1,slider:1,menu:1,menuitem:1,dialog:1,group:1,node:1},gt={toast:1,snackbar:1,spinner:1,progress:1,tooltip:1,badge:1},mt={textfield:1,status:1,log:1,progressbar:1,meter:1,timer:1,output:1};function ee(e){return ht[e]?e:"node"}function te(e){return!!e.transient||!!gt[e.role]}function Se(e){return e.value!=null&&(!!mt[e.role]||!!e.value_node)}function ke(e){if(te(e))return null;const n=[],i=e.children||[];for(const t of i){const a=ke(t);a&&n.push(a)}return{role:ee(e.role),type:e.type!=null?e.type:null,icon:e.icon!=null?e.icon:null,id:e.id!=null?e.id:null,children:n}}function xe(e){let n=e.role;return e.type!=null&&(n+=":"+e.type),e.icon!=null&&(n+="#"+e.icon),e.id!=null&&(n+="@"+e.id),n}function Ee(e){const n=[];return(function i(t,a){n.push(a+":"+xe(t));for(const o of t.children)i(o,a+1)})(e,0),n.join(";")}function Oe(e,n,i,t){let a=n+":"+xe(e);i&&(a+="*"),t.push(a),pt(e.children,n+1,t)}function pt(e,n,i){let t=0;for(;t<e.length;){const a=Ee(e[t]);let o=t+1;for(;o<e.length&&Ee(e[o])===a;)o++;Oe(e[t],n,o-t>=2,i),t=o}}function bt(e){let n=0;const i=e.length;n<i&&(e.charCodeAt(n)===43||e.charCodeAt(n)===45)&&n++;const t=n;for(;n<i&&e.charCodeAt(n)>=48&&e.charCodeAt(n)<=57;)n++;if(n===t)return!1;if(n<i&&e.charCodeAt(n)===46){n++;const a=n;for(;n<i&&e.charCodeAt(n)>=48&&e.charCodeAt(n)<=57;)n++;if(n===a)return!1}return n===i}function ne(e){const n=(e==null?"":String(e)).replace(/^\s+|\s+$/g,"");if(n.length===0)return"EMPTY";if(bt(n)){const i=parseFloat(n),t=Math.abs(i);return i===0?"ZERO":i<0?"NEG":t<10?"POS1":t<100?"POS2":t<1e3?"POS3":"POSL"}return"NONEMPTY"}function _e(e,n){return e.id!=null?"key:"+e.id:"role:"+ee(e.role)+"#"+n}function wt(e,n){te(e)||(Se(e)&&n.push([_e(e,0),ne(e.value)]),Re(e,n))}function Re(e,n){const i={},t=e.children||[];for(const a of t){if(te(a))continue;const o=ee(a.role),c=i[o]||0;i[o]=c+1,Se(a)&&n.push([_e(a,c),ne(a.value)]),Re(a,n)}}function yt(e){const n=[];return wt(e,n),n.length===0?"":(n.sort((i,t)=>ft(i[0],t[0])),`
V:`+n.map(i=>i[0]+"="+i[1]).join(";"))}function ie(e,n){const i=[],t=ke(n);return t&&Oe(t,0,!1,i),"A:"+(e??"")+`
`+i.join(";")+yt(n)}function Ae(e,n){return ve(ie(e,n))}import{snapshotJs as vt}from"./tauri-snapshot.mjs";async function St(e,n){const i=await e.execute(vt(n||[]));i.sig=Ae(i.anchor,i.tree);const t=ie(i.anchor,i.tree),a=t.indexOf(`
V:`);return i.vsection=a>=0?t.slice(a+3):"",i.structuralSig=a>=0?ve(t.slice(0,a)):i.sig,i.content=i.sig+"|"+i.textNodes.map(o=>o[0]+"="+o[1]).join(";"),i}async function kt(e){try{await e.executeAsync(n=>{const i=()=>new Promise(t=>requestAnimationFrame(()=>requestAnimationFrame(t)));(async()=>{await new Promise(t=>{let a=null,o=null;const c=()=>{if(o&&clearTimeout(o),h&&clearTimeout(h),a)try{a.disconnect()}catch{}t()},g=()=>{o&&clearTimeout(o),o=setTimeout(c,400)},h=setTimeout(c,1800);try{a=new MutationObserver(g),a.observe(document.documentElement,{subtree:!0,childList:!0,attributes:!0,characterData:!0})}catch{}g()});try{const t=(document.getAnimations?document.getAnimations():[]).filter(a=>a.playState==="running");await Promise.race([Promise.allSettled(t.map(a=>a.finished)),new Promise(a=>setTimeout(a,800))])}catch{}await i(),n()})()})}catch{}}async function xt(e){try{return await e.execute(()=>{const n=(document.title||"").toLowerCase(),i=(document.body&&document.body.innerText||"").toLowerCase(),t=a=>a.test(n)||a.test(i);return document.querySelector('#challenge-running, #cf-challenge-running, #challenge-form, .cf-turnstile, [id^="cf-chl"], script[src*="challenge-platform"], iframe[src*="challenges.cloudflare.com"]')?{vendor:"Cloudflare",marker:"challenge-platform"}:t(/just a moment/)||t(/checking your browser before/)||t(/performing (a )?security verification/)||t(/enable javascript and cookies to continue/)?{vendor:"Cloudflare",marker:"interstitial"}:t(/attention required/)&&t(/cloudflare/)?{vendor:"Cloudflare",marker:"attention-required"}:document.querySelector('#px-captcha, .px-block, [class*="perimeterx"]')?{vendor:"PerimeterX",marker:"px-captcha"}:/ray id:/.test(i)&&i.length<1200?{vendor:"Cloudflare",marker:"ray-id-block"}:null})}catch{return null}}const Et=`
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
`;async function Ot(e,n){let i;try{i=await e.execute(Et)}catch{return}i&&s("EXPLORE:GROUNDTRUTH "+JSON.stringify({sig:n,focusTrap:!!i.focusTrap,elements:i.elements||[]}))}const Ce=JSON.stringify("header,nav,main,footer,aside,[role=banner],[role=navigation],[role=main],[role=contentinfo],[role=complementary],[role=region],[role=search],[role=listbox],[role=list],[role=tablist],[role=toolbar],[role=dialog],[id]"),fn=`
  const sel = ${Ce};
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
      key: keyOf(el), node: el,
      text: (el.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
`,hn=`
  const sel = ${Ce};
  const old = window.__reproitAnchors;
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
    if (cur.has(k)) { dup.add(k); continue; }
    cur.set(k, el);
  }
  const churned = [];
  for (const a of old) {
    if (dup.has(a.key)) continue;
    const now = cur.get(a.key);
    if (!now) continue;
    if (now === a.node) continue;
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x && Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w && Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key);
  }
  window.__reproitAnchors = null;
  return churned;
`,_t=`
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
`,z=200,q=2e3,Rt=`
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
`,At="try { window.__reproitLongTasks = []; } catch (_) {} return true;",Ct=`
  const t = window.__reproitLongTasks || [];
  window.__reproitLongTasks = [];
  return t;
`;async function Te(e){try{await e.execute(Rt)}catch{}}async function Tt(e){let n=[];try{n=await e.execute(Ct)}catch{return null}if(!n||!n.length)return null;const i=Math.max(...n);return i>=q?{kind:"hang",bucket:q,count:n.length}:i>=z?{kind:"jank",bucket:z,count:n.length}:null}const Ne=100,Nt=2,It=350;function Ie(e){if(!e||!e.length)return null;let n=0;for(const o of e)o>=q&&n++;if(n>0)return{kind:"hang",bucket:q,count:n};let i=0,t=0;const a=e.length;for(;t<a;){if(e[t]<Ne){t++;continue}let o=t,c=0,g=0;for(;o<a&&e[o]>=Ne;)c+=e[o],e[o]>g&&(g=e[o]),o++;const h=o-t,b=g>=It,v=h>=Nt&&c>=z;(b||v)&&i++,t=o}return i>0?{kind:"jank",bucket:z,count:i}:null}const Lt=`
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
`,Pt="try { window.__reproitFrameIntervals = []; } catch (_) {} return true;",Ft=`
  const t = window.__reproitFrameIntervals || [];
  window.__reproitFrameIntervals = [];
  return t;
`;async function Le(e){try{await e.execute(Lt)}catch{}}async function Jt(e){let n=[];try{n=await e.execute(Ft)}catch{return null}return Ie(n)}async function Mt(e){const n=await Tt(e);return n||Jt(e)}const Dt=`
  try {
    if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
      return performance.memory.usedJSHeapSize;
    }
  } catch (_) {}
  return null;
`;function L(e,n){try{const i=He(e,n,{encoding:"utf8",stdio:["ignore","pipe","ignore"],timeout:5e3});return i==null?null:String(i)}catch{return null}}function Pe(e){if(!e)return null;if(K()==="win32"){const a=e.split(/[\\/]/).pop()||e,o=L("tasklist",["/FI","IMAGENAME eq "+a,"/FO","CSV","/NH"]);if(o==null)return null;const c=[];for(const g of o.split(/\r?\n/)){const h=g.match(/^"[^"]*","(\d+)"/);h&&c.push(parseInt(h[1],10))}return c.length!==1||!Number.isFinite(c[0])||c[0]<=0?null:c[0]}const i=L("ps",["-axww","-o","pid=,comm="]);if(i==null)return null;const t=[];for(const a of i.split(`
`)){const o=a.match(/^\s*(\d+)\s+(.*)$/);o&&o[2].trim()===e&&t.push(parseInt(o[1],10))}return t.length!==1||!Number.isFinite(t[0])||t[0]<=0?null:t[0]}function Ut(e){if(!(e>0))return null;if(K()==="win32"){const t=L("tasklist",["/FI","PID eq "+e,"/FO","CSV","/NH"]);if(t==null)return null;const a=t.match(/"([\d.,]+)\s*K"/);if(!a)return null;const o=parseInt(a[1].replace(/[.,]/g,""),10);return!Number.isFinite(o)||o<=0?null:o*1024}const n=L("ps",["-o","rss=","-p",String(e)]);if(n==null)return null;const i=parseInt(n.trim(),10);return!Number.isFinite(i)||i<=0?null:i*1024}async function re(e,n,i){if(i&&(i.tried||(i.tried=!0,i.pid=Pe(H)),i.pid>0)){const a=Ut(i.pid);if(a!=null){s("MEMORY:SAMPLE "+JSON.stringify({t_ms:n,heap_used:a}));return}}let t=null;try{t=await e.execute(Dt)}catch{t=null}t!=null&&s("MEMORY:SAMPLE "+JSON.stringify({t_ms:n,heap_used:t}))}const Wt=`
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
`;async function $(e){try{await e.execute(Wt)}catch{}}function Ht(e){s("EXCEPTION CAUGHT BY TAURI WEBVIEW"),s("The following error was thrown:"),s(String(e&&e.message?e.message:e));const n=e&&e.stack?String(e.stack):"";for(const i of n.split(`
`).slice(0,8))i&&s(i);s("\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550")}async function Y(e){let n=[];try{n=await e.execute(()=>{const i=window.__reproit_errors||[];return window.__reproit_errors=[],i})}catch{return}if(Array.isArray(n))for(const i of n)Ht(i)}const Bt=`
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
`;async function Fe(e,n){try{return!!await e.execute(Bt,n)}catch{return!1}}const jt=`
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
`;async function zt(e,n){try{return await e.executeAsync(jt,n)}catch{return null}}function qt(e,n){try{Z(W(n,".."),{recursive:!0})}catch{}const i=K();try{if(i==="linux"){const t=process.env.DISPLAY||":0";let a=(L("xdotool",["search","--pid",String(e),"--onlyvisible"])||"").trim().split(/\s+/).filter(Boolean).pop();if(!a)return null;const o=L("xdotool",["getwindowgeometry","--shell",a])||"",c={};for(const v of o.split(`
`)){const _=v.match(/^(\w+)=(-?\d+)/);_&&(c[_[1]]=parseInt(_[2],10))}if(!(c.WIDTH>0&&c.HEIGHT>0))return null;const g=c.WIDTH-c.WIDTH%2,h=c.HEIGHT-c.HEIGHT%2;return me("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","x11grab","-framerate","15","-video_size",`${g}x${h}`,"-i",`${t}+${c.X||0},${c.Y||0}`,"-c:v","libx264","-pix_fmt","yuv420p",n],{stdio:["pipe","ignore","ignore"]})}if(i==="win32"){const a=(L("tasklist",["/FI","PID eq "+e,"/FO","CSV","/NH","/V"])||"").match(/^"[^"]*","\d+","[^"]*","[^"]*","[^"]*","([^"]*)"/),o=a&&a[1]&&a[1]!=="N/A"?a[1]:null;return o?me("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","gdigrab","-framerate","15","-i","title="+o,"-c:v","libx264","-pix_fmt","yuv420p",n],{stdio:["pipe","ignore","ignore"]}):null}if(i==="darwin")return null}catch{}return null}async function $t(e){!e||e.exitCode!==null||await new Promise(n=>{let i=!1;const t=()=>{i||(i=!0,n())};e.once("exit",t);try{e.stdin&&e.stdin.writable&&e.stdin.write("q")}catch{}try{e.kill("SIGINT")}catch{}setTimeout(t,4e3)})}function Yt(e){return String(e).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g,(n,i)=>process.env[i]||"")}const oe=new Set,Gt=`
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
`,Vt=`
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
`;async function Xt(e,n,i){if(s("FUZZ:ACT "+i+" "+n),n.startsWith("shoot:")){await ye(e,n.slice(6));return}if(n.startsWith("assert:")){const o=n.slice(7);if(o.startsWith("text=")){const c=o.slice(5);let g=!1;try{g=await e.execute("return !!(document.body && document.body.innerText.includes(arguments[0]))",c)}catch{}s("FUZZ:ASSERT "+(g?"pass":"fail")+" text="+JSON.stringify(c)+" actor="+i)}else if(o.startsWith("count:")){const c=o.slice(6),g=c.lastIndexOf("="),h=g>=0?c.slice(0,g):c,b=g>=0?parseInt(c.slice(g+1),10):0;let v=-1;try{v=await e.execute(Vt,h)}catch{}s("FUZZ:ASSERT "+(v===b?"pass":"fail")+" count "+h+" want="+b+" got="+v+" actor="+i)}else s("FUZZ:ASSERT fail unsupported "+o+" actor="+i);await e.pause(300);return}if(n==="back"){await e.back().catch(()=>{}),await e.pause(400);return}if(n.startsWith("auth:")){s("JOURNEY[a] step: auth-restore unsupported on tauri runner; use login() for "+n),await e.pause(200);return}if(n.startsWith("type:")){const o=n.slice(5),c=o.lastIndexOf("="),g=c>=0?o.slice(0,c):o,h=Yt(c>=0?o.slice(c+1):"");h!=null&&String(h).length>0&&oe.add(String(h));let b=!1;try{b=await e.execute(Gt,g,h)}catch{}b||s("FUZZ:MISS "+i+" "+n),await e.pause(900);return}const t=n.slice(4);await Fe(e,t)||s("FUZZ:MISS "+i+" "+n),await e.pause(900)}async function Zt(e){const n=process.env.REPROIT_SCENARIO_BARRIER;let i=process.env.REPROIT_DEVICE;if(!i){try{i=(await(await fetch(n+"/claim")).text()).trim()}catch{i=""}(!i||i.startsWith("ERR"))&&(i="a")}s("JOURNEY claimed role="+i),await e.pause(1500),await $(e);const t=a=>new Promise(o=>setTimeout(o,a));for(let a=0;a<1e5;a++){let o="WAIT";try{o=(await(await fetch(n+"/next?device="+i)).text()).trim()}catch{await t(100);continue}if(o==="DONE")break;if(o==="WAIT"){await t(40);continue}const c=o.startsWith("ACT	")?o.slice(4):o;await Xt(e,c,i),await $(e),await Y(e);try{await fetch(n+"/done?device="+i,{method:"POST"})}catch{}}await Y(e),s("JOURNEY DONE"),s("All tests passed")}async function Kt(){H||(s("EXCEPTION CAUGHT BY REPROIT"),s("REPROIT_APP (executable path) required"),s("\u2550".repeat(8)),process.exit(0));const e=ct(),{remote:n}=await import("webdriverio"),i=new URL(ot),t=await n({hostname:i.hostname,port:Number(i.port||4444),path:i.pathname||"/",capabilities:{"tauri:options":{application:H}}});if(process.env.REPROIT_SCENARIO_BARRIER){s("JOURNEY[a] step: scenario actor="+(process.env.REPROIT_DEVICE||"a")),await Zt(t),await t.deleteSession();return}s("JOURNEY claimed role=a"),await t.pause(1500);try{await t.setTimeout({script:3e4})}catch{}await $(t),await Te(t),await Le(t);const a=await xt(t);if(a){const r=`target is behind a ${a.vendor} bot-challenge (${a.marker}); reproit could not reach the app.`;s("EXPLORE:UNSCANNABLE "+JSON.stringify({reason:"bot-wall",vendor:a.vendor,marker:a.marker,diagnostic:r})),s("JOURNEY[a] step: UNSCANNABLE - "+r),s("JOURNEY DONE"),s("All tests passed"),await t.deleteSession();return}const o=new Set,c=new Set,g=dt(e.seed||0),h=lt();h.length&&s(`JOURNEY[a] step: value_nodes=${h.length}`);const b=new Map,v=new Set;function _(r){if(v.has(r.structuralSig))return r.structuralSig;if(r.vsection){let u=b.get(r.structuralSig);if(u||(u=new Set,b.set(r.structuralSig,u)),u.add(r.vsection),u.size>st)return v.add(r.structuralSig),s(`JOURNEY[a] step: value-cap hit (${r.structuralSig})`),r.structuralSig}return r.sig}const R=async()=>{await $(t),await Te(t),await Le(t),await Y(t);const r=await St(t,h);if(r.sig=_(r),s("FUZZ:OBS "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},labels:r.labels.slice(0,24),elements:r.tappables.slice(0,24).map(u=>({role:u.role}))})),!o.has(r.sig)){o.add(r.sig),s("EXPLORE:STATE "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},labels:r.labels.slice(0,24),elements:r.tappables.slice(0,24).map(l=>{const O={sel:l.sel,role:l.role,label:l.label};return l.key||(O.nokey=!0),O})}));let u=null,d=null;try{u=await t.execute(we),await t.pause(120),d=await t.execute(we)}catch{}const m=et(u,d);(m.checks.length||!m.complete)&&s("EXPLORE:OVERFLOW "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},...m})),await Ot(t,r.sig);let S=null;try{S=await t.execute(_t,[...oe])}catch{}S&&S.length&&s("EXPLORE:CONTENTBUG "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:S}));let C=null;try{const l=await t.execute(pe);if(C=l,l&&l.length){await t.pause(300);const O=await t.execute(pe);C=$e(l,O||[])}}catch{}C&&C.length&&s("EXPLORE:OCCLUSION "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:C}));let A=null;try{A=await t.execute(tt)}catch{}A&&A.length&&s("EXPLORE:ZEROCONTRAST "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:A}));let N=null;try{N=await t.execute(Ye)}catch{}N&&N.length&&s("EXPLORE:SECURITY "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:N}));let w=null;try{w=await t.execute(be)}catch{}if(w&&w.length){await kt(t);try{w=await t.execute(be)}catch{}}w&&w.length&&s("EXPLORE:BLANKSCREEN "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:w}));let p=null;try{p=await t.execute(()=>{const l=window.__reproit_invariants||[],O=[];for(let F=0;F<l.length;F++){const J=l[F];if(!J||typeof J.test!="function")continue;let I=!0,y="";try{const x=J.test();x&&typeof x=="object"?(I=!!x.ok,y=x.message?String(x.message):""):I=!!x}catch(x){I=!1,y=x&&x.message?String(x.message):String(x)}I||O.push({id:String(J.id),message:y})}return O})}catch{}p&&p.length&&s("EXPLORE:INVARIANT "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:p}));let E=null;try{E=await t.execute(Xe,[...oe])}catch{}if(E&&E.length&&s("EXPLORE:BROKENASSET "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:E})),!j){let l=[];try{l=await t.executeAsync(rt)}catch{l=[]}l&&l.length&&s("EXPLORE:SCROLLROUNDTRIP "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},items:l}))}}return r};let f=await R(),T=0;const M=e.prefix||null,k=e.replay||null;let ae=!1;const se=M?M.length:0,Je=k?k.length:(e.budget||at)+se,ce=new Set,D=new Set;async function le(r,u){let d=null;try{if(d=await t.getWindowSize(),!d||!(d.width>0&&d.height>0)){d=null;return}const m=await t.execute(Ze);await t.setWindowSize(Math.round(d.width/2),Math.round(d.height/2)),await t.pause(350);let S=null;try{S=await t.execute(Ke,m)}catch{S=null}S&&S.length&&s("EXPLORE:ZOOMREFLOW "+JSON.stringify({sig:r,...u?{route:u}:{},items:S}))}catch{}finally{if(d)try{await t.setWindowSize(d.width,d.height),await t.pause(350)}catch{}}}!k&&!j&&f.anchor&&!D.has(f.anchor)&&(D.add(f.anchor),await le(f.sig,f.anchor));const ue=new Set,de=new Set;async function Me(r){const u=r.structuralSig;let d=null;try{d=await t.getWindowSize(),!d||!(d.width>0&&d.height>0)?d=null:(await t.setWindowSize(d.height,d.width),await t.pause(350))}catch{}if(d)try{await t.setWindowSize(d.width,d.height),await t.pause(350)}catch{}const m=await R();return r.tappables&&r.tappables.length>0&&m.structuralSig!==u&&s("EXPLORE:ROTATION "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},expected:u,got:m.structuralSig})),m}async function De(r){const u=r.structuralSig;try{await t.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"hidden"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!0})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pagehide")),window.dispatchEvent(new Event("blur"))}),await t.pause(300),await t.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"visible"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!1})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pageshow")),window.dispatchEvent(new Event("focus"))}),await t.pause(300)}catch{}const d=await R();return r.tappables&&r.tappables.length>0&&d.structuralSig!==u&&s("EXPLORE:BGRESTORE "+JSON.stringify({sig:r.sig,...r.anchor?{route:r.anchor}:{},expected:u,got:d.structuralSig})),d}const G=Date.now(),V={pid:null,tried:!1};k&&await re(t,0,V);const P=e.clip&&typeof e.clip.sel=="string"?e.clip:null,X=!!(B&&k&&P),Ue=X?W(B,"clip.mov"):null;let U=null;if(X){const r=Pe(H);r&&(U=qt(r,Ue)),U&&await t.pause(400)}for(let r=0;r<Je&&T<3;r++){if(k&&r>0&&await re(t,Date.now()-G,V),!k&&!j&&(ue.has(f.sig)||(ue.add(f.sig),f=await Me(f)),de.has(f.sig)||(de.add(f.sig),f=await De(f))),!k&&!ce.has(f.sig)){ce.add(f.sig);let p=[];try{p=await t.executeAsync(it)}catch{p=[]}let E=!1;for(const l of p||[])E=!0,s("EXPLORE:CHOICEBUG "+JSON.stringify({from:f.sig,role:l.role,outlier:l.outlier,magnitude:l.magnitude,siblingMedian:l.siblingMedian}));if(E){f=await R();continue}}let u;if(k)u=k[r];else if(M&&r<se)u=M[r];else if(e.seed){const p=f.tappables.map(y=>y.sel).sort(),E=e.edgeWeights&&e.edgeWeights[f.sig]||{},l=p.map(y=>"tap:"+y).concat(["back"]),O=new Set(e.contractActions||[]),F=l.map(y=>(O.has(y)?4:1)/(1+(E[y]||0))),J=F.reduce((y,x)=>y+x,0);let I=g(1<<20)/(1<<20)*J;u=l[l.length-1];for(let y=0;y<l.length;y++)if(I-=F[y],I<=0){u=l[y];break}}else{u=null;for(const p of f.tappables)if(!c.has(f.sig+"|"+p.sel)){u="tap:"+p.sel;break}u=u||"back"}if(k&&!ae&&process.env.REPROIT_INSPECT==="1"){const p=f.tappables.find(l=>`tap:${l.sel}`===u);ae=await nt({action:u,step:r+1,total:k.length,target:p?.label||p?.sel||null})==="continue"}if(s("FUZZ:ACT "+u),u.startsWith("shoot:")){await ye(t,u.slice(6));continue}if(u==="back"){const p=f.sig,E=f.content;await t.back().catch(()=>{}),await t.pause(600);const l=await R();l.sig!==p?(s("EXPLORE:EDGE "+JSON.stringify({from:p,action:"back",to:l.sig})),T=0):l.content!==E?T=0:T++,f=l;continue}const d=u.slice(4);c.add(f.sig+"|"+d);const m=f.sig,S=f.content,C=f.anchor;try{await t.execute(At)}catch{}try{await t.execute(Pt)}catch{}try{await t.execute(Ge)}catch{}if(!await Fe(t,d)){s("FUZZ:MISS "+u),T++;continue}await t.pause(700);const A=await Mt(t);A&&s("EXPLORE:"+(A.kind==="hang"?"HANG":"JANK")+" "+JSON.stringify({from:m,action:"tap:"+d,bucket:A.bucket,count:A.count}));let N=!1;try{N=await t.execute(Ve)}catch{}const w=await R();N&&(w.sig===m||w.anchor&&w.anchor===C)&&s("EXPLORE:FOCUSLOSS "+JSON.stringify({from:m,action:"tap:"+d})),w.sig!==m?(s("EXPLORE:EDGE "+JSON.stringify({from:m,action:"tap:"+d,to:w.sig})),T=0,!k&&!j&&w.anchor&&!D.has(w.anchor)&&(D.add(w.anchor),await le(w.sig,w.anchor))):w.content!==S&&(T=0),f=w}if(k&&await re(t,Date.now()-G,V),await Y(t),X){await t.pause(300);const r=U?await zt(t,P.sel):null;let u=!1;if(r){const d=Math.max(0,(Date.now()-G)/1e3-.2),m={videoW:r.videoW,videoH:r.videoH,boxes:[{x:r.x,y:r.y,w:r.w,h:r.h,tStart:d,tEnd:1e9,label:P.label||P.oracle||"finding",color:"red"}]};try{Z(B,{recursive:!0}),ge(W(B,"box-spec.json"),JSON.stringify(m)),u=!0}catch{u=!1}await t.pause(900)}await $t(U),s("FINDING:BOXED "+JSON.stringify({oracle:P.oracle||null,sel:P.sel,drew:u}))}s(`JOURNEY[a] step: explored ${o.size} states`),s("JOURNEY DONE"),s("All tests passed"),await t.deleteSession()}const Qt=process.argv[1]&&import.meta.url===new URL(`file://${process.argv[1]}`).href;Qt&&Kt().catch(e=>{s("EXCEPTION CAUGHT BY TAURI RUNNER"),s(String(e&&e.stack?e.stack:e)),s("Some tests failed"),process.exit(0)});export{Ie as classifyFrameIntervals,ie as descriptorOf,Ae as signatureOf,ne as valueClass};
