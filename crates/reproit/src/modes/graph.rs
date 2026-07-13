//! App-map visualization: render the state graph for humans.
//!
//! - mermaid: paste into FigJam (native Mermaid import), GitHub markdown,
//!   or docs. The design-team artifact: a living user-flow diagram.
//! - dot: Graphviz for pipelines that want it.
//!
//! The input is reproit's learned internal app model or explicit appmap JSON
//! (schema: src/appmap.rs); see examples/appmap.example.json.

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
        other => anyhow::bail!("unknown format {other:?} (mermaid | dot)"),
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
