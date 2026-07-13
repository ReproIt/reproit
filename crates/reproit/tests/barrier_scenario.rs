//! End-to-end multi-actor barrier: the TUI backend's scenario client speaks
//! the conductor protocol (`GET /claim` + `GET /next` + `POST /done`) through
//! the real binary. A tiny in-test conductor serves an interleaved two-actor
//! script; two `reproit __tui` processes (driving `sleep`, a blank inert
//! screen, same trick as the determinism e2e) must each pull exactly their own
//! actions, in the global order, and ack every step. This pins the wire
//! contract every backend's runner implements (web/electron/tauri/flutter/
//! appium/desktop-ax/desktop-uia/desktop-atspi/instrumented/tui), from the
//! runner side; modes/barrier.rs pins the conductor side.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// What the in-test conductor observed: each action it served (in order) and
/// each `/done` ack it received (in order), so the test can assert strict
/// global ordering, not just per-actor completion.
#[derive(Default)]
struct Observed {
    served: Vec<String>,
    acked: Vec<String>,
}

/// A minimal conductor speaking the same wire protocol as modes/barrier.rs:
/// `/claim` hands out roles in order, `/next` enforces the join barrier and the
/// strict step order, `/done` advances. Runs until the script completes.
fn start_conductor(script: Vec<(usize, &'static str)>, n: usize) -> (u16, Arc<Mutex<Observed>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind conductor");
    let port = listener.local_addr().unwrap().port();
    let observed = Arc::new(Mutex::new(Observed::default()));
    let obs = observed.clone();
    let state = Arc::new(Mutex::new((0usize, false, vec![false; n], 0usize))); // (cursor, served, joined, claimed)
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { break };
            let mut buf = [0u8; 1024];
            let Ok(len) = sock.read(&mut buf) else {
                continue;
            };
            let req = String::from_utf8_lossy(&buf[..len]).to_string();
            let line = req.lines().next().unwrap_or("").to_string();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let dev = path
                .split("device=")
                .nth(1)
                .and_then(|s| s.chars().next())
                .map(|c| (c as u8).wrapping_sub(b'a') as usize);
            let body = {
                let mut s = state.lock().unwrap();
                if path.starts_with("/claim") {
                    let role = s.3;
                    s.3 += 1;
                    if role < n {
                        s.2[role] = true;
                        ((b'a' + role as u8) as char).to_string()
                    } else {
                        "ERR full".to_string()
                    }
                } else if let Some(dev) = dev {
                    if dev < n {
                        s.2[dev] = true;
                    }
                    if path.starts_with("/next") {
                        if s.0 >= script.len() {
                            "DONE".to_string()
                        } else if !s.2.iter().all(|&j| j) || script[s.0].0 != dev {
                            "WAIT".to_string()
                        } else {
                            if !s.1 {
                                s.1 = true;
                                obs.lock().unwrap().served.push(format!(
                                    "{}:{}",
                                    (b'a' + dev as u8) as char,
                                    script[s.0].1
                                ));
                            }
                            format!("ACT\t{}", script[s.0].1)
                        }
                    } else if method == "POST" && path.starts_with("/done") {
                        if s.0 < script.len() && script[s.0].0 == dev && s.1 {
                            obs.lock().unwrap().acked.push(format!(
                                "{}:{}",
                                (b'a' + dev as u8) as char,
                                script[s.0].1
                            ));
                            s.0 += 1;
                            s.1 = false;
                        }
                        "OK".to_string()
                    } else {
                        "ERR bad-request".to_string()
                    }
                } else {
                    "ERR no-device".to_string()
                }
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
    });
    (port, observed)
}

/// Spawn one `reproit __tui` scenario actor against the conductor. `label` is
/// the per-process device env (None exercises the `/claim` path).
fn spawn_actor(port: u16, label: Option<&str>) -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_reproit"));
    cmd.arg("__tui")
        .env("REPROIT_TUI_CMD", "sleep 30")
        .env(
            "REPROIT_SCENARIO_BARRIER",
            format!("http://127.0.0.1:{port}"),
        )
        .env_remove("REPROIT_FUZZ_CONFIG")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    match label {
        Some(l) => {
            cmd.env("REPROIT_DEVICE", l);
        }
        None => {
            cmd.env_remove("REPROIT_DEVICE");
        }
    }
    cmd.spawn().expect("spawn reproit __tui actor")
}

