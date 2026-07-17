use super::*;

fn load_fuzz_json() -> serde_json::Value {
    let Ok(path) = std::env::var("REPROIT_FUZZ_CONFIG") else {
        return serde_json::json!({});
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// The list of per-seed fuzz configs plus whether this is a multi-seed batch.
pub(super) fn load_batch() -> (Vec<serde_json::Value>, bool) {
    let j = load_fuzz_json();
    if let Some(batch) = j.get("batch").and_then(|v| v.as_array()) {
        if !batch.is_empty() {
            return (batch.clone(), true);
        }
    }
    (vec![j], false)
}

struct Rng {
    s: u32,
}
impl Rng {
    fn new(seed: u32) -> Self {
        Rng {
            s: if seed == 0 { 1 } else { seed },
        }
    }
    fn step(&mut self) -> u32 {
        self.s ^= self.s << 13;
        self.s ^= self.s >> 17;
        self.s ^= self.s << 5;
        self.s
    }
    fn unit(&mut self) -> f64 {
        (self.step() & 0x7fff_ffff) as f64 / (0x8000_0000u32 as f64)
    }
}

fn str_array(j: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    j.get(key).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    })
}

fn edge_key(sig: &str, action: &str) -> String {
    format!("{sig}|{action}")
}

fn remember_actions(m: &mut BTreeMap<String, Vec<String>>, sig: &str, actions: Vec<String>) {
    let known = m.entry(sig.to_string()).or_default();
    for a in actions {
        if !known.contains(&a) {
            known.push(a);
        }
    }
}

fn first_untried_action(
    m: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    sig: &str,
) -> Option<String> {
    m.get(sig).and_then(|actions| {
        actions
            .iter()
            .find(|a| !tried.contains(&edge_key(sig, a)))
            .cloned()
    })
}

fn has_frontier(m: &BTreeMap<String, Vec<String>>, tried: &BTreeSet<String>) -> bool {
    m.keys()
        .any(|sig| first_untried_action(m, tried, sig).is_some())
}

fn remember_edge(
    g: &mut BTreeMap<String, Vec<(String, String)>>,
    from: &str,
    action: &str,
    to: &str,
) {
    let edges = g.entry(from.to_string()).or_default();
    if !edges.iter().any(|(a, t)| a == action && t == to) {
        edges.push((action.to_string(), to.to_string()));
    }
}

