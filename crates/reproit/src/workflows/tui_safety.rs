//! Confirmation boundary for driving arbitrary terminal applications.

use crate::interface::cli::context::Ctx;

/// TUI fuzzing drives a real process with synthetic keystrokes and can cause
/// real side effects. Always warn, require `--yes` in non-interactive use, and
/// otherwise obtain explicit terminal confirmation.
pub(super) fn confirm_tui_fuzz(ctx: &Ctx, executable: &str) -> bool {
    eprintln!(
        "  WARNING: reproit will drive `{executable}` in a PTY by sending SYNTHETIC \
         KEYSTROKES.\n  A real terminal app can have real side effects (send messages, run \
         shell\n  commands, write or delete files). Point it at a THROWAWAY / sandboxed instance."
    );
    if ctx.yes {
        return true;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("  Refusing without confirmation. Re-run with --yes to proceed.");
        return false;
    }
    use std::io::Write;
    eprint!("  Proceed? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut response = String::new();
    if std::io::stdin().read_line(&mut response).is_err() {
        return false;
    }
    matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
