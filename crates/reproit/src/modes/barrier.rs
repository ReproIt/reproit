//! The multi-actor conductor: a tiny host-local HTTP barrier that hands each
//! device runner its next action only when it is that device's turn in the
//! global, strictly-ordered scenario script.
//!
//! This is the framework-agnostic core of multi-actor: every backend (web node,
//! flutter dart on a sim, android, desktop) drives ONE actor and speaks the
//! same verbs to this barrier over localhost (which iOS sims and host processes
//! alike can reach):
//!
//!   GET  /claim                  -> body `<letter>` (your role: `a`, `b`, ...)
//!                                        | `ERR full` (all roles taken)
//!   GET  /next?device=<a|b|...>  -> body `WAIT` (not your turn, or not all
//!                                        actors have joined yet)
//!                                        | `ACT\t<action>` (do this now)
//!                                        | `DONE` (scenario finished)
//!   POST /done?device=<a|b|...>  -> body `OK`  (that action is complete; the
//!                                        conductor advances to the next step)
//!
//! Identity (`/claim`) and ordering (`/next`+`/done`) both live here, in the
//! orchestrator, so the only per-backend code is "execute one action" + "speak
//! these verbs". A device label `a`/`b`/... maps to actor index 0/1/... .
//!
//! Two safety properties this enforces, both framework-neutral:
//!   * Distinct roles. `/claim` hands out `a`, then `b`, ... from an atomic
//!     counter and refuses an over-subscribed claimant, so two devices can
//!     never drive the same actor (the failure that silently dropped an actor
//!     when a shared-build runner defaulted every device to role `a`).
//!   * Join-barrier. No action is served until every actor has shown up (via
//!     `/claim` or its first `/next`). If one never joins, the run stalls and
//!     `diagnose()` names who is missing rather than failing anonymously.

use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

struct State {
    /// Number of distinct actors/devices this scenario expects.
    n: usize,
    /// Owning device index (0=`a`, 1=`b`, ...) of each step, in global order.
    owners: Vec<usize>,
    /// The action string of each step.
    actions: Vec<String>,
    /// The step currently being served (or about to be).
    cursor: usize,
    /// Whether `cursor`'s action has been handed out and we're awaiting
    /// `/done`.
    served: bool,
    /// When `cursor`'s action was first handed out (for stall attribution).
    served_at: Option<Instant>,
    /// Which actor indices have shown up (claimed or polled at least once).
    joined: Vec<bool>,
}

impl State {
    fn all_joined(&self) -> bool {
        self.joined.iter().all(|&j| j)
    }
}

/// Where a stalled (or finished) scenario got stuck, for human-readable
/// attribution when a run does not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Every step completed.
    Done,
    /// Not all actors joined; these device indices never showed up.
    AwaitingJoin(Vec<usize>),
    /// An action was handed out but never reported done by its owner.
    Stalled {
        dev: usize,
        action: String,
        secs: u64,
    },
    /// In progress at this step index (no stall observed).
    Running(usize),
}

