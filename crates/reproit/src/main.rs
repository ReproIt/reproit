//! Thin process entry point for the Reproit CLI.

use anyhow::Result;
use std::process::ExitCode;

fn main() -> Result<ExitCode> {
    reproit::startup()
}
