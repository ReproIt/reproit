//! Domain data and deterministic analysis.

pub(crate) mod appmap;
pub(crate) mod attribute;
pub(crate) mod authority;
pub(crate) mod backend;
pub(crate) mod candidate;
pub(crate) mod capsule;
pub(crate) mod contracts;
pub(crate) mod evidence;
pub(crate) mod execution;
pub(crate) mod fault;
pub(crate) mod fixture;
pub(crate) mod hash;
pub(crate) mod invariants;
pub(crate) mod json_path;
pub(crate) mod locale;
pub(crate) mod map;
#[cfg(any(test, windows, all(target_os = "linux", feature = "linux-atspi")))]
pub(crate) mod native_oracle;
pub(crate) mod observation;
pub(crate) mod oracle;
pub(crate) mod overflow;
pub(crate) mod repro;
pub(crate) mod route_access;
pub(crate) mod runner;
pub(crate) mod signature;
pub(crate) mod target;

#[cfg(test)]
mod boundary_tests;
