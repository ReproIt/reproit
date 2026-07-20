//! Reproit's CLI application, domain model, and platform orchestration.
//!
//! The binary entry point is intentionally tiny; application startup lives here
//! so it is exercised by the same library tests as the rest of the CLI.

// These two doc-format lints (new in clippy 1.93) fire on intentionally aligned
// hanging-indent doc tables (e.g. model/repro.rs) whose alignment aids reading.
// Keep the alignment rather than reflow it to satisfy a purely stylistic lint.
#![allow(clippy::doc_overindented_list_items, clippy::doc_lazy_continuation)]

// Narrow public integration surface.
pub mod backend_contracts;
mod interface;

// Domain and platform namespaces.
mod adapters;
mod domain;
mod runtime;
mod workflows;

#[cfg(feature = "perf-bench")]
pub mod perf_bench;

/// Version string stamped by build.rs: a clean `0.1.<commit-count>` for an
/// install / clean build, plus a `(<rev>-dirty <date>)` suffix ONLY for local
/// working builds with uncommitted edits. So `cargo install` shows a plain
/// `0.1.64` while a dev build is obviously identifiable.
pub(crate) const VERSION: &str = env!("REPROIT_VERSION");

pub use runtime::startup::run as startup;

/// Run the CLI from an explicit argument sequence.
pub(crate) async fn run_from<I, T>(args: I) -> anyhow::Result<std::process::ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    workflows::run_from(args).await
}
