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
//!   - Windows (implemented): Windows Error Reporting's per-user
//!     `HKCU\Software\Microsoft\Windows\Windows Error Reporting\DontShowUI`.
//!     We set only the UI suppression flag, not global WER `Disabled`, then
//!     restore the prior value (or delete it again if it was unset).
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
/// When suppression does not apply (non-native backend or unsupported host OS)
/// the guard is inert: it records that it changed nothing and Drop is a no-op,
/// so a normal run never touches a system setting.
#[must_use = "hold the guard for the duration of the run; dropping it restores the setting"]
pub struct CrashReporterGuard {
    /// The per-OS crash-dialog value to restore on Drop:
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
        #[cfg(target_os = "windows")]
        {
            Self::engage_windows()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            // Linux suppression is not needed for our purpose: headless runs do
            // not show a blocking crash dialog, and apport/abrt log out of band.
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

    /// Windows: suppress Windows Error Reporting UI for this user only. This
    /// avoids native fuzz/check runs getting wedged behind a modal "app stopped
    /// working" dialog while leaving WER collection itself enabled.
    #[cfg(target_os = "windows")]
    fn engage_windows() -> Self {
        let prior = read_wer_dont_show_ui();
        if set_wer_dont_show_ui("1").is_err() {
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
        #[cfg(target_os = "windows")]
        {
            match prior {
                Some(v) => {
                    let _ = set_wer_dont_show_ui(&v);
                }
                None => {
                    let _ = delete_wer_dont_show_ui();
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

#[cfg(target_os = "windows")]
const WER_REG_PATH: &str = r"HKCU\Software\Microsoft\Windows\Windows Error Reporting";

#[cfg(target_os = "windows")]
const WER_DONT_SHOW_UI: &str = "DontShowUI";

/// Read WER `DontShowUI`. Returns the registry data text (usually `0x0` or
/// `0x1`) or None when the value is unset.
#[cfg(target_os = "windows")]
fn read_wer_dont_show_ui() -> Option<String> {
    let out = std::process::Command::new("reg")
        .args(["query", WER_REG_PATH, "/v", WER_DONT_SHOW_UI])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_query_value(&String::from_utf8_lossy(&out.stdout), WER_DONT_SHOW_UI)
}

#[cfg(any(target_os = "windows", test))]
fn parse_reg_query_value(stdout: &str, name: &str) -> Option<String> {
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(name) {
            continue;
        }
        let _kind = parts.next()?;
        let value = parts.next()?;
        return Some(value.to_string());
    }
    None
}

/// Set WER `DontShowUI`. `reg add` accepts decimal (`1`) and hex (`0x1`) data.
#[cfg(target_os = "windows")]
fn set_wer_dont_show_ui(value: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("reg")
        .args([
            "add",
            WER_REG_PATH,
            "/v",
            WER_DONT_SHOW_UI,
            "/t",
            "REG_DWORD",
            "/d",
            value,
            "/f",
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("reg add failed"))
    }
}

/// Delete WER `DontShowUI`, restoring the user's default behavior.
#[cfg(target_os = "windows")]
fn delete_wer_dont_show_ui() -> std::io::Result<()> {
    let status = std::process::Command::new("reg")
        .args(["delete", WER_REG_PATH, "/v", WER_DONT_SHOW_UI, "/f"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("reg delete failed"))
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

    #[test]
    fn parses_windows_reg_query_value() {
        let stdout = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\Windows Error Reporting
    DontShowUI    REG_DWORD    0x1
"#;
        assert_eq!(
            parse_reg_query_value(stdout, "DontShowUI").as_deref(),
            Some("0x1")
        );
        assert!(parse_reg_query_value(stdout, "Disabled").is_none());
    }
}
