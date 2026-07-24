//! Execution adapters for devices, runtimes, and operating systems.

pub(crate) mod cloud_profile;
pub(crate) mod config;
pub(crate) mod crash_reporter;
pub(crate) mod credentials;
pub(crate) mod device;
pub(crate) mod drive;
pub(crate) mod frames;
pub(crate) mod inspect_control;
pub(crate) mod orchestrator;
pub(crate) mod platform;
pub(crate) mod project_scaffold;
pub(crate) mod reset;
pub(crate) mod scoped_env;
pub(crate) mod simctl;
pub(crate) mod tui;
pub(crate) mod update;
pub(crate) mod vmservice;

// Native runners compile only on the host that provides their platform APIs.
#[cfg(all(target_os = "linux", feature = "linux-atspi"))]
pub(crate) mod atspi;
#[cfg(windows)]
pub(crate) mod uia;