fn path_to_frontier(
    g: &BTreeMap<String, Vec<(String, String)>>,
    m: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    from: &str,
) -> Option<Vec<String>> {
    if first_untried_action(m, tried, from).is_some() {
        return Some(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut q = std::collections::VecDeque::new();
    seen.insert(from.to_string());
    q.push_back((from.to_string(), Vec::<String>::new()));
    while let Some((sig, path)) = q.pop_front() {
        if let Some(edges) = g.get(&sig) {
            for (action, to) in edges {
                if !seen.insert(to.clone()) {
                    continue;
                }
                let mut next = path.clone();
                next.push(action.clone());
                if first_untried_action(m, tried, to).is_some() {
                    return Some(next);
                }
                q.push_back((to.clone(), next));
            }
        }
    }
    None
}

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

fn observe_scenario(
    app: &Acc,
    value_selectors: &[String],
    cap: &mut ValueCap,
    seen: &mut BTreeSet<String>,
) -> Snapshot {
    // LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT ported
    // to the Linux AT-SPI backend: a desktop window has no device orientation to
    // rotate, and this backend drives the app by walking the AT-SPI tree and
    // clicking -- it has no app-lifecycle background/foreground hook (minimizing
    // is a window-manager action, not a paused->resumed lifecycle), so the ground
    // truth those oracles need cannot be produced here.
    let snap = snapshot(app, value_selectors, cap);
    let observation_labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
    emit(&format!(
        "FUZZ:OBS {}",
        serde_json::json!({
            "sig": snap.sig,
            "labels": observation_labels,
            "elements": snap.elements
        })
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
    app: &Acc,
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
    let mut current = observe_scenario(app, value_selectors, cap, &mut seen);

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
        emit(&format!("FUZZ:ACT {role} {act}"));
        grab_focus(&app_window(app));

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(app, name);
        } else if let Some(a) = act.strip_prefix("assert:") {
            let fresh = snapshot(app, value_selectors, cap);
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
                "JOURNEY[a] step: auth-restore unsupported on desktop-atspi runner; drive the \
                 login UI explicitly for auth:{acct}"
            ));
        } else if act == "back" {
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(b) = act.strip_prefix("type:") {
            let (finder, value) = b.rsplit_once('=').unwrap_or((b, ""));
            match find_typable(app, finder, 0) {
                Some(node) if set_text(&node, value) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(label) = act.strip_prefix("tap:") {
            let fresh = snapshot(app, value_selectors, cap);
            match fresh.nodes.get(label) {
                Some(node) if do_press(node) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(700));
        } else {
            emit(&format!("FUZZ:MISS {role} {act}"));
        }

        // Crash oracle: the app process gone from the bus cannot continue.
        // Deliberately no /done ack, so the conductor names this actor+action.
        if target_lost(acc_pid(app)) {
            crash(
                "target lost",
                &format!("the AT-SPI target vanished during {act}"),
            );
            failed = true;
            break;
        }
        let nxt = observe_scenario(app, value_selectors, cap, &mut seen);
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

pub(super) fn find_app_by_name(desktop: &Acc, target: &str) -> Option<Acc> {
    let want = target.to_lowercase();
    acc_children(desktop)
        .into_iter()
        .find(|app| acc_name(app).to_lowercase().contains(&want))
}

pub(super) fn find_app_by_pid(desktop: &Acc, pid: u32) -> Option<Acc> {
    acc_children(desktop)
        .into_iter()
        .find(|app| acc_pid(app) == pid)
}

// One seed's explore/replay walk (single-seed contract, per-seed coverage).
pub(super) fn run_seed(
    app: &Acc,
    value_selectors: &[String],
    cap: &mut ValueCap,
    target_pid: u32,
    fuzz: &serde_json::Value,
    // App-invariant scrape of the launched child's stderr (None when we attached
    // to an already-running app by name, which exposes no stderr to scrape).
    mut inv: Option<&mut InvariantScrape>,
) -> bool {
    let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let mut rng = Rng::new(seed);
    if seed != 0 {
        emit(&format!("JOURNEY[a] step: fuzz seed={seed}"));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut tried: BTreeSet<String> = BTreeSet::new();
    let mut actions_by_state: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut graph: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    let mut observe = |app: &Acc, cap: &mut ValueCap, seen: &mut BTreeSet<String>| -> Snapshot {
        let snap = snapshot(app, value_selectors, cap);
        let observation_labels: Vec<&String> =
            snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
        emit(&format!(
            "FUZZ:OBS {}",
            serde_json::json!({
                "sig": snap.sig,
                "labels": observation_labels,
                "elements": snap.elements
            })
        ));
        if seen.insert(snap.sig.clone()) {
            let labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
            emit(&format!(
                "EXPLORE:STATE {}",
                serde_json::json!({ "sig": snap.sig, "labels": labels, "elements": snap.elements })
            ));
            if !snap.content_bugs.is_empty() {
                let items: Vec<serde_json::Value> = snap
                    .content_bugs
                    .iter()
                    .map(|(k, reason, text)| {
                        serde_json::json!({ "key": k, "reason": reason, "text": text })
                    })
                    .collect();
                emit(&format!(
                    "EXPLORE:CONTENTBUG {}",
                    serde_json::json!({ "sig": snap.sig, "items": items })
                ));
            }
            // BROKEN-ASSET (tofu) for this newly-seen state, keyed by the SAME
            // sig. Only emitted when a U+FFFD replacement character actually
            // rendered, so a clean state stays silent (no marker, no finding).
            if !snap.broken_assets.is_empty() {
                let items: Vec<serde_json::Value> = snap
                    .broken_assets
                    .iter()
                    .map(|(k, detail)| {
                        serde_json::json!({ "key": k, "reason": "tofu", "detail": detail })
                    })
                    .collect();
                emit(&format!(
                    "EXPLORE:BROKENASSET {}",
                    serde_json::json!({ "sig": snap.sig, "items": items })
                ));
            }
        }
        // APP-INVARIANT (EXPLORE:INVARIANT): re-emit any violation the app's SDK
        // reported for this state (scraped from the child's stderr). Runs every
        // settle, not just new states, so a violation that appears on a revisit
        // is still caught; the scrape de-dups per sig so it is reported once.
        if let Some(iv) = inv.as_deref_mut() {
            iv.flush_for(&snap.sig);
        }
        snap
    };

    let mut current = observe(app, cap, &mut seen);
    let launch_sig = current.sig.clone();
    let mut stuck = 0u32;
    let mut crashed = false;

    let prefix = str_array(fuzz, "prefix");
    let replay = str_array(fuzz, "replay");
    let prefix_len = prefix.as_ref().map(|p| p.len()).unwrap_or(0);
    let map_mode = replay.is_none() && prefix.is_none() && seed == 0;
    let configured = std::env::var("REPROIT_FUZZ_CONFIG").is_ok();
    let budget: usize = if let Some(r) = &replay {
        r.len()
    } else if map_mode && !configured {
        usize::MAX
    } else {
        fuzz.get("budget")
            .and_then(|v| v.as_u64())
            .unwrap_or(ACTION_BUDGET as u64) as usize
            + prefix_len
    };
    let edge_weights: BTreeMap<String, BTreeMap<String, u64>> = fuzz
        .get("edgeWeights")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(sig, m)| {
                    m.as_object().map(|mm| {
                        (
                            sig.clone(),
                            mm.iter()
                                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                                .collect(),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let is_soak = replay.is_some();
    let soak_start = Instant::now();
    if is_soak {
        sample_rss(target_pid, 0);
    }

    let mut i = 0usize;
    while i < budget && stuck < 3 {
        if is_soak && i > 0 {
            sample_rss(target_pid, soak_start.elapsed().as_millis() as u64);
        }
        let act: Option<String> = if let Some(r) = &replay {
            r.get(i).cloned()
        } else if prefix.as_ref().map(|p| i < p.len()).unwrap_or(false) {
            prefix.as_ref().and_then(|p| p.get(i).cloned())
        } else if seed != 0 {
            let mut taps: Vec<String> = current.tappables.clone();
            taps.sort();
            let ew = edge_weights.get(&current.sig);
            let mut options: Vec<String> = taps.iter().map(|l| format!("tap:{l}")).collect();
            options.push("back".to_string());
            let weights: Vec<f64> = options
                .iter()
                .map(|o| 1.0 / (1.0 + ew.and_then(|m| m.get(o)).copied().unwrap_or(0) as f64))
                .collect();
            let total: f64 = weights.iter().sum();
            let mut r = rng.unit() * total;
            let mut chosen = options.last().cloned();
            for (k, w) in weights.iter().enumerate() {
                r -= w;
                if r <= 0.0 {
                    chosen = Some(options[k].clone());
                    break;
                }
            }
            chosen
        } else {
            let mut taps: Vec<String> = current.tappables.clone();
            taps.sort();
            let mut options: Vec<String> = taps.iter().map(|l| format!("tap:{l}")).collect();
            options.push("back".to_string());
            remember_actions(&mut actions_by_state, &current.sig, options);
            let mut a = first_untried_action(&actions_by_state, &tried, &current.sig);
            if a.is_none() {
                a = path_to_frontier(&graph, &actions_by_state, &tried, &current.sig)
                    .and_then(|p| p.first().cloned());
            }
            if a.is_none() && has_frontier(&actions_by_state, &tried) && current.sig != launch_sig {
                break;
            }
            a
        };

        let Some(act) = act else { break };
        emit(&format!("FUZZ:ACT {act}"));

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(app, name);
            i += 1;
            continue;
        }
        if act == "back" {
            let from_sig = current.sig.clone();
            tried.insert(edge_key(&from_sig, "back"));
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
            // Crash oracle: if the action killed the target, stop before recording
            // the now-empty tree as a state/edge (mirrors run_scenario_actor).
            if target_lost(acc_pid(app)) {
                crash(
                    "target lost",
                    &format!("the AT-SPI target vanished during {act}"),
                );
                crashed = true;
                break;
            }
            let observe_start = Instant::now();
            let nxt = observe(app, cap, &mut seen);
            maybe_emit_hang(
                &from_sig,
                "back",
                observe_start.elapsed().as_millis() as u64,
            );
            if nxt.sig != current.sig {
                emit(&format!(
                    "EXPLORE:EDGE {}",
                    serde_json::json!({ "from": current.sig, "action": "back", "to": nxt.sig })
                ));
                remember_edge(&mut graph, &current.sig, "back", &nxt.sig);
            }
            if nxt.sig != current.sig || nxt.content != current.content {
                stuck = 0;
            } else {
                stuck += 1;
            }
            current = nxt;
            i += 1;
            continue;
        }
        let label = act.strip_prefix("tap:").unwrap_or(&act).to_string();
        let from_sig = current.sig.clone();
        tried.insert(edge_key(&current.sig, &act));
        let press_start = Instant::now();
        let pressed = current.nodes.get(&label).map(do_press).unwrap_or(false);
        if !pressed {
            emit(&format!("FUZZ:MISS {act}"));
            stuck += 1;
            i += 1;
            continue;
        }
        std::thread::sleep(Duration::from_millis(700));
        // Crash oracle: a tap that killed the target ends the walk before the
        // empty tree is recorded as a state/edge (mirrors run_scenario_actor).
        if target_lost(acc_pid(app)) {
            crash(
                "target lost",
                &format!("the AT-SPI target vanished during {act}"),
            );
            crashed = true;
            break;
        }
        let nxt = observe(app, cap, &mut seen);
        let elapsed = press_start.elapsed().as_millis() as u64;
        maybe_emit_hang(
            &from_sig,
            &format!("tap:{label}"),
            elapsed.saturating_sub(700),
        );
        if nxt.sig != current.sig {
            emit(&format!(
                "EXPLORE:EDGE {}",
                serde_json::json!({
                    "from": current.sig,
                    "action": format!("tap:{label}"),
                    "to": nxt.sig
                })
            ));
            remember_edge(&mut graph, &current.sig, &format!("tap:{label}"), &nxt.sig);
        }
        if nxt.sig != current.sig || nxt.content != current.content {
            stuck = 0;
        }
        current = nxt;
        i += 1;
    }

    emit(&format!("JOURNEY[a] step: explored {} states", seen.len()));
    crashed
}
