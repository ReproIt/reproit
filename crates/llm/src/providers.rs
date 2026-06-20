use crate::claude;
use crate::{Provider, Spec, Task};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Hard cap on one llm task. Authoring runs are long; hangs are longer.
const TASK_TIMEOUT: Duration = Duration::from_secs(900);

/// How the prompt reaches the CLI.
enum PromptVia {
    /// Last positional argument. Fine for short prompts; long prompts (a
    /// semantics dump) risk ARG_MAX, so prefer stdin where the CLI allows.
    Arg,
    Stdin,
}

pub struct CliProvider {
    name: &'static str,
    bin: String,
    base_args: Vec<String>,
    model: Option<String>,
    extra_args: Vec<String>,
    prompt_via: PromptVia,
}

impl CliProvider {
    /// `codex exec [-m model] [extra] <prompt>` (ChatGPT subscription billing).
    pub fn codex(spec: &Spec) -> Self {
        CliProvider {
            name: "codex-cli",
            bin: spec.bin.clone().unwrap_or_else(|| "codex".into()),
            base_args: vec!["exec".into()],
            model: spec.model.clone(),
            extra_args: spec.extra_args.clone(),
            prompt_via: PromptVia::Arg,
        }
    }

    /// `claude -p [--model model] [extra]` with the prompt on stdin
    /// (Claude subscription billing; headless Claude Code).
    pub fn claude(spec: &Spec) -> Self {
        CliProvider {
            name: "claude-cli",
            bin: spec.bin.clone().unwrap_or_else(|| "claude".into()),
            base_args: vec!["-p".into()],
            model: spec.model.clone(),
            extra_args: spec.extra_args.clone(),
            prompt_via: PromptVia::Stdin,
        }
    }

    fn full_prompt(task: &Task) -> String {
        match &task.system {
            Some(system) => format!("{system}\n\n{}", task.prompt),
            None => task.prompt.clone(),
        }
    }
}

#[async_trait]
impl Provider for CliProvider {
    fn name(&self) -> &str {
        self.name
    }

    async fn check(&self) -> std::result::Result<(), String> {
        let probe = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {}", self.bin))
            .output()
            .await;
        match probe {
            Ok(out) if out.status.success() => Ok(()),
            _ => Err(format!("{} not found on PATH", self.bin)),
        }
    }

    fn can_write(&self) -> bool {
        true
    }

    async fn complete(&self, task: &Task) -> Result<String> {
        let prompt = Self::full_prompt(task);
        let mut cmd = Command::new(&self.bin);
        cmd.args(&self.base_args);
        if task.write {
            match self.name {
                // Sandboxed write access scoped to the workdir.
                "codex-cli" => {
                    cmd.arg("--full-auto");
                }
                "claude-cli" => {
                    cmd.args(["--permission-mode", "acceptEdits"]);
                }
                _ => {}
            }
        }
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
        cmd.args(&self.extra_args);
        if let Some(dir) = &task.workdir {
            cmd.current_dir(dir);
        }
        match self.prompt_via {
            PromptVia::Arg => {
                cmd.arg(&prompt);
                cmd.stdin(Stdio::null());
            }
            PromptVia::Stdin => {
                cmd.stdin(Stdio::piped());
            }
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", self.bin))?;
        if matches!(self.prompt_via, PromptVia::Stdin) {
            let mut stdin = child.stdin.take().context("no stdin handle")?;
            stdin.write_all(prompt.as_bytes()).await?;
            drop(stdin); // EOF so the CLI starts
        }
        let out = tokio::time::timeout(TASK_TIMEOUT, child.wait_with_output())
            .await
            .with_context(|| format!("{} timed out after {TASK_TIMEOUT:?}", self.name))??;
        if !out.status.success() {
            bail!(
                "{} exited with {:?}: {}",
                self.name,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// OpenAI Chat Completions over raw HTTP. Per-token billing. Kept inline (no
/// dedicated `openai` submodule like `claude`) until something needs an OpenAI
/// tool loop; the llm trait only needs complete().
pub struct OpenAiProvider {
    model: String,
}

impl OpenAiProvider {
    pub fn new(spec: &Spec) -> Result<Self> {
        // No default model on purpose: OpenAI model names churn, and a stale
        // hardcoded default fails worse than an explicit config error.
        let Some(model) = spec.model.clone() else {
            bail!("openai-api requires llm.model in reproit.yaml (e.g. model: gpt-5.2)");
        };
        Ok(OpenAiProvider { model })
    }

    fn key() -> std::result::Result<String, String> {
        std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| "OPENAI_API_KEY not set".to_string())
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai-api"
    }

    async fn check(&self) -> std::result::Result<(), String> {
        Self::key().map(|_| ())
    }

    async fn complete(&self, task: &Task) -> Result<String> {
        let key = Self::key().map_err(anyhow::Error::msg)?;
        let base = std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.openai.com".to_string());
        let mut messages = Vec::new();
        if let Some(system) = &task.system {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        messages.push(serde_json::json!({"role": "user", "content": task.prompt}));
        let body = serde_json::json!({"model": self.model, "messages": messages});

        let client = reqwest::Client::builder().timeout(TASK_TIMEOUT).build()?;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let resp = client
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(&key)
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    let v: serde_json::Value = r.json().await?;
                    let content = v
                        .pointer("/choices/0/message/content")
                        .and_then(serde_json::Value::as_str)
                        .context("openai response had no message content")?;
                    return Ok(content.trim().to_string());
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let text = r.text().await.unwrap_or_default();
                    if matches!(status, 429 | 500 | 502 | 503) && attempt < 4 {
                        tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1))).await;
                        continue;
                    }
                    bail!("openai api error {status}: {text}");
                }
                Err(e) if (e.is_connect() || e.is_timeout()) && attempt < 4 => {
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1))).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Raw Messages API via the `claude` module. Per-token billing: the prod path.
pub struct ClaudeApiProvider {
    model: Option<String>,
}

impl ClaudeApiProvider {
    pub fn new(spec: &Spec) -> Self {
        ClaudeApiProvider {
            model: spec.model.clone(),
        }
    }
}

#[async_trait]
impl Provider for ClaudeApiProvider {
    fn name(&self) -> &str {
        "claude-api"
    }

    async fn check(&self) -> std::result::Result<(), String> {
        claude::Client::from_env()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn complete(&self, task: &Task) -> Result<String> {
        let client = claude::Client::from_env()?;
        let mut req = claude::MessagesRequest::new(32_000)
            .thinking_adaptive()
            .effort("high")
            .user(task.prompt.clone());
        if let Some(model) = &self.model {
            req = req.model(model.clone());
        }
        if let Some(system) = &task.system {
            req = req.system(system.clone());
        }
        // Stream: generated artifacts are long and non-streaming requests
        // at this size risk HTTP timeouts.
        let resp = client.messages_stream(&req, &mut |_| {}).await?;
        if resp.is_refusal() {
            return Err(claude::Error::Refusal {
                category: resp.refusal_category(),
            }
            .into());
        }
        Ok(resp.text())
    }
}
