function C(i){const s=i.getBoundingClientRect();if(s.width===0||s.height===0)return!1;const n=getComputedStyle(i);return n.visibility!=="hidden"&&n.display!=="none"}function k(i){const s={screen:1,header:1,text:1,button:1,link:1,textfield:1,image:1,icon:1,list:1,listitem:1,tab:1,switch:1,checkbox:1,radio:1,slider:1,menu:1,menuitem:1,dialog:1,group:1,node:1},n=i.tagName.toLowerCase(),o=(i.getAttribute("role")||"").toLowerCase();if(o){if(o==="textbox"||o==="searchbox"||o==="combobox")return"textfield";if(o==="heading")return"header";if(o==="img")return"image";if(o==="switch")return"switch";if(o==="link")return"link";if(o==="button")return"button";if(s[o])return o}if(n==="input"){const a=(i.getAttribute("type")||"text").toLowerCase();return a==="checkbox"?"checkbox":a==="radio"?"radio":a==="range"?"slider":["button","submit","reset","image"].includes(a)?"button":"textfield"}return n==="textarea"||n==="select"?"textfield":n==="a"?"link":n==="button"?"button":n==="img"||n==="svg"?"image":/^h[1-6]$/.test(n)||n==="header"?"header":n==="ul"||n==="ol"?"list":n==="li"?"listitem":n==="dialog"?"dialog":n==="nav"||n==="menu"?"menu":"node"}function w(i,s){const n=i.tagName.toLowerCase();return!!(["a","button","select"].includes(n)||n==="input"||n==="textarea"||s==="textfield"||["button","link","menuitem","tab","checkbox","switch","radio"].includes(s)||i.hasAttribute("onclick")||i.tabIndex>=0)}const E="const visible = "+C.toString()+`;
const roleOf = `+k.toString()+`;
const interactive = `+w.toString()+`;
`,O=`
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
`,v=new Function("sel",E+O),T=v.toString();function A(i){const s=(Array.isArray(i)?i:[]).map(t=>String(t??"").toLowerCase()).filter(t=>t.length>0),n=t=>{const e=String(t||"").toLowerCase();if(!e)return!1;if(s.some(c=>e.indexOf(c)!==-1||c.length>=3&&c.indexOf(e)!==-1))return!0;const r=[],u=e.match(/\{\{[^}]*\}\}/g);u&&r.push(...u);const l=e.match(/\$\{[^}]*\}/g);return l&&r.push(...l),e.indexOf("[object object]")!==-1&&r.push("[object object]"),r.some(c=>s.some(d=>d.indexOf(c)!==-1))},o=t=>{const e=t.getBoundingClientRect();if(e.width===0||e.height===0)return!1;const r=getComputedStyle(t);return r.visibility!=="hidden"&&r.display!=="none"},a=new Set(["code","pre","script","style","textarea"]),h=t=>{if(t.isContentEditable)return!0;for(let e=t;e&&e!==document.body;e=e.parentElement)if(a.has(e.tagName.toLowerCase()))return!0;return!1},p=t=>{const e=(t.getAttribute("data-testid")||t.getAttribute("data-test-id")||"").trim();if(e)return"testid:"+e;const r=(t.getAttribute("id")||"").trim();if(r)return"id:"+r;const u=(t.getAttribute("name")||"").trim();return u?"name:"+u:null},x=t=>{let e="";for(const r of t.childNodes)r.nodeType===3&&(e+=r.textContent);return e.replace(/\s+/g," ").trim()},g=t=>t.length<=24&&!/[.!?]/.test(t),S=t=>{if(!t)return null;if(t.includes("[object Object]")){const e=t.replace(/\[object Object\]/g," ").replace(/\s+/g," ").trim();if(g(e))return"object-object"}if(/\{\{[^}]*\}\}/.test(t)||/\$\{[^}]*\}/.test(t)){const e=t.replace(/\{\{[^}]*\}\}/g," ").replace(/\$\{[^}]*\}/g," ").replace(/\s+/g," ").trim();if(g(e))return"unrendered-template"}return null},f=[],m=new Set,y=document.body?document.body.querySelectorAll("*"):[],b={};for(const t of y){if(!o(t)||h(t))continue;const e=t.tagName.toLowerCase(),r=b[e]||0;b[e]=r+1;const u=p(t)||"tag:"+e+"#"+r,l=x(t),c=S(l);if(!c||n(l))continue;const d=u+"|"+c;m.has(d)||(m.add(d),f.push({key:u,reason:c,text:l.slice(0,80)}))}return f.sort((t,e)=>t.key<e.key?-1:t.key>e.key?1:t.reason<e.reason?-1:t.reason>e.reason?1:0),f}const R="return ("+A.toString()+")(arguments[0]);";export{R as DETECT_CONTENT_BUGS_SRC,E as DOM_PREDICATES_SRC,T as RESOLVE_STRUCTURAL_TARGET_SRC,A as detectContentBugs,v as resolveStructuralTarget};
