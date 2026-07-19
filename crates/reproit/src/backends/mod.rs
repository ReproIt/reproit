//! Execution adapters for devices, runtimes, and process orchestration.

pub(crate) mod drive;
pub(crate) mod frames;
pub(crate) mod orchestrator;
pub(crate) mod platform;
pub(crate) mod reset;
pub(crate) mod simctl;
pub(crate) mod tui;
pub(crate) mod vmservice;

// Native runners compile only on the host that provides their platform APIs.
#[cfg(all(target_os = "linux", feature = "linux-atspi"))]
pub(crate) mod atspi;
#[cfg(windows)]
pub(crate) mod uia;
