//! `reproit skills install`: emit the reproit playbook (the find -> reproduce
//! -> fix -> check loop and journey-authoring knowledge that tool descriptions
//! can't carry) into the coding agent's native format. The PLAYBOOK is
//! harness-neutral; only the packaging differs, so we target FORMATS, not
//! vendors:
//!   - agents: a marker-delimited section in `AGENTS.md`, the cross-agent open
//!     standard (Codex, opencode, Cursor, Aider, Zed, Copilot, Windsurf, ...).
//!   - skill:  the Agent Skills format, a SKILL.md tree (Claude Code /
//!     opencode).
//! The skill sources live in `skills/` at the repo root and are embedded into
//! the binary at compile time, so one source of truth renders to every harness.

use crate::interface::cli::args::SkillFormat;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Markers bounding reproit's block in a shared AGENTS.md, so re-installing
/// replaces our section in place instead of clobbering the user's own content.
const AGENTS_BEGIN: &str = "<!-- BEGIN reproit skills (auto-generated) -->";
const AGENTS_END: &str = "<!-- END reproit skills (auto-generated) -->";

/// (path under skills/, file contents) embedded at compile time. Keep in sync
/// with the `skills/` tree; a missing file is a compile error, not a runtime
/// one.
const FILES: &[(&str, &str)] = &[
    (
        "reproit/SKILL.md",
        include_str!("../../../../skills/reproit/SKILL.md"),
    ),
    (
        "reproit/references/oracles.md",
        include_str!("../../../../skills/reproit/references/oracles.md"),
    ),
    (
        "reproit/references/why.md",
        include_str!("../../../../skills/reproit/references/why.md"),
    ),
    (
        "reproit/references/cloud.md",
        include_str!("../../../../skills/reproit/references/cloud.md"),
    ),
    (
        "reproit/references/configuration.md",
        include_str!("../../../../skills/reproit/references/configuration.md"),
    ),
    (
        "reproit-journeys/SKILL.md",
        include_str!("../../../../skills/reproit-journeys/SKILL.md"),
    ),
    (
        "reproit-journeys/templates/journey.yaml",
        include_str!("../../../../skills/reproit-journeys/templates/journey.yaml"),
    ),
    (
        "reproit-screenshots/SKILL.md",
        include_str!("../../../../skills/reproit-screenshots/SKILL.md"),
    ),
];

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot resolve ~ (pass --dir)")
}

/// Render the playbook into the chosen format. `dir` overrides the default
/// location; `global` selects the user-global location.
pub fn install(format: SkillFormat, global: bool, dir: Option<PathBuf>) -> Result<()> {
    match format {
        SkillFormat::Agents => install_agents(global, dir),
        SkillFormat::Skill => install_skill(global, dir),
    }
}

/// Agent Skills: the SKILL.md tree, one file per embedded source. Overwrites in
/// place so re-running picks up an upgraded binary's playbook. Default location
/// is `.claude/skills`; opencode users can point `--dir` at their skills dir.
fn install_skill(global: bool, dir: Option<PathBuf>) -> Result<()> {
    let base = match dir {
        Some(d) => d,
        None if global => home()?.join(".claude").join("skills"),
        None => PathBuf::from(".claude").join("skills"),
    };
    for (rel, contents) in FILES {
        let dst = base.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&dst, contents).with_context(|| format!("writing {}", dst.display()))?;
    }
    println!(
        "Installed 3 skills ({} files) to {}",
        FILES.len(),
        base.display()
    );
    println!("  reproit             find -> reproduce -> fix -> check loop");
    println!("  reproit-journeys    author single/multi-user scripted journeys");
    println!("  reproit-screenshots author store/marketing screenshot tours");
    Ok(())
}

/// AGENTS.md (the cross-agent standard): flatten the playbook into a delimited
/// section. Idempotent: replaces our marked block if present, appends it
/// otherwise, and never touches the rest of the user's file. Default location
/// is the project-root AGENTS.md (where every AGENTS.md-aware agent looks).
fn install_agents(global: bool, dir: Option<PathBuf>) -> Result<()> {
    let path = match dir {
        Some(d) => d.join("AGENTS.md"),
        None if global => home()?.join("AGENTS.md"),
        None => PathBuf::from("AGENTS.md"),
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = upsert_section(&existing, &agents_section());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, merged).with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote the reproit playbook into {}", path.display());
    Ok(())
}