/// A running conductor. Drop it (or call `stop`) to shut the server down.
pub struct Conductor {
    handle: JoinHandle<()>,
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Conductor {
    /// Start serving `script` (an ordered list of `(device_index, action)`) for
    /// `n` actors on an ephemeral localhost port. Returns once the listener is
    /// bound.
    pub async fn start(script: Vec<(usize, String)>, n: usize) -> Result<Conductor> {
        let state = Arc::new(Mutex::new(State {
            n,
            owners: script.iter().map(|(d, _)| *d).collect(),
            actions: script.iter().map(|(_, a)| a.clone()).collect(),
            cursor: 0,
            served: false,
            served_at: None,
            joined: vec![false; n],
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let st = state.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(&mut sock, &st).await;
                });
            }
        });
        Ok(Conductor {
            handle,
            port,
            state,
        })
    }

    /// The base URL runners poll, reachable from host processes and iOS sims.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Whether every step has been completed.
    pub fn is_done(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.cursor >= s.actions.len()
    }

    /// Where the scenario stands: completed, awaiting a no-show actor, stalled
    /// on an un-acked action, or simply in progress. Used to turn an
    /// anonymous timeout into a named diagnosis.
    pub fn diagnose(&self) -> Stage {
        let s = self.state.lock().unwrap();
        if s.cursor >= s.actions.len() {
            return Stage::Done;
        }
        let missing: Vec<usize> = s
            .joined
            .iter()
            .enumerate()
            .filter(|(_, j)| !**j)
            .map(|(i, _)| i)
            .collect();
        if !missing.is_empty() {
            return Stage::AwaitingJoin(missing);
        }
        if s.served {
            let secs = s.served_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            return Stage::Stalled {
                dev: s.owners[s.cursor],
                action: s.actions[s.cursor].clone(),
                secs,
            };
        }
        Stage::Running(s.cursor)
    }
}

