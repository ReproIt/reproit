//! Provider-agnostic "llm": the seam that makes the LLM hot-swappable.
//!
//! The abstraction sits at the TASK level (prompt in, final text out), not at
//! the wire-API level. Unifying provider wire formats into one type system is
//! the lowest-common-denominator trap; a task-level trait costs nothing and
//! lets CLI agents (which bring their own tool loop, sandbox, and
//! subscription billing) and raw APIs coexist behind one call site.
//!
//! Providers:
//!   - codex-cli   `codex exec`     OpenAI, billed via ChatGPT subscription
//!   - claude-cli  `claude -p`      Anthropic, billed via Claude subscription
//!   - claude-api  raw Messages API (the `claude` module), per-token, for
//!     prod/CI
//!   - openai-api  Chat Completions over raw HTTP, per-token (model required)

// A self-contained Anthropic client (typed Messages API, retries, streaming,
// tool loop). The `claude-api` provider drives only the streaming path today;
// the rest is kept whole for a future agent provider. Exposed as the crate's
// public API so the unused-but-intentional surface doesn't read as dead code.
pub mod claude;
mod providers;

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

pub struct Task {
    pub prompt: String,
    /// CLI providers fold this into the prompt; API providers send it as the
    /// real system prompt.
    pub system: Option<String>,
    /// Working directory for CLI agents (they can read the repo from here).
    pub workdir: Option<PathBuf>,
    /// Allow the agent to EDIT files under workdir (CLI providers only:
    /// codex gets --full-auto, claude gets --permission-mode acceptEdits).
    /// API providers cannot write; gate at the call site via can_write().
    pub write: bool,
}

impl Task {
    pub fn new(prompt: impl Into<String>) -> Self {
        Task {
            prompt: prompt.into(),
            system: None,
            workdir: None,
            write: false,
        }
    }
    pub fn write(mut self) -> Self {
        self.write = true;
        self
    }
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }
    pub fn workdir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workdir = Some(dir.into());
        self
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    /// Availability probe for `reproit doctor` (binary on PATH, key set, ...).
    async fn check(&self) -> std::result::Result<(), String>;
    /// One task in, final text out.
    async fn complete(&self, task: &Task) -> Result<String>;
    /// Can this provider execute write-tasks (edit files in workdir)?
    fn can_write(&self) -> bool {
        false
    }
}

/// Mirrors the `llm:` section of reproit.yaml.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    /// codex-cli | claude-cli | claude-api (default: codex-cli)
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Override the CLI binary path.
    pub bin: Option<String>,
    pub extra_args: Vec<String>,
}

pub fn from_spec(spec: &Spec) -> Result<Box<dyn Provider>> {
    let provider = spec.provider.as_deref().unwrap_or("codex-cli");
    match provider {
        "codex-cli" => Ok(Box::new(providers::CliProvider::codex(spec))),
        "claude-cli" => Ok(Box::new(providers::CliProvider::claude(spec))),
        "claude-api" => Ok(Box::new(providers::ClaudeApiProvider::new(spec))),
        "openai-api" => Ok(Box::new(providers::OpenAiProvider::new(spec)?)),
        other => anyhow::bail!(
            "unknown llm provider {other:?} (expected codex-cli, claude-cli, claude-api, or \
             openai-api)"
        ),
    }
}