/// Flatten every embedded skill file (frontmatter stripped) into one markdown
/// block, wrapped in the begin/end markers.
fn agents_section() -> String {
    let mut out = String::new();
    out.push_str(AGENTS_BEGIN);
    out.push_str("\n# reproit (agent playbook)\n");
    for (rel, contents) in FILES {
        out.push_str(&format!("\n<!-- from skills/{rel} -->\n"));
        out.push_str(strip_frontmatter(contents).trim());
        out.push('\n');
    }
    out.push_str(AGENTS_END);
    out.push('\n');
    out
}

/// Drop a leading `---\n...\n---\n` YAML frontmatter block, if present.
fn strip_frontmatter(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return rest[idx + "\n---\n".len()..].trim_start();
        }
    }
    s
}

/// Replace the existing begin..end block in `doc` with `section`; if no markers
/// are present, append `section` (preserving the user's content verbatim).
fn upsert_section(doc: &str, section: &str) -> String {
    if let (Some(start), Some(end)) = (doc.find(AGENTS_BEGIN), doc.find(AGENTS_END)) {
        if end > start {
            let end = end + AGENTS_END.len();
            let mut out = String::new();
            out.push_str(&doc[..start]);
            out.push_str(section.trim_end());
            out.push_str(&doc[end..]);
            return out;
        }
    }
    let mut out = doc.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(section);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::oracle::Oracle;
    use clap::CommandFactory;

    fn all_skill_text() -> String {
        FILES.iter().map(|(_, c)| *c).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn strip_frontmatter_drops_yaml_header() {
        let s = "---\nname: x\ndescription: y\n---\n# Body\ntext";
        assert_eq!(strip_frontmatter(s), "# Body\ntext");
        assert_eq!(strip_frontmatter("# Body"), "# Body");
    }

    #[test]
    fn agents_section_strips_skill_frontmatter() {
        let section = agents_section();
        assert!(!section.contains("description: >-"));
        assert!(section.contains("# reproit (agent playbook)"));
        // The flattened playbook carries the core loop in (a core verb appears).
        assert!(section.contains("reproit fuzz"));
    }

    #[test]
    fn agents_upsert_is_idempotent_and_preserves_user_content() {
        let user = "# My AGENTS.md\n\nmy own rules\n";
        let once = upsert_section(user, &agents_section());
        let twice = upsert_section(&once, &agents_section());
        // Re-installing replaces our block in place, never duplicates it.
        assert_eq!(once, twice);
        assert_eq!(once.matches(AGENTS_BEGIN).count(), 1);
        // The user's own content survives.
        assert!(once.contains("my own rules"));
    }

    fn skill_file(rel: &str) -> &'static str {
        FILES
            .iter()
            .find(|(p, _)| *p == rel)
            .unwrap_or_else(|| panic!("no embedded skill file {rel}"))
            .1
    }

    // Drift guard: every user-facing oracle must be DOCUMENTED in the oracle
    // reference, so adding an oracle (the code) without updating the playbook
    // (the docs) fails CI instead of silently shipping a stale skill. We require
    // the backtick-quoted tag in oracles.md specifically (not a bare substring
    // anywhere, which an incidental prose mention like "visual flicker" would
    // satisfy).
    #[test]
    fn skills_document_every_oracle() {
        let oracles = skill_file("reproit/references/oracles.md");
        for &o in Oracle::ALL {
            let tag = o.as_str();
            assert!(
                oracles.contains(&format!("`{tag}`")),
                "the `{tag}` oracle is not documented in skills/reproit/references/oracles.md \
                 (expected a `{tag}` table row)"
            );
        }
    }

    // Drift guard, the other direction: the core verbs the skills teach must
    // still exist as real (non-hidden) subcommands AND be referenced as
    // `reproit <verb>` in the skills, so a rename/removal in the CLI fails CI
    // instead of leaving the skill pointing at a dead command.
    #[test]
    fn skill_core_commands_exist_and_are_documented() {
        const CORE: &[&str] = &[
            "scan",
            "fuzz",
            "check",
            "keep",
            "create",
            "push",
            "repro",
            "repros",
            "watch",
            "auth",
            "journey",
            "login",
            "bugs",
            "triage",
            "timeline",
            "resolution-events",
        ];
        let text = all_skill_text();
        let cmd = crate::interface::cli::args::Cli::command();
        let names: Vec<&str> = cmd
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name())
            .collect();
        for verb in CORE {
            assert!(
                names.contains(verb),
                "skills reference `reproit {verb}` but it is not a CLI subcommand \
                 (renamed/removed?); update the skills or the CORE list"
            );
            assert!(
                text.contains(&format!("reproit {verb}")),
                "`reproit {verb}` is a core verb but no skill references it"
            );
        }
    }
}