impl Drop for Conductor {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Map an actor index to its device letter (`0`->`a`, `1`->`b`, ...).
pub fn letter(i: usize) -> char {
    (b'a' + i as u8) as char
}

async fn serve_conn(sock: &mut TcpStream, state: &Arc<Mutex<State>>) -> Result<()> {
    // We only need the request line ("<METHOD> <path?query> HTTP/1.1"); a single
    // read covers it for these tiny requests.
    let mut buf = [0u8; 1024];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let device = path
        .split("device=")
        .nth(1)
        .and_then(|s| s.split(['&', ' ']).next())
        .and_then(|s| s.chars().next())
        .map(|c| (c as u8).wrapping_sub(b'a') as usize);

    let body = route(method, path, device, state);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.flush().await?;
    Ok(())
}

fn route(method: &str, path: &str, device: Option<usize>, state: &Arc<Mutex<State>>) -> String {
    let mut s = state.lock().unwrap();
    // Identity negotiation: a device that does not know its role (e.g. a
    // shared-build runner where the label can't be baked in) asks for one.
    if path.starts_with("/claim") {
        return claim(&mut s);
    }
    let Some(dev) = device else {
        return "ERR no-device".into();
    };
    if path.starts_with("/next") {
        return next_action(&mut s, dev);
    }
    if method == "POST" && path.starts_with("/done") {
        // Advance only if this device owns the served step (idempotent / safe).
        if s.cursor < s.actions.len() && s.owners[s.cursor] == dev && s.served {
            s.cursor += 1;
            s.served = false;
            s.served_at = None;
        }
        return "OK".into();
    }
    "ERR bad-request".into()
}

/// Hand out the lowest role not yet taken (claimed, or already seen via
/// `/next`), atomically. Refuses an over-subscribed claimant so two devices can
/// never share an actor, and stays correct even if some runners are
/// env-labelled (they register via `/next`) while others claim.
fn claim(s: &mut State) -> String {
    match s.joined.iter().position(|&j| !j) {
        Some(role) => {
            s.joined[role] = true;
            letter(role).to_string()
        }
        None => "ERR full".into(),
    }
}

fn next_action(s: &mut State, dev: usize) -> String {
    // First contact also counts as joining (covers env-labelled runners that
    // never call /claim).
    if dev < s.n {
        s.joined[dev] = true;
    }
    if s.cursor >= s.actions.len() {
        return "DONE".into();
    }
    // Join-barrier: hold every actor until all have shown up, so actor A's first
    // step can't run before actor B exists to observe its effect.
    if !s.all_joined() {
        return "WAIT".into();
    }
    if s.owners[s.cursor] == dev {
        if !s.served {
            s.served = true;
            s.served_at = Some(Instant::now());
        }
        return format!("ACT\t{}", s.actions[s.cursor]);
    }
    "WAIT".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(script: &[(usize, &str)]) -> Arc<Mutex<State>> {
        let n = script.iter().map(|(d, _)| *d + 1).max().unwrap_or(0);
        Arc::new(Mutex::new(State {
            n,
            owners: script.iter().map(|(d, _)| *d).collect(),
            actions: script.iter().map(|(_, a)| a.to_string()).collect(),
            cursor: 0,
            served: false,
            served_at: None,
            joined: vec![false; n],
        }))
    }

    #[test]
    fn strict_order_one_actor_at_a_time() {
        // alice, alice, bob, alice
        let s = st(&[(0, "tap:a1"), (0, "tap:a2"), (1, "tap:b1"), (0, "tap:a3")]);
        // bob asks first: marks bob joined, but alice hasn't joined -> WAIT.
        assert_eq!(route("GET", "/next?device=b", Some(1), &s), "WAIT");
        // alice gets step 0 (now both joined), completes it.
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "ACT\ttap:a1");
        assert_eq!(route("POST", "/done?device=a", Some(0), &s), "OK");
        // still alice's turn (step 1); bob still waits.
        assert_eq!(route("GET", "/next?device=b", Some(1), &s), "WAIT");
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "ACT\ttap:a2");
        assert_eq!(route("POST", "/done?device=a", Some(0), &s), "OK");
        // now bob's step.
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "WAIT");
        assert_eq!(route("GET", "/next?device=b", Some(1), &s), "ACT\ttap:b1");
        assert_eq!(route("POST", "/done?device=b", Some(1), &s), "OK");
        // back to alice, last step, then DONE for everyone.
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "ACT\ttap:a3");
        assert_eq!(route("POST", "/done?device=a", Some(0), &s), "OK");
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "DONE");
        assert_eq!(route("GET", "/next?device=b", Some(1), &s), "DONE");
    }

    #[test]
    fn done_without_serving_does_not_advance() {
        let s = st(&[(0, "tap:a1")]);
        // A spurious /done before /next must not skip the step.
        assert_eq!(route("POST", "/done?device=a", Some(0), &s), "OK");
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "ACT\ttap:a1");
    }

    #[test]
    fn claim_hands_out_distinct_roles_then_refuses() {
        let s = st(&[(0, "tap:a1"), (1, "tap:b1")]);
        assert_eq!(route("GET", "/claim", None, &s), "a");
        assert_eq!(route("GET", "/claim", None, &s), "b");
        // Over-subscribed: no third role exists.
        assert_eq!(route("GET", "/claim", None, &s), "ERR full");
    }

    #[test]
    fn join_barrier_holds_until_all_present() {
        let s = st(&[(0, "tap:a1"), (1, "tap:b1")]);
        // Alice is ready but bob hasn't shown: alice must WAIT despite owning step 0.
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "WAIT");
        // Bob claims -> now both joined, alice proceeds.
        assert_eq!(route("GET", "/claim", None, &s), "b");
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "ACT\ttap:a1");
    }

    #[test]
    fn diagnose_reports_a_missing_actor() {
        let s = st(&[(0, "tap:a1"), (1, "tap:b1")]);
        // Only alice shows up.
        assert_eq!(route("GET", "/next?device=a", Some(0), &s), "WAIT");
        let stage = {
            let mut g = s.lock().unwrap();
            // mimic Conductor::diagnose against the shared State
            if g.cursor >= g.actions.len() {
                Stage::Done
            } else {
                let missing: Vec<usize> = g
                    .joined
                    .iter()
                    .enumerate()
                    .filter(|(_, j)| !**j)
                    .map(|(i, _)| i)
                    .collect();
                if !missing.is_empty() {
                    Stage::AwaitingJoin(missing)
                } else if g.served {
                    Stage::Stalled {
                        dev: g.owners[g.cursor],
                        action: g.actions[g.cursor].clone(),
                        secs: 0,
                    }
                } else {
                    let _ = &mut g;
                    Stage::Running(0)
                }
            }
        };
        assert_eq!(stage, Stage::AwaitingJoin(vec![1]));
    }
}