fn stdout_of(child: std::process::Child) -> String {
    let out = child.wait_with_output().expect("actor output");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn two_tui_actors_interleave_in_the_scripted_order() {
    // alice, bob, alice: the conductor enforces this global order; the actors
    // only ever see their own steps.
    let script = vec![(0, "key:Down"), (1, "key:Right"), (0, "key:Up")];
    let (port, observed) = start_conductor(script, 2);
    let a = spawn_actor(port, Some("a"));
    let b = spawn_actor(port, Some("b"));
    let out_a = stdout_of(a);
    let out_b = stdout_of(b);

    // Each actor executed exactly its own actions and reported the shared
    // completion markers the orchestrator keys on.
    assert!(out_a.contains("JOURNEY claimed role=a"), "{out_a}");
    assert!(out_a.contains("FUZZ:ACT a key:Down"), "{out_a}");
    assert!(out_a.contains("FUZZ:ACT a key:Up"), "{out_a}");
    assert!(
        !out_a.contains("key:Right"),
        "bob's step leaked to a\n{out_a}"
    );
    assert!(out_a.contains("JOURNEY DONE"), "{out_a}");
    assert!(out_a.contains("All tests passed"), "{out_a}");
    assert!(out_b.contains("JOURNEY claimed role=b"), "{out_b}");
    assert!(out_b.contains("FUZZ:ACT b key:Right"), "{out_b}");
    assert!(
        !out_b.contains("key:Down"),
        "alice's step leaked to b\n{out_b}"
    );
    assert!(out_b.contains("JOURNEY DONE"), "{out_b}");

    // The conductor saw every step served AND acked, in the global order: the
    // strict-interleaving promise, observed from the wire.
    let obs = observed.lock().unwrap();
    let want = vec!["a:key:Down", "b:key:Right", "a:key:Up"];
    assert_eq!(obs.served, want, "serve order");
    assert_eq!(obs.acked, want, "ack order");
}

#[test]
fn every_barrier_speaking_backend_ships_a_conductor_client() {
    // `Backend::speaks_barrier` (backends/platform.rs) promises a conductor
    // client per supporting backend. Pin that promise to the shipped runner
    // sources so the flag cannot silently rot: every runner behind a `true`
    // backend must read the conductor URL and poll `/next?device=`.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repo root")
        .to_path_buf();
    let clients = [
        ("templates/explorer.dart", "FlutterDrive"),
        ("runners/web/runner.mjs", "WebCdp (web)"),
        ("runners/electron.mjs", "WebCdp (electron)"),
        ("runners/tauri.mjs", "WebCdp (tauri)"),
        (
            "runners/rn/runner.mjs",
            "Appium (react-native/swift-ios/android)",
        ),
        ("runners/macos-ax.swift", "DesktopAx"),
        ("crates/reproit/src/backends/uia.rs", "DesktopUia"),
        ("crates/reproit/src/backends/atspi.rs", "DesktopAtspi"),
        ("runners/reproit_imgui.h", "Instrumented (imgui)"),
        ("runners/reproit_clay.h", "Instrumented (clay)"),
        ("crates/reproit/src/backends/tui.rs", "Tui"),
    ];
    for (rel, backend) in clients {
        let src = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("reading {rel}: {e}"));
        assert!(
            src.contains("REPROIT_SCENARIO_BARRIER"),
            "{backend} runner {rel} lost the conductor URL hookup"
        );
        assert!(
            src.contains("/next?device="),
            "{backend} runner {rel} lost the conductor poll loop"
        );
    }
}

#[test]
fn an_unlabeled_actor_claims_a_role_and_runs_assertions() {
    // No REPROIT_DEVICE: the actor must claim its role from the conductor.
    // The script also exercises the non-key verbs: an assert against the blank
    // `sleep` screen (fails honestly, marker still emitted) and a type.
    let script = vec![
        (0, "key:Down"),
        (0, "assert:text=nonexistent"),
        (0, "type:field=hello"),
    ];
    let (port, observed) = start_conductor(script, 1);
    let out = stdout_of(spawn_actor(port, None));
    assert!(out.contains("JOURNEY claimed role=a"), "{out}");
    assert!(out.contains("FUZZ:ACT a key:Down"), "{out}");
    assert!(
        out.contains("FUZZ:ASSERT fail text=\"nonexistent\" actor=a"),
        "{out}"
    );
    assert!(out.contains("FUZZ:ACT a type:field=hello"), "{out}");
    assert!(out.contains("JOURNEY DONE"), "{out}");
    let obs = observed.lock().unwrap();
    assert_eq!(obs.acked.len(), 3, "every step acked: {:?}", obs.acked);
}
