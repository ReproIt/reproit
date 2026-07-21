use super::*;

// multi-actor scenario client (the conductor protocol).

fn barrier_hit(base: &str, method: &str, path: &str) -> Option<String> {
    let addr = base.trim_end_matches('/');
    let addr = addr.strip_prefix("http://").unwrap_or(addr);
    let mut sock = std::net::TcpStream::connect(addr).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        sock,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    sock.flush().ok()?;
    let mut raw = String::new();
    sock.read_to_string(&mut raw).ok()?;
    Some(
        raw.split_once("\r\n\r\n")
            .map(|(_, body)| body.trim().to_string())
            .unwrap_or_default(),
    )
}

fn find_typable(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
    finder: &str,
) -> Option<IUIAutomationElement> {
    let want = finder.strip_prefix("key:").unwrap_or(finder);
    // Depth-first search for the first control whose AutomationId or Name matches
    // and that accepts text via the Value pattern.
    let mut stack = vec![window.clone()];
    while let Some(el) = stack.pop() {
        let aid = el_automation_id(&el);
        let name = el_name(&el);
        let matches_id = aid
            .as_deref()
            .map(|a| a == want || a == finder)
            .unwrap_or(false);
        let matches_name = !name.is_empty() && name == want;
        if (matches_id || matches_name)
            && get_pattern::<IUIAutomationValuePattern>(&el, UIA_ValuePatternId.0).is_some()
        {
            return Some(el);
        }
        for child in children_of(automation, &el) {
            stack.push(child);
        }
    }
    None
}

fn observe_scenario(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
    value_selectors: &[String],
    cap: &mut ValueCap,
    seen: &mut BTreeSet<String>,
) -> Snapshot {
    // LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT ported
    // to the Windows UIA backend: a desktop window has no device orientation to
    // rotate, and this backend drives the app by walking the UIA tree and clicking
    // -- it has no app-lifecycle background/foreground hook (minimizing is a
    // window-manager action, not a paused->resumed lifecycle, and a minimized
    // window's UIA tree is unavailable), so the ground truth those oracles need
    // cannot be produced here.
    let snap = snapshot(automation, window, value_selectors, cap);
    let observation_labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
    emit(&crate::domain::runner::observation_frame_line(
        &serde_json::json!({
            "sig": snap.sig,
            "labels": observation_labels,
            "elements": snap.elements
        }),
    ));
    if seen.insert(snap.sig.clone()) {
        let labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
        emit(&format!(
            "EXPLORE:STATE {}",
            serde_json::json!({ "sig": snap.sig, "labels": labels, "elements": snap.elements })
        ));
    }
    snap
}

pub(super) fn run_scenario_actor(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
    value_selectors: &[String],
    cap: &mut ValueCap,
    base: &str,
) -> Result<()> {
    let mut role = std::env::var("REPROIT_DEVICE").unwrap_or_default();
    if role.is_empty() {
        role = match barrier_hit(base, "GET", "/claim") {
            Some(r) if !r.is_empty() && !r.starts_with("ERR") => r,
            _ => "a".to_string(),
        };
    }
    emit(&format!("JOURNEY claimed role={role}"));
    std::thread::sleep(Duration::from_millis(900));

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut failed = false;
    let mut current = observe_scenario(automation, window, value_selectors, cap, &mut seen);

    for _ in 0..100_000u32 {
        let body = match barrier_hit(base, "GET", &format!("/next?device={role}")) {
            Some(b) => b,
            None => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if body == "DONE" {
            break;
        }
        if body == "WAIT" {
            std::thread::sleep(Duration::from_millis(40));
            continue;
        }
        let act = body.strip_prefix("ACT\t").unwrap_or(&body).to_string();
        emit(&crate::domain::runner::action_frame_line(Some(&role), &act));
        // Bring this actor's own window forward before acting.
        unsafe {
            let _ = SetForegroundWindow(window_hwnd(window));
        }

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(window, name);
        } else if let Some(a) = act.strip_prefix("assert:") {
            let fresh = snapshot(automation, window, value_selectors, cap);
            let contents = fresh.labels.join("\n");
            if let Some(want) = a.strip_prefix("text=") {
                let ok = contents.contains(want);
                emit(&format!(
                    "FUZZ:ASSERT {} text={} actor={role}",
                    if ok { "pass" } else { "fail" },
                    serde_json::json!(want)
                ));
            } else if let Some(rest) = a.strip_prefix("count:") {
                let (finder, want) = rest.rsplit_once('=').unwrap_or((rest, "0"));
                let want: i64 = want.parse().unwrap_or(0);
                let got = if finder.is_empty() {
                    0
                } else {
                    contents.matches(finder).count() as i64
                };
                emit(&format!(
                    "FUZZ:ASSERT {} count {finder} want={want} got={got} actor={role}",
                    if got == want { "pass" } else { "fail" }
                ));
            } else {
                emit(&format!("FUZZ:ASSERT fail unsupported {a} actor={role}"));
            }
        } else if let Some(acct) = act.strip_prefix("auth:") {
            emit(&format!(
                "JOURNEY[a] step: auth-restore unsupported on desktop-uia runner; drive the login \
                 UI explicitly for auth:{acct}"
            ));
        } else if act == "back" {
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(b) = act.strip_prefix("type:") {
            let (finder, value) = b.rsplit_once('=').unwrap_or((b, ""));
            match find_typable(automation, window, finder) {
                Some(ctrl) if set_text(&ctrl, value) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(label) = act.strip_prefix("tap:") {
            let fresh = snapshot(automation, window, value_selectors, cap);
            match fresh.nodes.get(label) {
                Some(node) if press(node) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(700));
        } else {
            emit(&format!("FUZZ:MISS {role} {act}"));
        }

        if !window_exists(window) {
            crash(
                "target window gone",
                &format!("the window vanished during {act}"),
            );
            failed = true;
            break;
        }
        let nxt = observe_scenario(automation, window, value_selectors, cap, &mut seen);
        if nxt.sig != current.sig {
            emit(&format!(
                "EXPLORE:EDGE {}",
                serde_json::json!({ "from": current.sig, "action": act, "to": nxt.sig })
            ));
        }
        current = nxt;
        let _ = barrier_hit(base, "POST", &format!("/done?device={role}"));
    }

    emit("JOURNEY DONE");
    emit(if failed {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}

struct PidWindowSearch {
    pid: u32,
    hwnd: HWND,
}

unsafe extern "system" fn enum_pid_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam.0 as *mut PidWindowSearch);
    let mut owner = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut owner));
    if owner == search.pid && IsWindowVisible(hwnd).as_bool() {
        search.hwnd = hwnd;
        return false.into();
    }
    true.into()
}

// Find the first visible top-level HWND owned by `pid`, retried until
// `timeout`. EnumWindows is used instead of walking UIA's desktop root: a
// broken provider elsewhere on the desktop can block FindAll and must not hang
// attachment to the process Reproit itself just launched.
pub(super) fn window_for_pid(
    automation: &IUIAutomation,
    pid: u32,
    timeout: Duration,
) -> Option<IUIAutomationElement> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut search = PidWindowSearch {
            pid,
            hwnd: HWND::default(),
        };
        let _ = unsafe {
            EnumWindows(
                Some(enum_pid_window),
                LPARAM((&mut search as *mut PidWindowSearch) as isize),
            )
        };
        if !search.hwnd.0.is_null() {
            if let Ok(window) = unsafe { automation.ElementFromHandle(search.hwnd) } {
                return Some(window);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}
