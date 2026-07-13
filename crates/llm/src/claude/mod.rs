//! Thin internal Claude API client: the `claude-api` provider's wire layer.
//!
//! Scope rules (resist drift toward a general-purpose SDK):
//!   - Only the surface reproit actually uses: Messages API, retries,
//!     streaming, the tool loop.
//!   - Types tolerate unknown content blocks (kept as raw JSON and echoed
//!     back verbatim), so API additions and protected thinking blocks never
//!     break us.
//!   - Every wire-level constant (base URL, version header, auth headers,
//!     default model) lives in this module and nowhere else.
//!
//! There is no official Rust SDK; raw HTTP against POST /v1/messages is the
//! sanctioned path. A future `agent` module may alternatively shell out to
//! headless Claude Code (`claude -p`) behind the same call sites.

mod client;
mod error;
mod runner;
mod stream;
mod types;

pub use client::Client;
pub use error::Error;
pub use runner::{ToolOutcome, MAX_TOOL_ITERATIONS};
pub use types::*;

pub type Result<T> = std::result::Result<T, Error>;
