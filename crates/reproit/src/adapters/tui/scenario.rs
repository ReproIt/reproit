//! Multi-actor scenario coordination over the bounded conductor protocol.

use super::*;

fn barrier_hit(base: &str, method: &str, path: &str) -> Result<String> {
    let addr = base.trim_end_matches('/');
    let addr = addr.strip_prefix("http://").unwrap_or(addr);
    let mut sock = std::net::TcpStream::connect(addr)
        .with_context(|| format!("connecting to conductor at {addr}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        sock,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;
    let mut raw = String::new();
    sock.read_to_string(&mut raw)?;
    Ok(raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or_default())
}

/// Emit EXPLORE:STATE for a newly seen screen and return its signature, the
/// scenario-side twin of the fuzz loop's emit_state (states a scenario reaches,
/// often only reachable with a peer acting, still land in the map).
fn observe_scenario(parser: &Arc<Mutex<vt100::Parser>>, seen: &mut BTreeSet<String>) -> String {
    let (sig, _fp, labels) = snapshot(parser);
    let observation = serde_json::json!({
        "sig": sig,
        "labels": labels,
        "elements": structural_input_elements()
    });
    emit(&crate::domain::runner::observation_frame_line(&observation));
    if seen.insert(sig.clone()) {
        let payload = serde_json::json!({
            "sig": sig,
            "labels": labels,
            "elements": structural_input_elements()
        });
        emit(&format!("EXPLORE:STATE {payload}"));
    }
    sig
}

/// Play ONE actor of a multi-user scenario: launch the app in a PTY, then loop
/// pulling this actor's next action from the conductor and acking completion,
/// so N runner processes interleave exactly as the journey specifies. The
/// terminal action vocabulary:
///   key:<Name>            press the key (same alphabet as fuzz/replay)
///   type:<finder>=<v>     type <v> literally; a PTY has ONE input channel
///                         (the keyboard), so the finder names intent, not a
///                         target to locate
///   back                  Esc, the universal "leave this screen" key
///   shoot:<name>          screenshot point (same contract as replay)
///   assert:text=<t>       the visible screen contains <t>
///   assert:count:<f>=<n>  the visible screen shows <f> exactly <n> times
///   auth:<acct>           unsupported (a terminal has no session store to
///                         restore); loud no-op so ordering still advances
/// Anything else (a `tap:<sel>` authored for a pointer surface) is a FUZZ:MISS,
/// so a stale or cross-surface journey fails loudly instead of silently
/// passing. Crash detection is the same oracle as fuzzing (a rendered panic,
/// or a panic/signal exit).
pub(super) fn run_scenario_actor(cmdline: &str, base: &str) -> Result<()> {
    // Role identity: the per-process env label wins (each TUI actor is its own
    // process with its own env, so the label is reliable, unlike a shared-build
    // simulator); a runner without one claims a distinct role atomically.
    let mut role = std::env::var("REPROIT_DEVICE").unwrap_or_default();
    if role.is_empty() {
        role = match barrier_hit(base, "GET", "/claim") {
            Ok(r) if !r.is_empty() && !r.starts_with("ERR") => r,
            _ => "a".to_string(),
        };
    }
    emit(&format!("JOURNEY claimed role={role}"));

    let (_master, mut child, parser, writer, _erases, _mouse) = spawn_session(cmdline)?;
    std::thread::sleep(Duration::from_millis(900));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cur_sig = observe_scenario(&parser, &mut seen);
    let mut failed = false;

    'actor: for _guard in 0..100_000u32 {
        let body = match barrier_hit(base, "GET", &format!("/next?device={role}")) {
            Ok(b) => b,
            Err(_) => {
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

        if let Some(name) = act.strip_prefix("shoot:") {
            // Screenshot point: capture the current screen, no state move.
            shoot(&parser, name);
        } else if let Some(a) = act.strip_prefix("assert:") {
            let contents = parser.lock().unwrap().screen().contents();
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
                "JOURNEY[a] step: auth-restore unsupported on tui runner; drive the login keys \
                 explicitly for auth:{acct}"
            ));
        } else {
            // Input actions: keystrokes into the PTY.
            let bytes: Vec<u8> = if act == "back" {
                b"\x1b".to_vec()
            } else if let Some(a) = act.strip_prefix("type:") {
                let value = a.rsplit_once('=').map(|(_, v)| v).unwrap_or(a);
                value.as_bytes().to_vec()
            } else if let Some(key) = act.strip_prefix("key:") {
                let b = bytes_for_key(&parser, key);
                if b.is_empty() {
                    emit(&format!("FUZZ:MISS {role} {act}"));
                }
                b
            } else {
                emit(&format!("FUZZ:MISS {role} {act}"));
                Vec::new()
            };
            if !bytes.is_empty() {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
            std::thread::sleep(Duration::from_millis(260));
        }

        // Crash oracle, same rules as fuzzing: a panic rendered on screen, or
        // the process dying with a panic/signal code. A crashed actor cannot
        // continue, and we deliberately do NOT ack the step, so the conductor's
        // diagnose() names this actor and action as the stall point.
        if looks_crashed(&parser) {
            emit("EXCEPTION CAUGHT BY TUI APP");
            emit("The following crash was rendered to the terminal:");
            for line in parser.lock().unwrap().screen().contents().lines().take(12) {
                if !line.trim().is_empty() {
                    emit(line.trim_end());
                }
            }
            emit("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
            failed = true;
            let _ = child.kill();
            break 'actor;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let code = status.exit_code();
            if code == 101 || code >= 128 {
                emit("EXCEPTION CAUGHT BY TUI APP");
                emit(&format!(
                    "The process crashed (exit code {code}) after {act}"
                ));
                emit("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
            } else {
                // A clean quit mid-scenario still strands the remaining steps
                // (and this actor's peers), so it fails the run; relaunching
                // would silently resume from the start screen, not the state
                // the scenario was in.
                emit(&format!(
                    "JOURNEY[a] step: app exited (code {code}) before the scenario finished"
                ));
            }
            failed = true;
            break 'actor;
        }

        let next_sig = observe_scenario(&parser, &mut seen);
        if next_sig != cur_sig {
            let payload = serde_json::json!({ "from": cur_sig, "action": act, "to": next_sig });
            emit(&format!("EXPLORE:EDGE {payload}"));
        }
        cur_sig = next_sig;

        let _ = barrier_hit(base, "POST", &format!("/done?device={role}"));
    }

    let _ = child.kill();
    emit("JOURNEY DONE");
    emit(if failed {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}
