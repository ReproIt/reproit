//! UIA action execution and post-action health signals.

use windows::core::BSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationInvokePattern, IUIAutomationLegacyIAccessiblePattern,
    IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern, IUIAutomationValuePattern,
    UIA_InvokePatternId, UIA_LegacyIAccessiblePatternId, UIA_SelectionItemPatternId,
    UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::IsHungAppWindow;

use super::{emit, get_pattern, HANG_FLOOR_MS};

pub(super) fn press(element: &IUIAutomationElement) -> bool {
    if let Some(pattern) = get_pattern::<IUIAutomationInvokePattern>(element, UIA_InvokePatternId.0)
    {
        if unsafe { pattern.Invoke() }.is_ok() {
            return true;
        }
    }
    if let Some(pattern) = get_pattern::<IUIAutomationTogglePattern>(element, UIA_TogglePatternId.0)
    {
        if unsafe { pattern.Toggle() }.is_ok() {
            return true;
        }
    }
    if let Some(pattern) =
        get_pattern::<IUIAutomationSelectionItemPattern>(element, UIA_SelectionItemPatternId.0)
    {
        if unsafe { pattern.Select() }.is_ok() {
            return true;
        }
    }
    if let Some(pattern) = get_pattern::<IUIAutomationLegacyIAccessiblePattern>(
        element,
        UIA_LegacyIAccessiblePatternId.0,
    ) {
        if unsafe { pattern.DoDefaultAction() }.is_ok() {
            return true;
        }
    }
    false
}

pub(super) fn set_text(element: &IUIAutomationElement, value: &str) -> bool {
    if let Some(pattern) = get_pattern::<IUIAutomationValuePattern>(element, UIA_ValuePatternId.0) {
        if unsafe { pattern.SetValue(&BSTR::from(value)) }.is_ok() {
            return true;
        }
    }
    false
}

pub(super) fn crash(title: &str, detail: &str) {
    emit(&format!(
        "EXCEPTION CAUGHT BY REPROIT \u{2561} {title} \u{255e}"
    ));
    emit(&format!("The following condition was hit: {detail}"));
    emit(&"\u{2550}".repeat(8));
}

pub(super) fn send_escape() {
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_ESCAPE,
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_ESCAPE,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
    }
}

pub(super) fn window_hwnd(window: &IUIAutomationElement) -> HWND {
    unsafe {
        window
            .CurrentNativeWindowHandle()
            .unwrap_or(HWND(std::ptr::null_mut()))
    }
}

pub(super) fn maybe_emit_hang(window: &IUIAutomationElement, from_sig: &str, action: &str) {
    let hwnd = window_hwnd(window);
    if hwnd.0.is_null() {
        return;
    }
    if unsafe { IsHungAppWindow(hwnd) }.as_bool() {
        emit(&format!(
            "EXPLORE:HANG {}",
            serde_json::json!({ "from": from_sig, "action": action, "bucket": HANG_FLOOR_MS })
        ));
    }
}

pub(super) fn window_exists(window: &IUIAutomationElement) -> bool {
    unsafe { window.CurrentProcessId() }.is_ok() && !window_hwnd(window).0.is_null()
}

fn working_set_bytes(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(handle, &mut counters, counters.cb).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        ok.then_some(counters.WorkingSetSize as u64)
    }
}

pub(super) fn sample_rss(pid: u32, elapsed_ms: u64) {
    if let Some(rss) = working_set_bytes(pid) {
        emit(&format!(
            "MEMORY:SAMPLE {}",
            serde_json::json!({ "t_ms": elapsed_ms, "heap_used": rss })
        ));
    }
}
