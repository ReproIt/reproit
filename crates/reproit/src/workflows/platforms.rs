//! Human-readable platform support matrix.

use crate::adapters::platform;

pub(crate) fn print() {
    println!("Platform support matrix (UI framework -> introspection backend)\n");
    println!("  {:<16} {:<26} CAPABILITY", "PLATFORM", "BACKEND");
    for platform in platform::all() {
        println!(
            "  {:<16} {:<26} {}",
            platform.id,
            platform.backend.as_str(),
            platform.note
        );
    }
    println!(
        "\n  All listed platform IDs are live. Local readiness still depends on `reproit doctor` \
         and host tooling.\n\n  The point: Qt/GTK/WinUI/Avalonia/wxWidgets share ONE backend per \
         OS\n(they publish to the OS accessibility API), Electron/Tauri reuse the\nweb backend, \
         Appium covers native mobile, and TUI uses a PTY."
    );
}
