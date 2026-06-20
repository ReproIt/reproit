//! App-map visualization: render the state graph for humans.
//!
//! - mermaid: paste into FigJam (native Mermaid import), GitHub markdown,
//!   or docs. The design-team artifact: a living user-flow diagram.
//! - dot: Graphviz for pipelines that want it.
//! - html: self-contained interactive viewer (force layout, no network).
//!
//! Until `reproit map` ships, the input is a hand-written or agent-written
//! appmap JSON (schema: src/appmap.rs); see examples/appmap.example.json.

use crate::appmap::{Action, AppMap, InterruptPolicy, Reversibility};
use anyhow::{Context, Result};
use std::path::Path;

pub fn render(map_path: &Path, format: &str, out: Option<&Path>) -> Result<()> {
    let raw = std::fs::read_to_string(map_path)
        .with_context(|| format!("reading app map {}", map_path.display()))?;
    let map: AppMap =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", map_path.display()))?;

    let rendered = match format {
        "mermaid" => mermaid(&map),
        "dot" => dot(&map),
        "html" => html(&map)?,
        other => anyhow::bail!("unknown format {other:?} (mermaid | dot | html)"),
    };
    match out {
        Some(path) => {
            std::fs::write(path, rendered)?;
            println!("wrote {}", path.display());
        }
        None => print!("{rendered}"),
    }
    Ok(())
}

fn action_label(a: &Action) -> String {
    match a {
        Action::Tap { finder } => format!("tap {finder}"),
        Action::Type { finder, .. } => format!("type into {finder}"),
        Action::Scroll { finder, .. } => format!("scroll {finder}"),
        Action::Back => "back".to_string(),
        Action::System { event } => format!("system: {event}"),
    }
}

fn esc(s: &str) -> String {
    s.replace('"', "'").replace('\n', " ")
}

fn mermaid(map: &AppMap) -> String {
    let mut out = String::from("flowchart LR\n");
    for (id, state) in &map.states {
        let params = if state.parameters.is_empty() {
            String::new()
        } else {
            format!(" ({})", state.parameters.join(", "))
        };
        out.push_str(&format!(
            "  {id}[\"{}{params}\"]\n",
            esc(&state.description)
        ));
    }
    for t in &map.transitions {
        let arrow = match t.reversibility {
            Reversibility::VerifiedIrreversible | Reversibility::Destructive => "-.->",
            _ => "-->",
        };
        out.push_str(&format!(
            "  {} {arrow}|\"{}\"| {}\n",
            t.from,
            esc(&action_label(&t.action)),
            t.to
        ));
    }
    if !map.interrupts.is_empty() {
        out.push_str("  subgraph interrupts[\"interrupts (overlay any state)\"]\n");
        for i in &map.interrupts {
            let policy = match &i.policy {
                InterruptPolicy::Dismiss { .. } => "dismiss",
                InterruptPolicy::Accept { .. } => "accept",
                InterruptPolicy::Promote { .. } => "promote",
            };
            out.push_str(&format!(
                "    int_{}[\"{} [{policy}]\"]\n",
                i.id,
                esc(&i.description)
            ));
        }
        out.push_str("  end\n");
    }
    out
}

fn dot(map: &AppMap) -> String {
    let mut out =
        String::from("digraph appmap {\n  rankdir=LR;\n  node [shape=box, style=rounded];\n");
    for (id, state) in &map.states {
        out.push_str(&format!(
            "  {id} [label=\"{}\"];\n",
            esc(&state.description)
        ));
    }
    for t in &map.transitions {
        let style = match t.reversibility {
            Reversibility::VerifiedIrreversible | Reversibility::Destructive => {
                ", style=dashed, color=red"
            }
            _ => "",
        };
        out.push_str(&format!(
            "  {} -> {} [label=\"{}\"{style}];\n",
            t.from,
            t.to,
            esc(&action_label(&t.action))
        ));
    }
    out.push_str("}\n");
    out
}

fn html(map: &AppMap) -> Result<String> {
    // Embed graph data; tiny force layout, pan/zoom, no network access.
    let nodes: Vec<serde_json::Value> = map
        .states
        .iter()
        .map(|(id, s)| {
            serde_json::json!({
                "id": id,
                "label": s.description,
                "params": s.parameters,
            })
        })
        .collect();
    let edges: Vec<serde_json::Value> = map
        .transitions
        .iter()
        .map(|t| {
            serde_json::json!({
                "from": t.from,
                "to": t.to,
                "label": action_label(&t.action),
                "hard": matches!(
                    t.reversibility,
                    Reversibility::VerifiedIrreversible | Reversibility::Destructive
                ),
            })
        })
        .collect();
    let interrupts: Vec<serde_json::Value> = map
        .interrupts
        .iter()
        .map(|i| serde_json::json!({ "id": i.id, "label": i.description }))
        .collect();
    let data = serde_json::json!({
        "app": map.app,
        "version": map.version,
        "nodes": nodes,
        "edges": edges,
        "interrupts": interrupts,
    });
    Ok(HTML_TEMPLATE.replace("/*DATA*/", &serde_json::to_string(&data)?))
}

