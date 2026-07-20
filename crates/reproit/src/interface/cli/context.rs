//! Stable process-exit and output policy shared by command adapters.

use crate::domain::repro;
use std::process::ExitCode;

/// Central exit-code contract used by CI and integrations.
#[derive(Clone, Copy)]
#[allow(dead_code)] // Clean is the implicit success path but part of the contract.
pub(crate) enum Exit {
    Clean = 0,
    Regression = 1,
    Flaky = 2,
    Stale = 3,
}

impl Exit {
    pub(crate) fn code(self) -> ExitCode {
        ExitCode::from(self as u8)
    }
}

impl From<repro::Outcome> for Exit {
    fn from(outcome: repro::Outcome) -> Self {
        match outcome {
            repro::Outcome::Pass => Self::Clean,
            repro::Outcome::Fail => Self::Regression,
            repro::Outcome::Flaky => Self::Flaky,
            repro::Outcome::Stale => Self::Stale,
        }
    }
}

/// The single conversion point from an application outcome to a process code.
pub(crate) fn exit_with(exit: Exit) -> ExitCode {
    exit.code()
}

/// Global output and confirmation policy carried by command workflows.
#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    pub(crate) json: bool,
    pub(crate) quiet: bool,
    pub(crate) yes: bool,
}

impl Ctx {
    /// Print a human line unless quiet or JSON output was requested.
    pub(crate) fn say(&self, line: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            println!("{line}");
        }
    }

    /// Emit one JSON document when machine output was requested.
    pub(crate) fn emit(&self, value: &serde_json::Value) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
            );
        }
    }

    pub(crate) fn confirmed(&self) -> bool {
        self.yes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(exit_with(Exit::Clean), ExitCode::from(0));
        assert_eq!(exit_with(Exit::Regression), ExitCode::from(1));
        assert_eq!(exit_with(Exit::Flaky), ExitCode::from(2));
        assert_eq!(exit_with(Exit::Stale), ExitCode::from(3));
    }
}
