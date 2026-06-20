//! Suppress the OS crash-reporter dialog for the duration of a NATIVE fuzz run.
//!
//! A fuzz that finds N crashes in a native app pops N OS crash dialogs (the
//! macOS "AppName quit unexpectedly" panel, the Windows Error Reporting prompt).
//! On an unattended/CI run that wedges the machine. So for native backends that
//! drive a real app process which can crash (desktop AX/UIA/AT-SPI, Appium), we
//! turn the dialog off for the run and RESTORE the prior setting afterward, even
//! if the run panics or is interrupted (RAII / Drop).
//!
//! This is SCOPED: only native/desktop+appium runs touch a system setting. A
//! web/headless run (Playwright, Flutter headless) never does, since there is no
//! OS crash dialog to suppress there.
//!
//! Per-OS knobs:
//!   - macOS (implemented): `com.apple.CrashReporter DialogType`. We read the
//!     current value (which may be UNSET), set it to `none` for the run, and on
//!     teardown restore the prior value (or DELETE the key if it was unset).
//!   - Windows (documented hook, not implemented here): Windows Error Reporting.
//!     The analogous knob is `HKCU\Software\Microsoft\Windows\Windows Error
//!     Reporting\DontShowUI = 1` (and/or `Disabled = 1`), restored on teardown
//!     the same way. A WER-suppression guard would slot in beside the macOS one.
//!   - Linux (no action needed): a headless run under Xvfb has no GUI crash
//!     dialog. apport/abrt are daemons that log rather than block, so there is
//!     no modal to suppress for our purpose.

use crate::platform::Backend;

/// Whether a backend runs a NATIVE target process whose crash would pop an OS
/// crash-reporter dialog, so the suppression guard should engage. Web/headless
/// backends (WebCdp, FlutterDrive's headless tier) and the in-process/terminal
/// backends do not, so they are left untouched (the scoping requirement).
pub fn suppresses_for(backend: Backend) -> bool {
    match backend {
        // Native desktop apps driven via the OS accessibility API: a crash pops
        // the OS crash dialog. These are exactly the cases the user authorized.
        Backend::DesktopAx | Backend::DesktopUia | Backend::DesktopAtspi => true,
        // Appium drives a real native iOS/Android/RN app; a native crash there
        // can also surface an OS report on a desktop-attached device/simulator.
        Backend::Appium => true,
        // Web (CDP/WebDriver) and the Flutter headless tier have no OS crash
        // dialog; the immediate-mode + terminal backends run in-process / in a
        // PTY. None of these touch a system setting.
        Backend::WebCdp | Backend::FlutterDrive | Backend::Instrumented | Backend::Tui => false,
    }
}

/// An RAII guard that suppresses the OS crash-reporter dialog while it is alive
/// and restores the prior state on Drop (so restore happens on normal return,
/// early return, `?`, or panic unwinding). Construct it with `engage` for the
/// run's backend; hold it for the run's lifetime.
///
/// When suppression does not apply (non-native backend, or a non-macOS host
/// today) the guard is inert: it records that it changed nothing and Drop is a
/// no-op, so a normal run never touches a system setting.
#[must_use = "hold the guard for the duration of the run; dropping it restores the setting"]
pub struct CrashReporterGuard {
    /// The macOS DialogType value to restore on Drop:
    ///   - None        -> we changed nothing (inert guard); Drop is a no-op.
    ///   - Some(None)   -> the key was UNSET before; Drop deletes it again.
    ///   - Some(Some(v))-> the key was `v` before; Drop restores `v`.
    restore: Option<Option<String>>,
}

impl CrashReporterGuard {
    /// Engage suppression for a run on `backend`. Returns an inert guard when
    /// suppression does not apply to this backend or host OS, so callers can
    /// unconditionally `let _guard = engage(...)`.
    pub fn engage(backend: Backend) -> Self {
        if !suppresses_for(backend) {
            return Self { restore: None };
        }
        #[cfg(target_os = "macos")]
        {
            Self::engage_macos()
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Windows/Linux suppression is documented above but not implemented
            // here; the guard is inert so nothing is touched.
            Self { restore: None }
        }
    }

    /// An always-inert guard (changes nothing, Drop is a no-op). For callers
    /// that cannot resolve a backend (e.g. an unknown platform id) but still
    /// want the uniform `let _guard = ...` shape.
    pub fn engage_inert() -> Self {
        Self { restore: None }
    }

    /// macOS: read the current `com.apple.CrashReporter DialogType` (which may be
    /// unset), set it to `none`, and remember the prior state for restore. A
    /// failure to read/write is non-fatal: we just leave the guard inert rather
    /// than failing the fuzz run over a cosmetic setting.
    #[cfg(target_os = "macos")]
    fn engage_macos() -> Self {
        let prior = read_dialog_type();
        // Set it to `none` for the run.
        if set_dialog_type("none").is_err() {
            // Could not write the setting; do not claim to restore something we
            // never changed. Inert guard.
            return Self { restore: None };
        }
        Self {
            restore: Some(prior),
        }
    }
}

impl Drop for CrashReporterGuard {
    fn drop(&mut self) {
        let Some(prior) = self.restore.take() else {
            return; // inert: changed nothing.
        };
        #[cfg(target_os = "macos")]
        {
            match prior {
                Some(v) => {
                    let _ = set_dialog_type(&v);
                }
                None => {
                    let _ = delete_dialog_type();
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = prior;
        }
    }
}

/// Read `com.apple.CrashReporter DialogType`. Returns the trimmed value, or None
/// when the key is unset (the common default).
#[cfg(target_os = "macos")]
fn read_dialog_type() -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "com.apple.CrashReporter", "DialogType"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // key unset -> `defaults read` exits non-zero.
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Set `com.apple.CrashReporter DialogType` to `value`.
#[cfg(target_os = "macos")]
fn set_dialog_type(value: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("defaults")
        .args(["write", "com.apple.CrashReporter", "DialogType", value])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("defaults write failed"))
    }
}

/// Delete the `com.apple.CrashReporter DialogType` key (restore to "unset").
#[cfg(target_os = "macos")]
fn delete_dialog_type() -> std::io::Result<()> {
    let status = std::process::Command::new("defaults")
        .args(["delete", "com.apple.CrashReporter", "DialogType"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("defaults delete failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_backends_suppress_web_and_headless_do_not() {
        // The native target processes that pop an OS crash dialog.
        assert!(suppresses_for(Backend::DesktopAx));
        assert!(suppresses_for(Backend::DesktopUia));
        assert!(suppresses_for(Backend::DesktopAtspi));
        assert!(suppresses_for(Backend::Appium));
        // Web / Flutter-headless / in-process / terminal: never touched.
        assert!(!suppresses_for(Backend::WebCdp));
        assert!(!suppresses_for(Backend::FlutterDrive));
        assert!(!suppresses_for(Backend::Instrumented));
        assert!(!suppresses_for(Backend::Tui));
    }

    #[test]
    fn non_native_backend_yields_an_inert_guard() {
        // A web run must NOT touch any system setting: the guard records no
        // restore state and its Drop is a no-op.
        let g = CrashReporterGuard::engage(Backend::WebCdp);
        assert!(g.restore.is_none(), "web run must not arm any restore");
        drop(g); // no panic, no system mutation.

        let g2 = CrashReporterGuard::engage(Backend::FlutterDrive);
        assert!(g2.restore.is_none());
    }
}