const HTML_TEMPLATE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><title>reproit app map</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600&display=swap" rel="stylesheet">
<style>
  body{margin:0;background:#0a0b0d;color:#e9ebec;font:13px/1.5 'JetBrains Mono',ui-monospace,SFMono-Regular,Menlo,monospace}
  #hud{position:fixed;top:12px;left:14px;z-index:2}
  #hud b{color:#4ade80;font-weight:500}
  #hud .dim{color:#6b7177;font-size:11px}
  svg{width:100vw;height:100vh;display:block;cursor:grab}
  .edge{stroke:rgba(255,255,255,0.18);stroke-width:1.2;fill:none;marker-end:url(#arrow)}
  .edge.hard{stroke:#f4625a;stroke-dasharray:5 4}
  .elabel{fill:#6b7177;font-size:10px}
  .node circle{fill:#15181c;stroke:#4ade80;stroke-width:1.4}
  .node text{fill:#e9ebec;font-size:11px;text-anchor:middle}
  .node .params{fill:#6b7177;font-size:9px}
</style></head><body>
<div id="hud"><b id="title"></b> <span class="dim" id="meta"></span><br>
<span class="dim">drag nodes / scroll to zoom / dashed red = irreversible</span></div>
<svg id="s"><defs><marker id="arrow" viewBox="0 0 10 10" refX="22" refY="5"
 markerWidth="7" markerHeight="7" orient="auto-start-reverse">
 <path d="M0,0 L10,5 L0,10 z" fill="rgba(255,255,255,0.28)"/></marker></defs><g id="view"></g></svg>
<script>
const DATA = /*DATA*/;
document.getElementById('title').textContent = DATA.app + ' app map';
document.getElementById('meta').textContent =
  'v' + DATA.version + ' / ' + DATA.nodes.length + ' states / ' + DATA.edges.length + ' transitions';
const W = innerWidth, H = innerHeight;
const N = DATA.nodes.map((n,i)=>({...n,
  x: W/2 + Math.cos(i/DATA.nodes.length*6.283)*Math.min(W,H)/3.2,
  y: H/2 + Math.sin(i/DATA.nodes.length*6.283)*Math.min(W,H)/3.2, vx:0, vy:0}));
const byId = Object.fromEntries(N.map(n=>[n.id,n]));
const E = DATA.edges.filter(e=>byId[e.from]&&byId[e.to]);
for (let it=0; it<400; it++) {           // tiny force sim
  for (const a of N) for (const b of N) { if (a===b) continue;
    let dx=a.x-b.x, dy=a.y-b.y, d2=dx*dx+dy*dy+0.01, f=9000/d2;
    a.vx+=dx*f/Math.sqrt(d2); a.vy+=dy*f/Math.sqrt(d2); }
  for (const e of E) { const a=byId[e.from], b=byId[e.to];
    let dx=b.x-a.x, dy=b.y-a.y, d=Math.sqrt(dx*dx+dy*dy)+0.01, f=(d-190)*0.012;
    a.vx+=dx/d*f*d; a.vy+=dy/d*f*d; b.vx-=dx/d*f*d; b.vy-=dy/d*f*d; }
  for (const n of N) { n.vx+=(W/2-n.x)*0.002; n.vy+=(H/2-n.y)*0.002;
    n.x+=n.vx*0.08; n.y+=n.vy*0.08; n.vx*=0.6; n.vy*=0.6; }
}
const view = document.getElementById('view');
const NS = 'http://www.w3.org/2000/svg';
function el(t,attrs,parent){const e=document.createElementNS(NS,t);
  for(const k in attrs)e.setAttribute(k,attrs[k]);(parent||view).appendChild(e);return e}
function draw(){
  view.innerHTML='';
  for (const e of E){ const a=byId[e.from], b=byId[e.to];
    el('path',{class:'edge'+(e.hard?' hard':''),
      d:`M${a.x},${a.y} Q${(a.x+b.x)/2+(b.y-a.y)*0.12},${(a.y+b.y)/2-(b.x-a.x)*0.12} ${b.x},${b.y}`});
    const t=el('text',{class:'elabel',x:(a.x+b.x)/2+(b.y-a.y)*0.09,y:(a.y+b.y)/2-(b.x-a.x)*0.09});
    t.textContent=e.label; }
  for (const n of N){ const g=el('g',{class:'node','data-id':n.id});
    el('circle',{cx:n.x,cy:n.y,r:17},g);
    const t=el('text',{x:n.x,y:n.y+32},g); t.textContent=n.id;
    const d=el('text',{class:'params',x:n.x,y:n.y+45},g);
    d.textContent=n.label.slice(0,38)+(n.params.length?' ('+n.params.join(',')+')':''); }
}
draw();
let drag=null, pan=null, scale=1, tx=0, ty=0;
const svg=document.getElementById('s');
function apply(){view.setAttribute('transform',`translate(${tx},${ty}) scale(${scale})`)}
svg.addEventListener('mousedown',ev=>{
  const g=ev.target.closest('.node');
  if(g){drag=byId[g.getAttribute('data-id')]}else{pan={x:ev.clientX-tx,y:ev.clientY-ty}}});
svg.addEventListener('mousemove',ev=>{
  if(drag){drag.x=(ev.clientX-tx)/scale;drag.y=(ev.clientY-ty)/scale;draw()}
  else if(pan){tx=ev.clientX-pan.x;ty=ev.clientY-pan.y;apply()}});
addEventListener('mouseup',()=>{drag=null;pan=null});
svg.addEventListener('wheel',ev=>{ev.preventDefault();
  scale=Math.max(0.25,Math.min(3,scale*(ev.deltaY<0?1.1:0.9)));apply()},{passive:false});
</script></body></html>
"##;
