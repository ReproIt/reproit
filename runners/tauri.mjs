import{readFileSync as We,existsSync as He,mkdirSync as ne,mkdtempSync as Be,rmSync as ie,writeFileSync as pe}from"node:fs";import{join as W}from"node:path";import{signatureOf as we,descriptorOf as ye,valueClass as Xe,fnv1a as Ye,loadValueNodes as Ge}from"./shared/signature.mjs";import{loadFuzz as qe,rng as Ke,INJECTED_VALUES as ae,expandEnv as Ve}from"./shared/fuzz.mjs";import{RESOLVE_STRUCTURAL_TARGET_SRC as be,DETECT_CONTENT_BUGS_SRC as $e}from"./shared/dom-walk.mjs";import{execFileSync as ze,spawn as Ee}from"node:child_process";import{platform as re,tmpdir as je}from"node:os";import{classifyVideoFile as Ze}from"./shared/video-flicker.mjs";import{CHOICE_ANOMALY_IN_PAGE_SRC as Qe,CHOICE_OUTLIER_RATIO as et,CHOICE_MIN_MAGNITUDE as tt,CHOICE_ROLES as nt}from"./web/choice-oracle.mjs";import{occlusionScan as Se,confirmOcclusions as it,indicatorRelationshipScan as ve,confirmRelationshipViolations as at,securityScan as rt,focusLossArm as ot,focusLossCheck as st,blankScreenScan as _e,brokenAssetScan as ct,zoomTappableKeys as lt,zoomReflowScan as ut,scrollRoundTripScan as dt}from"./web/hygiene-oracles.mjs";import{layoutOverflowScan as ke,confirmLayoutOverflow as ft}from"./web/overflow-oracle.mjs";import{zeroContrastScan as ht}from"./web/zero-contrast-oracle.mjs";import{deadInputScrollCandidates as gt,deadInputPointOwner as Oe,deadInputArm as oe,deadInputRead as q,classifyWheelProbe as xe,classifyKeyProbe as mt,DEAD_INPUT_MAX_SCROLLABLES as pt}from"./web/dead-input-oracle.mjs";import{inspectPlatformStep as wt}from"./inspect-control.mjs";const yt=`
  var __reproitChoiceFn = ${Qe};
  var __reproitDone = arguments[arguments.length - 1];
  __reproitChoiceFn({
    settleMs: 600,
    ratio: ${et},
    minMag: ${tt},
    choiceRoles: ${JSON.stringify(nt)},
  }).then(function (findings) { __reproitDone(findings || []); })
    .catch(function () { __reproitDone([]); });
`,bt=`
  var __srtFn = ${dt.toString()};
  var __srtDone = arguments[arguments.length - 1];
  Promise.resolve(__srtFn()).then(function (items) { __srtDone(items || []); })
    .catch(function () { __srtDone([]); });
`;async function Et(e){const a=[],i=await e.execute(gt).catch(()=>[]);for(const r of(i||[]).slice(0,pt)){const o=await e.execute(Oe,r).catch(()=>null);if(!o||["gone","visible-interceptor","dialog"].includes(o.owner))continue;await e.execute(oe,r).catch(()=>!1);try{await e.action("wheel").scroll({x:Math.round(r.x),y:Math.round(r.y),deltaX:0,deltaY:120}).perform()}catch{await e.execute(q,r).catch(()=>null);continue}await e.pause(150);const c=await e.execute(q,r).catch(()=>null),d=xe(o,c);if(d){await e.pause(1500);const f=await e.execute(Oe,r).catch(()=>null);await e.execute(oe,r).catch(()=>!1);try{await e.action("wheel").scroll({x:Math.round(r.x),y:Math.round(r.y),deltaX:0,deltaY:120}).perform()}catch{}await e.pause(150);const y=await e.execute(q,r).catch(()=>null);xe(f,y)===d&&a.push({key:r.key,input:"wheel:down",context:r.context+" blocked by "+(o.desc||"overlay")})}else c&&(c.topDelta||c.winDelta)&&await e.execute(f=>{const y=document.querySelector('[data-reproit-deadinput="'+f.idx+'"]');y&&(y.scrollTop-=f.topDelta),f.winDelta&&window.scrollBy(0,-f.winDelta)},{idx:r.idx,topDelta:c.topDelta,winDelta:c.winDelta}).catch(()=>{})}await e.execute(()=>{for(const r of document.querySelectorAll("[data-reproit-deadinput]"))r.removeAttribute("data-reproit-deadinput")}).catch(()=>{});const t=await e.execute(()=>{for(const r of document.querySelectorAll("input, textarea")){const o=(r.getAttribute("type")||"text").toLowerCase(),c=r.tagName==="TEXTAREA"||r.tagName==="INPUT"&&/^(text|search|email|url|tel)$/.test(o),d=r.getBoundingClientRect();if(!c||r.disabled||r.readOnly||r.value!==""||d.width<40||d.height<12||d.bottom<0||d.top>innerHeight)continue;r.setAttribute("data-reproit-deadinput","key");const f=r.getAttribute("data-testid")||r.getAttribute("data-test-id");return{key:f?"testid:"+f:r.name?"name:"+r.name:"editable#0",context:r.tagName.toLowerCase()+(r.id?"#"+r.id:"")}}return null}).catch(()=>null);if(t){await(await e.$('[data-reproit-deadinput="key"]')).click().catch(()=>{}),await e.execute(oe,{idx:"key"}).catch(()=>!1),await e.keys(["a"]).catch(()=>{}),await e.pause(150);const o=await e.execute(()=>document.querySelector('[data-reproit-deadinput="key"]')?.value??null).catch(()=>null),c=await e.execute(q,{idx:"key"}).catch(()=>null);mt(c,"",o??"")?a.push({key:t.key,input:"key:a",context:t.context}):o&&await e.keys(["Backspace"]).catch(()=>{}),await e.execute(()=>{const d=document.querySelector('[data-reproit-deadinput="key"]');d&&d.removeAttribute("data-reproit-deadinput"),document.activeElement?.blur?.()}).catch(()=>{})}return a.slice(0,6)}const H=process.env.REPROIT_APP,St=process.env.REPROIT_WEBDRIVER_URL||"http://127.0.0.1:4444",K=process.env.REPROIT_VIDEO_DIR||void 0,V=process.env.REPROIT_PROBE==="1",vt=36,bn=40,_t=8;function s(e){process.stdout.write(e+`
`)}async function Re(e,a){const i=process.env.REPROIT_SHOTS_DIR;if(i)try{ne(i,{recursive:!0});const t=await e.takeScreenshot();pe(W(i,a+".png"),Buffer.from(t,"base64"))}catch{}s("SHOOT:"+a)}import{snapshotJs as kt}from"./tauri-snapshot.mjs";async function Ot(e,a){const i=await e.execute(kt(a||[]));i.sig=we(i.anchor,i.tree);const t=ye(i.anchor,i.tree),r=t.indexOf(`
V:`);return i.vsection=r>=0?t.slice(r+3):"",i.structuralSig=r>=0?Ye(t.slice(0,r)):i.sig,i.content=i.sig+"|"+i.textNodes.map(o=>o[0]+"="+o[1]).join(";"),i}async function xt(e){try{await e.executeAsync(a=>{const i=()=>new Promise(t=>requestAnimationFrame(()=>requestAnimationFrame(t)));(async()=>{await new Promise(t=>{let r=null,o=null;const c=()=>{if(o&&clearTimeout(o),f&&clearTimeout(f),r)try{r.disconnect()}catch{}t()},d=()=>{o&&clearTimeout(o),o=setTimeout(c,400)},f=setTimeout(c,1800);try{r=new MutationObserver(d),r.observe(document.documentElement,{subtree:!0,childList:!0,attributes:!0,characterData:!0})}catch{}d()});try{const t=(document.getAnimations?document.getAnimations():[]).filter(r=>r.playState==="running");await Promise.race([Promise.allSettled(t.map(r=>r.finished)),new Promise(r=>setTimeout(r,800))])}catch{}await i(),a()})()})}catch{}}async function Rt(e){try{return await e.execute(()=>{const a=(document.title||"").toLowerCase(),i=(document.body&&document.body.innerText||"").toLowerCase(),t=r=>r.test(a)||r.test(i);return document.querySelector('#challenge-running, #cf-challenge-running, #challenge-form, .cf-turnstile, [id^="cf-chl"], script[src*="challenge-platform"], iframe[src*="challenges.cloudflare.com"]')?{vendor:"Cloudflare",marker:"challenge-platform"}:t(/just a moment/)||t(/checking your browser before/)||t(/performing (a )?security verification/)||t(/enable javascript and cookies to continue/)?{vendor:"Cloudflare",marker:"interstitial"}:t(/attention required/)&&t(/cloudflare/)?{vendor:"Cloudflare",marker:"attention-required"}:document.querySelector('#px-captcha, .px-block, [class*="perimeterx"]')?{vendor:"PerimeterX",marker:"px-captcha"}:/ray id:/.test(i)&&i.length<1200?{vendor:"Cloudflare",marker:"ray-id-block"}:null})}catch{return null}}const Tt=`
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
    if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
      return true;
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
`;async function At(e,a){let i;try{i=await e.execute(Tt)}catch{return}i&&s("EXPLORE:GROUNDTRUTH "+JSON.stringify({sig:a,focusTrap:!!i.focusTrap,elements:i.elements||[]}))}const Nt=$e,$=200,z=2e3,Ct=`
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
`,It="try { window.__reproitLongTasks = []; } catch (_) {} return true;",Lt=`
  const t = window.__reproitLongTasks || [];
  window.__reproitLongTasks = [];
  return t;
`;async function Te(e){try{await e.execute(Ct)}catch{}}async function Pt(e){let a=[];try{a=await e.execute(Lt)}catch{return null}if(!a||!a.length)return null;const i=Math.max(...a);return i>=z?{kind:"hang",bucket:z,count:a.length}:i>=$?{kind:"jank",bucket:$,count:a.length}:null}const Ae=100,Ft=2,Dt=350;function Ne(e){if(!e||!e.length)return null;let a=0;for(const o of e)o>=z&&a++;if(a>0)return{kind:"hang",bucket:z,count:a};let i=0,t=0;const r=e.length;for(;t<r;){if(e[t]<Ae){t++;continue}let o=t,c=0,d=0;for(;o<r&&e[o]>=Ae;)c+=e[o],e[o]>d&&(d=e[o]),o++;const f=o-t,y=d>=Dt,k=f>=Ft&&c>=$;(y||k)&&i++,t=o}return i>0?{kind:"jank",bucket:$,count:i}:null}const Jt=`
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
`,Ut="try { window.__reproitFrameIntervals = []; } catch (_) {} return true;",Mt=`
  const t = window.__reproitFrameIntervals || [];
  window.__reproitFrameIntervals = [];
  return t;
`;async function Ce(e){try{await e.execute(Jt)}catch{}}async function Wt(e){let a=[];try{a=await e.execute(Mt)}catch{return null}return Ne(a)}async function Ht(e){const a=await Pt(e);return a||Wt(e)}const Bt=`
  try {
    if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
      return performance.memory.usedJSHeapSize;
    }
  } catch (_) {}
  return null;
`;function L(e,a){try{const i=ze(e,a,{encoding:"utf8",stdio:["ignore","pipe","ignore"],timeout:5e3});return i==null?null:String(i)}catch{return null}}function se(e){if(!e)return null;if(re()==="win32"){const r=e.split(/[\\/]/).pop()||e,o=L("tasklist",["/FI","IMAGENAME eq "+r,"/FO","CSV","/NH"]);if(o==null)return null;const c=[];for(const d of o.split(/\r?\n/)){const f=d.match(/^"[^"]*","(\d+)"/);f&&c.push(parseInt(f[1],10))}return c.length!==1||!Number.isFinite(c[0])||c[0]<=0?null:c[0]}const i=L("ps",["-axww","-o","pid=,args="]);if(i==null)return null;const t=[];for(const r of i.split(`
`)){const o=r.match(/^\s*(\d+)\s+(.*)$/);if(!o)continue;const c=o[2].trim();(c===e||c.startsWith(e+" "))&&t.push(parseInt(o[1],10))}return t.length!==1||!Number.isFinite(t[0])||t[0]<=0?null:t[0]}function Xt(e){if(!(e>0))return null;if(re()==="win32"){const t=L("tasklist",["/FI","PID eq "+e,"/FO","CSV","/NH"]);if(t==null)return null;const r=t.match(/"([\d.,]+)\s*K"/);if(!r)return null;const o=parseInt(r[1].replace(/[.,]/g,""),10);return!Number.isFinite(o)||o<=0?null:o*1024}const a=L("ps",["-o","rss=","-p",String(e)]);if(a==null)return null;const i=parseInt(a.trim(),10);return!Number.isFinite(i)||i<=0?null:i*1024}async function ce(e,a,i){if(i&&(i.tried||(i.tried=!0,i.pid=se(H)),i.pid>0)){const r=Xt(i.pid);if(r!=null){s("MEMORY:SAMPLE "+JSON.stringify({t_ms:a,heap_used:r}));return}}let t=null;try{t=await e.execute(Bt)}catch{t=null}t!=null&&s("MEMORY:SAMPLE "+JSON.stringify({t_ms:a,heap_used:t}))}const Yt=`
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
`;async function j(e){try{await e.execute(Yt)}catch{}}function Gt(e){s("EXCEPTION CAUGHT BY TAURI WEBVIEW"),s("The following error was thrown:"),s(String(e&&e.message?e.message:e));const a=e&&e.stack?String(e.stack):"";for(const i of a.split(`
`).slice(0,8))i&&s(i);s("\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550")}async function Z(e){let a=[];try{a=await e.execute(()=>{const i=window.__reproit_errors||[];return window.__reproit_errors=[],i})}catch{return}if(Array.isArray(a))for(const i of a)Gt(i)}const qt=`
  const resolveStructuralTarget = ${be};
  const el = resolveStructuralTarget(arguments[0]);
  if (!el) return false;
  // Stash the clicked element for the post-tap oracle probes (the focus-loss
  // guards read it in-page). A window ref only, never a DOM mutation, so the
  // signature/content/mutation oracles are untouched.
  try {
    window.__reproitLastTap = el;
    // UNCHANGED, deliberately. The web and Electron runners now OBSERVE focus on
    // the real click instead of calling el.focus() first (the synthetic focus is
    // ignored by focusLossCheck, so it bought nothing, while it parked focus on
    // the control and manufactured the \`pre === tapped\` precondition for the
    // NEXT action). The same correction belongs here, but it changes what this
    // runner reports and can only be proven against tauri-driver plus the
    // platform webdriver, which this change had no way to drive. Left alone and
    // stated rather than changed on reasoning alone.
    if (window.__reproitFocusProbe) {
      try { el.focus({ preventScroll: true }); } catch (_) {}
      window.__reproitTapFocused = document.activeElement === el;
    }
  } catch (_) {}
  el.click();
  return true;
`;async function Ie(e,a){try{return!!await e.execute(qt,a)}catch{return!1}}const Kt=`
  const done = arguments[arguments.length - 1];
  const resolveStructuralTarget = ${be};
  const el = resolveStructuralTarget(arguments[0]);
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
`;async function Vt(e,a){try{return await e.executeAsync(Kt,a)}catch{return null}}function Le(e,a){try{ne(W(a,".."),{recursive:!0})}catch{}const i=re();try{if(i==="linux"){const t=process.env.DISPLAY||":0";let r=(L("xdotool",["search","--pid",String(e),"--onlyvisible"])||"").trim().split(/\s+/).filter(Boolean).pop();if(!r)return null;const o=L("xdotool",["getwindowgeometry","--shell",r])||"",c={};for(const k of o.split(`
`)){const P=k.match(/^(\w+)=(-?\d+)/);P&&(c[P[1]]=parseInt(P[2],10))}if(!(c.WIDTH>0&&c.HEIGHT>0))return null;const d=c.WIDTH-c.WIDTH%2,f=c.HEIGHT-c.HEIGHT%2;return Ee("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","x11grab","-framerate","15","-video_size",`${d}x${f}`,"-i",`${t}+${c.X||0},${c.Y||0}`,"-c:v","libx264","-pix_fmt","yuv420p",a],{stdio:["pipe","ignore","ignore"]})}if(i==="win32"){const r=(L("tasklist",["/FI","PID eq "+e,"/FO","CSV","/NH","/V"])||"").match(/^"[^"]*","\d+","[^"]*","[^"]*","[^"]*","([^"]*)"/),o=r&&r[1]&&r[1]!=="N/A"?r[1]:null;return o?Ee("ffmpeg",["-hide_banner","-loglevel","error","-y","-f","gdigrab","-framerate","15","-i","title="+o,"-c:v","libx264","-pix_fmt","yuv420p",a],{stdio:["pipe","ignore","ignore"]}):null}if(i==="darwin")return null}catch{}return null}async function Pe(e){!e||e.exitCode!==null||await new Promise(a=>{let i=!1;const t=()=>{i||(i=!0,a())};e.once("exit",t);try{e.stdin&&e.stdin.writable&&e.stdin.write("q")}catch{}try{e.kill("SIGINT")}catch{}setTimeout(t,4e3)})}const $t=process.env.REPROIT_FLICKER_PIXELS==="1",le=process.env.REPROIT_FLICKER_DIAGNOSTICS==="1";function zt(e){if(!$t||!e)return null;let a;try{a=Be(W(je(),"reproit-tauri-flicker-"));const i=W(a,"transition.mov"),t=Le(e,i);return t?(le&&s("REPROIT:FLICKER_CAPTURE started"),{dir:a,mov:i,proc:t}):(le&&s("REPROIT:FLICKER_CAPTURE unavailable"),ie(a,{recursive:!0,force:!0}),null)}catch{return a&&ie(a,{recursive:!0,force:!0}),null}}async function Fe(e){if(!e)return null;try{await Pe(e.proc);const a=Ze(e.mov);if(le){const i=He(e.mov)?We(e.mov).length:0;s(`REPROIT:FLICKER_CAPTURE frames=${a?.frames||0} bytes=${i}`)}return a}catch{return null}finally{try{ie(e.dir,{recursive:!0,force:!0})}catch{}}}const jt=`
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
`,Zt=`
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
`;async function Qt(e,a,i){if(s("FUZZ:ACT "+i+" "+a),a.startsWith("shoot:")){await Re(e,a.slice(6));return}if(a.startsWith("assert:")){const o=a.slice(7);if(o.startsWith("text=")){const c=o.slice(5);let d=!1;try{d=await e.execute("return !!(document.body && document.body.innerText.includes(arguments[0]))",c)}catch{}s("FUZZ:ASSERT "+(d?"pass":"fail")+" text="+JSON.stringify(c)+" actor="+i)}else if(o.startsWith("count:")){const c=o.slice(6),d=c.lastIndexOf("="),f=d>=0?c.slice(0,d):c,y=d>=0?parseInt(c.slice(d+1),10):0;let k=-1;try{k=await e.execute(Zt,f)}catch{}s("FUZZ:ASSERT "+(k===y?"pass":"fail")+" count "+f+" want="+y+" got="+k+" actor="+i)}else s("FUZZ:ASSERT fail unsupported "+o+" actor="+i);await e.pause(300);return}if(a==="back"){await e.back().catch(()=>{}),await e.pause(400);return}if(a.startsWith("auth:")){s("JOURNEY[a] step: auth-restore unsupported on tauri runner; use login() for "+a),await e.pause(200);return}if(a.startsWith("type:")){const o=a.slice(5),c=o.lastIndexOf("="),d=c>=0?o.slice(0,c):o,f=Ve(c>=0?o.slice(c+1):"");f!=null&&String(f).length>0&&ae.add(String(f));let y=!1;try{y=await e.execute(jt,d,f)}catch{}y||s("FUZZ:MISS "+i+" "+a),await e.pause(900);return}const t=a.slice(4);await Ie(e,t)||s("FUZZ:MISS "+i+" "+a),await e.pause(900)}async function en(e){const a=process.env.REPROIT_SCENARIO_BARRIER;let i=process.env.REPROIT_DEVICE;if(!i){try{i=(await(await fetch(a+"/claim")).text()).trim()}catch{i=""}(!i||i.startsWith("ERR"))&&(i="a")}s("JOURNEY claimed role="+i),await e.pause(1500),await j(e);const t=r=>new Promise(o=>setTimeout(o,r));for(let r=0;r<1e5;r++){let o="WAIT";try{o=(await(await fetch(a+"/next?device="+i)).text()).trim()}catch{await t(100);continue}if(o==="DONE")break;if(o==="WAIT"){await t(40);continue}const c=o.startsWith("ACT	")?o.slice(4):o;await Qt(e,c,i),await j(e),await Z(e);try{await fetch(a+"/done?device="+i,{method:"POST"})}catch{}}await Z(e),s("JOURNEY DONE"),s("All tests passed")}async function tn(){H||(s("EXCEPTION CAUGHT BY REPROIT"),s("REPROIT_APP (executable path) required"),s("\u2550".repeat(8)),process.exit(0));const e=qe(),{remote:a}=await import("webdriverio"),i=new URL(St),t=await a({hostname:i.hostname,port:Number(i.port||4444),path:i.pathname||"/",capabilities:{"tauri:options":{application:H}}});if(process.env.REPROIT_SCENARIO_BARRIER){s("JOURNEY[a] step: scenario actor="+(process.env.REPROIT_DEVICE||"a")),await en(t),await t.deleteSession();return}s("JOURNEY claimed role=a"),await t.pause(1500);try{await t.setTimeout({script:3e4})}catch{}await j(t),await Te(t),await Ce(t);const r=await Rt(t);if(r){const n=`target is behind a ${r.vendor} bot-challenge (${r.marker}); reproit could not reach the app.`;s("EXPLORE:UNSCANNABLE "+JSON.stringify({reason:"bot-wall",vendor:r.vendor,marker:r.marker,diagnostic:n})),s("JOURNEY[a] step: UNSCANNABLE - "+n),s("JOURNEY DONE"),s("All tests passed"),await t.deleteSession();return}const o=new Set,c=new Set,d=Ke(e.seed||0),f=Ge();f.length&&s(`JOURNEY[a] step: value_nodes=${f.length}`);const y=new Map,k=new Set;function P(n){if(k.has(n.structuralSig))return n.structuralSig;if(n.vsection){let l=y.get(n.structuralSig);if(l||(l=new Set,y.set(n.structuralSig,l)),l.add(n.vsection),l.size>_t)return k.add(n.structuralSig),s(`JOURNEY[a] step: value-cap hit (${n.structuralSig})`),n.structuralSig}return n.sig}const F=async()=>{await j(t),await Te(t),await Ce(t),await Z(t);const n=await Ot(t,f);if(n.sig=P(n),s("FUZZ:OBS "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},labels:n.labels.slice(0,24),elements:n.tappables.slice(0,24).map(l=>({role:l.role}))})),!o.has(n.sig)){o.add(n.sig),s("EXPLORE:STATE "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},labels:n.labels.slice(0,24),elements:n.tappables.slice(0,24).map(h=>{const x={sel:h.sel,role:h.role,label:h.label};return h.key||(x.nokey=!0),x})}));let l=null,u=null;try{l=await t.execute(ke),await t.pause(120),u=await t.execute(ke)}catch{}const m=ft(l,u);(m.checks.length||!m.complete)&&s("EXPLORE:OVERFLOW "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},...m})),await At(t,n.sig);let S=null;try{S=await t.execute(Nt,[...ae])}catch{}S&&S.length&&s("EXPLORE:CONTENTBUG "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:S}));let R=null;try{const h=await t.execute(Se);if(R=h,h&&h.length){await t.pause(300);const x=await t.execute(Se);R=it(h,x||[])}}catch{}R&&R.length&&s("EXPLORE:OCCLUSION "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:R}));const N=await t.execute(ve).catch(()=>null);let C=null;if(N?.outcome==="VIOLATION"){await t.pause(120),C=await t.execute(ve).catch(()=>null);const h=at(N,C);h.length&&s("EXPLORE:RELATION "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:h}))}const T=C||N;T&&s("EXPLORE:RELATIONSTATUS "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},outcome:T.outcome,checks:T.checks}));let I=null;try{I=await t.execute(ht)}catch{}I&&I.length&&s("EXPLORE:ZEROCONTRAST "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:I}));let E=null;try{E=await t.execute(rt)}catch{}E&&E.length&&s("EXPLORE:SECURITY "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:E}));const w=await Et(t).catch(()=>[]);w.length&&s("EXPLORE:DEADINPUT "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:w}));let v=null;try{v=await t.execute(_e)}catch{}if(v&&v.length){await xt(t);try{v=await t.execute(_e)}catch{}}v&&v.length&&s("EXPLORE:BLANKSCREEN "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:v}));let p=null;try{p=await t.execute(()=>{const h=window.__reproit_invariants||[],x=[];for(let U=0;U<h.length;U++){const b=h[U];if(!b||typeof b.test!="function")continue;let M=!0,te="";try{const O=b.test();O&&typeof O=="object"?(M=!!O.ok,te=O.message?String(O.message):""):M=!!O}catch(O){M=!1,te=O&&O.message?String(O.message):String(O)}M||x.push({id:String(b.id),message:te})}return x})}catch{}p&&p.length&&s("EXPLORE:INVARIANT "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:p}));let J=null;try{J=await t.execute(ct,[...ae])}catch{}if(J&&J.length&&s("EXPLORE:BROKENASSET "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:J})),!V){let h=[];try{h=await t.executeAsync(bt)}catch{h=[]}h&&h.length&&s("EXPLORE:SCROLLROUNDTRIP "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},items:h}))}}return n};let g=await F(),A=0;const B=e.prefix||null,_=e.replay||null;let ue=!1;const de=B?B.length:0,De=_?_.length:(e.budget||vt)+de,fe=new Set,X=new Set;async function he(n,l){let u=null;try{if(u=await t.getWindowSize(),!u||!(u.width>0&&u.height>0)){u=null;return}const m=await t.execute(lt);await t.setWindowSize(Math.round(u.width/2),Math.round(u.height/2)),await t.pause(350);let S=null;try{S=await t.execute(ut,m)}catch{S=null}S&&S.length&&s("EXPLORE:ZOOMREFLOW "+JSON.stringify({sig:n,...l?{route:l}:{},items:S}))}catch{}finally{if(u)try{await t.setWindowSize(u.width,u.height),await t.pause(350)}catch{}}}!_&&!V&&g.anchor&&!X.has(g.anchor)&&(X.add(g.anchor),await he(g.sig,g.anchor));const ge=new Set,me=new Set;async function Je(n){const l=n.structuralSig;let u=null;try{u=await t.getWindowSize(),!u||!(u.width>0&&u.height>0)?u=null:(await t.setWindowSize(u.height,u.width),await t.pause(350))}catch{}if(u)try{await t.setWindowSize(u.width,u.height),await t.pause(350)}catch{}const m=await F();return n.tappables&&n.tappables.length>0&&m.structuralSig!==l&&s("EXPLORE:ROTATION "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},expected:l,got:m.structuralSig})),m}async function Ue(n){const l=n.structuralSig;try{await t.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"hidden"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!0})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pagehide")),window.dispatchEvent(new Event("blur"))}),await t.pause(300),await t.execute(()=>{try{Object.defineProperty(document,"visibilityState",{configurable:!0,get:()=>"visible"})}catch{}try{Object.defineProperty(document,"hidden",{configurable:!0,get:()=>!1})}catch{}document.dispatchEvent(new Event("visibilitychange")),window.dispatchEvent(new Event("pageshow")),window.dispatchEvent(new Event("focus"))}),await t.pause(300)}catch{}const u=await F();return n.tappables&&n.tappables.length>0&&u.structuralSig!==l&&s("EXPLORE:BGRESTORE "+JSON.stringify({sig:n.sig,...n.anchor?{route:n.anchor}:{},expected:l,got:u.structuralSig})),u}const Q=Date.now(),ee={pid:null,tried:!1};_&&await ce(t,0,ee);const D=e.clip&&typeof e.clip.sel=="string"?e.clip:null,Y=!!(K&&_&&D),Me=Y?W(K,"clip.mov"):null;let G=null;if(Y){const n=se(H);n&&(G=Le(n,Me)),G&&await t.pause(400)}for(let n=0;n<De&&A<3;n++){if(_&&n>0&&await ce(t,Date.now()-Q,ee),!_&&!V&&(ge.has(g.sig)||(ge.add(g.sig),g=await Je(g)),me.has(g.sig)||(me.add(g.sig),g=await Ue(g))),!_&&!fe.has(g.sig)){fe.add(g.sig);let w=[];try{w=await t.executeAsync(yt)}catch{w=[]}let v=!1;for(const p of w||[])v=!0,s("EXPLORE:CHOICEBUG "+JSON.stringify({from:g.sig,role:p.role,outlier:p.outlier,magnitude:p.magnitude,siblingMedian:p.siblingMedian}));if(v){g=await F();continue}}let l;if(_)l=_[n];else if(B&&n<de)l=B[n];else if(e.seed){const w=g.tappables.map(b=>b.sel).sort(),v=e.edgeWeights&&e.edgeWeights[g.sig]||{},p=w.map(b=>"tap:"+b).concat(["back"]),J=new Set(e.contractActions||[]),h=p.map(b=>(J.has(b)?4:1)/(1+(v[b]||0))),x=h.reduce((b,M)=>b+M,0);let U=d(1<<20)/(1<<20)*x;l=p[p.length-1];for(let b=0;b<p.length;b++)if(U-=h[b],U<=0){l=p[b];break}}else{l=null;for(const w of g.tappables)if(!c.has(g.sig+"|"+w.sel)){l="tap:"+w.sel;break}l=l||"back"}if(_&&!ue&&process.env.REPROIT_INSPECT==="1"){const w=g.tappables.find(p=>`tap:${p.sel}`===l);ue=await wt({action:l,step:n+1,total:_.length,target:w?.label||w?.sel||null})==="continue"}if(s("FUZZ:ACT "+l),l.startsWith("shoot:")){await Re(t,l.slice(6));continue}if(l==="back"){const w=g.sig,v=g.content;await t.back().catch(()=>{}),await t.pause(600);const p=await F();p.sig!==w?(s("EXPLORE:EDGE "+JSON.stringify({from:w,action:"back",to:p.sig})),A=0):p.content!==v?A=0:A++,g=p;continue}const u=l.slice(4);c.add(g.sig+"|"+u);const m=g.sig,S=g.content,R=g.anchor;try{await t.execute(It)}catch{}try{await t.execute(Ut)}catch{}try{await t.execute(ot)}catch{}const N=Y?null:zt(se(H));if(N&&await t.pause(500),!await Ie(t,u)){await Fe(N),s("FUZZ:MISS "+l),A++;continue}await t.pause(700);const C=await Fe(N);C&&s("EXPLORE:FLICKER "+JSON.stringify({from:m,action:"tap:"+u,peak:C.peak,frames:C.frames}));const T=await Ht(t);T&&s("EXPLORE:"+(T.kind==="hang"?"HANG":"JANK")+" "+JSON.stringify({from:m,action:"tap:"+u,bucket:T.bucket,count:T.count}));let I=!1;try{I=await t.execute(st)}catch{}const E=await F();I&&(E.sig===m||E.anchor&&E.anchor===R)&&s("EXPLORE:FOCUSLOSS "+JSON.stringify({from:m,action:"tap:"+u})),E.sig!==m?(s("EXPLORE:EDGE "+JSON.stringify({from:m,action:"tap:"+u,to:E.sig})),A=0,!_&&!V&&E.anchor&&!X.has(E.anchor)&&(X.add(E.anchor),await he(E.sig,E.anchor))):E.content!==S&&(A=0),g=E}if(_&&await ce(t,Date.now()-Q,ee),await Z(t),Y){await t.pause(300);const n=G?await Vt(t,D.sel):null;let l=!1;if(n){const u=Math.max(0,(Date.now()-Q)/1e3-.2),m={videoW:n.videoW,videoH:n.videoH,boxes:[{x:n.x,y:n.y,w:n.w,h:n.h,tStart:u,tEnd:1e9,label:D.label||D.oracle||"finding",color:"red"}]};try{ne(K,{recursive:!0}),pe(W(K,"box-spec.json"),JSON.stringify(m)),l=!0}catch{l=!1}await t.pause(900)}await Pe(G),s("FINDING:BOXED "+JSON.stringify({oracle:D.oracle||null,sel:D.sel,drew:l}))}s(`JOURNEY[a] step: explored ${o.size} states`),s("JOURNEY DONE"),s("All tests passed"),await t.deleteSession()}const nn=process.argv[1]&&import.meta.url===new URL(`file://${process.argv[1]}`).href;nn&&tn().catch(e=>{s("EXCEPTION CAUGHT BY TAURI RUNNER"),s(String(e&&e.stack?e.stack:e)),s("Some tests failed"),process.exit(0)});export{Ne as classifyFrameIntervals,ye as descriptorOf,we as signatureOf,Xe as valueClass};
