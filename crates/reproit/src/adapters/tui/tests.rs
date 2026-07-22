use super::*;

#[test]
fn volatile_error_screens_do_not_count_as_effective_coverage() {
    assert!(coverage_is_incomplete(false, 36, 0, 36));
    assert!(!coverage_is_incomplete(false, 36, 1, 35));
    assert!(!coverage_is_incomplete(false, 36, 0, 0));
    assert!(!coverage_is_incomplete(true, 36, 0, 36));
}

#[test]
fn tui_auth_registry_is_structural_and_locale_independent() {
    let path = input_file_path();
    std::fs::write(
        path,
        "{\"sel\":\"key:telefono\",\"inputPurpose\":\"tel\"}\n{\"sel\":\"key:codigo\",\"\
             inputPurpose\":\"one-time-code\"}\n",
    )
    .unwrap();
    let elements = structural_input_elements();
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0]["inputPurpose"], "otp");
    assert_eq!(elements[1]["inputPurpose"], "phone");
    assert!(elements.iter().all(|e| e["label"] == ""));
}

// Property tests (Hegel): hold the determinism invariants for ANY input.

#[hegel::test]
fn rng_is_reproducible_for_any_seed(tc: hegel::TestCase) {
    let seed: u32 = tc.draw(hegel::generators::integers::<u32>());
    let (mut a, mut b) = (Rng::new(seed), Rng::new(seed));
    for _ in 0..64 {
        assert_eq!(a.step(), b.step(), "same seed must yield the same stream");
    }
}

#[hegel::test]
fn signature_is_a_pure_function_of_the_skeleton(tc: hegel::TestCase) {
    // The state signature must be a deterministic function of the screen's
    // structural skeleton: same skeleton + cursor -> same sig, every time.
    let contents: String = tc.draw(hegel::generators::text());
    let cur: (u16, u16) = (
        tc.draw(hegel::generators::integers::<u16>()),
        tc.draw(hegel::generators::integers::<u16>()),
    );
    assert_eq!(
        structural_sig(&contents, cur),
        structural_sig(&contents, cur),
        "structural sig must be deterministic"
    );
}

#[hegel::test]
fn words_do_not_change_the_signature(tc: hegel::TestCase) {
    // Swapping ASCII letters (a stand-in for translating the UI) must not move
    // the signature: the localized identity of words is excluded by construction.
    let base: String = tc.draw(hegel::generators::text());
    let translated: String = base
        .chars()
        .map(|c| if c.is_ascii_alphabetic() { 'Z' } else { c })
        .collect();
    assert_eq!(
        structural_sig(&base, (0, 0)),
        structural_sig(&translated, (0, 0)),
        "swapping letters (translation) must not change the structural sig"
    );
}

// The runner primitives that make "author once, reproduce forever" true: a
// seeded RNG and deterministic action selection. (The signature primitives
// are pinned in the reproit-tui-sig crate the runner and SDKs share.)

#[test]
fn rng_is_reproducible_and_seed_sensitive() {
    let (mut a, mut b) = (Rng::new(42), Rng::new(42));
    for _ in 0..256 {
        assert_eq!(a.step(), b.step(), "same seed must yield the same stream");
    }
    assert_ne!(Rng::new(42).step(), Rng::new(43).step());
}

#[test]
fn ucb_pick_is_deterministic() {
    let actions: Vec<String> = ["key:Down", "key:Up", "key:Enter"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let bound: BTreeSet<String> = actions.iter().cloned().collect();
    let lv = BTreeMap::new();
    let ar = BTreeMap::new();
    let sp = BTreeMap::new();
    let pick = |seed| {
        let mut rng = Rng::new(seed);
        ucb_pick(&actions, &bound, "sig0", &lv, &ar, &sp, None, 0.5, &mut rng)
    };
    assert_eq!(pick(9), pick(9), "same seed + same state -> same action");
}

#[test]
fn path_to_frontier_crosses_cycles_to_untried_state() {
    let mut actions_by_state = BTreeMap::new();
    actions_by_state.insert("home".into(), vec!["key:Down".into(), "key:Enter".into()]);
    actions_by_state.insert(
        "settings".into(),
        vec!["key:Esc".into(), "key:Enter".into()],
    );
    actions_by_state.insert("help".into(), vec!["key:Esc".into()]);

    let tried = BTreeSet::from([
        edge_key("home", "key:Down"),
        edge_key("home", "key:Enter"),
        edge_key("settings", "key:Esc"),
    ]);
    let mut graph = BTreeMap::new();
    remember_edge(&mut graph, "home", "key:Down", "settings");
    remember_edge(&mut graph, "settings", "key:Esc", "home");
    remember_edge(&mut graph, "settings", "key:Enter", "help");

    assert_eq!(
        path_to_frontier(&graph, &actions_by_state, &tried, "home"),
        Some(vec!["key:Down".into()]),
        "home is exhausted, so walk the known cycle to settings"
    );
    assert_eq!(
        first_untried_action(&actions_by_state, &tried, "settings"),
        Some("key:Enter".into())
    );
}

#[test]
fn action_space_is_full_alphabet_with_known_keymap_bound() {
    let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
    let (all, bound) = action_space("jless data.json", &parser);
    assert_eq!(all.len(), KEYS.len(), "full alphabet always reachable");
    assert!(bound.contains("key:j") && bound.contains("key:dollar"));
    assert!(
        bound.contains("key:CtrlC"),
        "universal crash key always bound"
    );
    let (all2, bound2) = action_space("totally-unknown-app", &parser);
    assert_eq!(all2.len(), KEYS.len());
    assert!(bound2.contains("key:Down") && bound2.contains("key:Esc"));
    assert!(
        !bound2.contains("key:j"),
        "no keymap, blank screen -> j not bound"
    );
}

// Operability signals (EXPLORE:GROUNDTRUTH).

fn grid(rows: &[&str]) -> Vec<Vec<char>> {
    rows.iter().map(|r| r.chars().collect()).collect()
}

#[test]
fn count_full_erases_counts_only_full_screen_clears() {
    // CSI 2 J (erase all) and CSI 3 J (erase all + scrollback) are full
    // repaints; partial erases (0J/1J) and a bare J are not.
    assert_eq!(count_full_erases(b"\x1b[2J"), 1);
    assert_eq!(count_full_erases(b"\x1b[3J"), 1);
    assert_eq!(count_full_erases(b"\x1b[2J\x1b[H\x1b[2J"), 2, "two clears");
    assert_eq!(count_full_erases(b"\x1b[0J"), 0, "erase-to-end is partial");
    assert_eq!(count_full_erases(b"\x1b[J"), 0, "bare J is not 2J/3J");
    assert_eq!(count_full_erases(b"hello world"), 0, "no escape");
}

#[test]
fn mouse_click_uses_the_protocol_requested_by_the_app() {
    let protocol = AtomicU8::new(0);
    observe_mouse_protocol(b"\x1b[?1000h", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::X10);
    assert_eq!(
        mouse_click_bytes(MouseProtocol::X10, 12, 100),
        vec![0x1b, b'[', b'M', 32, 133, 45, 0x1b, b'[', b'M', 35, 133, 45]
    );

    observe_mouse_protocol(b"\x1b[?1006h", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::Sgr);
    assert_eq!(
        mouse_click_bytes(MouseProtocol::Sgr, 12, 100),
        b"\x1b[<0;101;13M\x1b[<0;101;13m"
    );

    observe_mouse_protocol(b"\x1b[?1006l", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::X10);
    observe_mouse_protocol(b"\x1b[?1000l", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::None);
}

#[test]
fn mouse_click_is_silent_until_the_app_requests_reporting() {
    assert!(mouse_click_bytes(MouseProtocol::None, 12, 100).is_empty());
}

#[test]
fn mouse_protocol_request_can_cross_pty_read_boundaries() {
    let protocol = AtomicU8::new(0);
    let mut tail = Vec::new();
    observe_mouse_protocol_stream(b"\x1b[?1000h", &mut tail, &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::X10);
    observe_mouse_protocol_stream(b"paint\x1b[?10", &mut tail, &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::X10);
    observe_mouse_protocol_stream(b"06hmore", &mut tail, &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::Sgr);
    observe_mouse_protocol_stream(b"\x1b[?10", &mut tail, &protocol);
    observe_mouse_protocol_stream(b"00l", &mut tail, &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::None);
    assert!(mouse_click_bytes(mouse_protocol(&protocol), 12, 100).is_empty());
}

#[test]
fn mouse_protocol_changes_follow_stream_order() {
    let protocol = AtomicU8::new(0);
    observe_mouse_protocol(b"\x1b[?1000h\x1b[?1006h\x1b[?1006l", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::X10);

    observe_mouse_protocol(b"\x1b[?1006l\x1b[?1006h", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::Sgr);

    observe_mouse_protocol(b"\x1b[?1006h\x1b[?1000l", &protocol);
    assert_eq!(mouse_protocol(&protocol), MouseProtocol::None);
}

#[test]
fn churned_chrome_rows_flags_unchanged_box_rows_only() {
    // A box-drawing border row that is byte-identical across the transition
    // is churned chrome (rebuilt unchanged after a full erase); a plain text
    // row is not chrome, and a row that actually changed is not churn.
    let pre = grid(&["\u{2500}\u{2500}\u{2500}", "abc", "\u{2502} x \u{2502}"]);
    let mut post = pre.clone();
    post[1] = "abd".chars().collect(); // text row changed -> not chrome anyway
    let churned = churned_chrome_rows(&pre, &post, 16);
    assert_eq!(
        churned,
        vec!["row:0".to_string(), "row:2".to_string()],
        "the two unchanged box rows are churn; the text row never is"
    );
    // A chrome row that genuinely changed is NOT churn (real update).
    let mut post2 = pre.clone();
    post2[0] = "\u{250c}\u{2500}\u{2510}".chars().collect();
    assert_eq!(
        churned_chrome_rows(&pre, &post2, 16),
        vec!["row:2".to_string()],
        "a changed chrome row is a real update, not churn"
    );
    // Cap bounds the output.
    let wide = grid(&["\u{2500}", "\u{2500}", "\u{2500}"]);
    assert_eq!(churned_chrome_rows(&wide, &wide, 2).len(), 2, "capped");
}

#[test]
fn content_bugs_catch_the_web_artifact_classes_with_stable_positions() {
    // The same broken-content classes the web classifier catches, scanned off
    // the settled cell grid and keyed by `pos:R,C`. First-match-wins per the
    // shared precedence; the output is sorted by (key, reason).
    let g = grid(&[
        "Name: [object Object]",
        "Hi {{ user.name }} welcome",
        "path is ${HOME}/x",
    ]);
    let bugs = detect_content_bugs(&g);
    let got: Vec<(&str, &str)> = bugs.iter().map(|b| (b.key.as_str(), b.reason)).collect();
    assert!(got.contains(&("pos:0,6", "object-object")));
    assert!(got.contains(&("pos:1,3", "unrendered-template")));
    assert!(got.contains(&("pos:2,8", "unrendered-template")));
    // Deterministic: same grid -> identical findings (run-to-run / replay).
    let again = detect_content_bugs(&g);
    let keys = |v: &[ContentBug]| -> Vec<String> {
        v.iter()
            .map(|b| format!("{}|{}", b.key, b.reason))
            .collect()
    };
    assert_eq!(keys(&bugs), keys(&again));
}

#[test]
fn content_bugs_do_not_flag_ordinary_prose_or_clean_screens() {
    // The bare-value classes require WHOLE-WORD boundaries, so a word that
    // merely CONTAINS the token ("Cancellation" ~ null, "Null Island" as a
    // proper noun is flagged only when standalone) is left alone. A clean
    // screen yields nothing (the control stays silent -> no marker).
    let prose = grid(&[
        "Cancellation policy applies",
        "undefinedValue is a name",
        "the NaNobot is friendly",
        "Settings  Profile  Logout",
    ]);
    assert!(
        detect_content_bugs(&prose).is_empty(),
        "substrings inside words are not artifacts"
    );
    let data = grid(&[
        r#"{"next": null, "total": NaN}"#,
        "const value = undefined;",
        "status: null",
    ]);
    assert!(
        detect_content_bugs(&data).is_empty(),
        "valid data/code scalars are not artifacts"
    );
}

#[test]
fn content_bugs_do_not_flag_path_embedded_null() {
    // A path segment `null` (git diff headers, file paths) is NOT a content
    // bug: `/` is not a word boundary in the desktop backends' guard, so the
    // token is not standalone. The old "any non-word char is a boundary" rule
    // flagged `--- /dev/null` (measured FP); the aligned rule must not.
    let diff = grid(&[
        "diff --git a/foo.txt b/foo.txt",
        "--- /dev/null",
        "+++ b/foo.txt",
        "content path foo/null/bar here",
    ]);
    assert!(
        detect_content_bugs(&diff).is_empty(),
        "path-embedded null (/dev/null, foo/null/bar) is not a content bug"
    );
    assert!(detect_content_bugs(&grid(&["Price: null", "value (null)", "null"])).is_empty());
}

#[test]
fn tofu_fires_on_a_rendered_replacement_char_and_stays_silent_on_clean() {
    // A cell rendering U+FFFD is broken text encoding: flagged with a
    // stable pos key and a clipped excerpt around the char.
    let g = grid(&["Files", "name: gl\u{FFFD}tch here"]);
    let tofu = detect_tofu(&g);
    assert_eq!(tofu.len(), 1);
    assert_eq!(tofu[0].0, "pos:1,8");
    assert_eq!(tofu[0].1, "name: gl\u{FFFD}tch here");
    // Deterministic: same grid -> identical findings (run-to-run / replay).
    assert_eq!(detect_tofu(&g), tofu);
    // Clean screens (plain, box-drawing, and non-ASCII text) yield nothing:
    // U+FFFD is the only tofu signal, a wide glyph never is.
    let clean = grid(&[
        "\u{250c}\u{2500}\u{2510}",
        "caf\u{e9} \u{4f60}\u{597d}",
        "Save",
    ]);
    assert!(detect_tofu(&clean).is_empty(), "no U+FFFD, no finding");
}

#[test]
fn blank_screen_fires_only_after_content_was_seen() {
    let blank = grid(&["    ", "    ", "    "]);
    let painted = grid(&["    ", " ok ", "    "]);
    // Before the app ever painted content, a blank screen is a slow boot,
    // not the bug: the seen_content guard keeps it silent.
    assert_eq!(blank_screen_item(&blank, false), None);
    // Once content was seen, an all-whitespace screen in a non-zero PTY is
    // the blank-screen bug, carrying the grid size.
    assert_eq!(blank_screen_item(&blank, true), Some((4, 3)));
    // A screen showing anything is never blank, guard or not.
    assert_eq!(blank_screen_item(&painted, true), None);
    // A zero-sized grid has no viewport to be blank in.
    assert_eq!(blank_screen_item(&grid(&[]), true), None);
    // And the guard's content test: ink is any non-whitespace cell.
    assert!(screen_has_ink(&painted));
    assert!(!screen_has_ink(&blank));
}

#[test]
fn blank_screen_requires_persistence_across_a_resample() {
    let blank = grid(&["    ", "    ", "    "]);
    let painted = grid(&["    ", " ok ", "    "]);
    // Both samples blank -> a genuine blank screen, carrying the grid size.
    assert_eq!(
        blank_screen_persisted(&blank, &blank, true),
        Some((4, 3)),
        "persistently blank fires"
    );
    // The first sample caught an all-whitespace transient, but the re-sample
    // has ink: an Ink-style clear+repaint gap, not a blank screen -> silent.
    assert_eq!(
        blank_screen_persisted(&blank, &painted, true),
        None,
        "ink on the re-sample means the first was a transient"
    );
    // A first sample that already has content never reaches the re-sample.
    assert_eq!(blank_screen_persisted(&painted, &blank, true), None);
    // The seen_content guard still applies (a slow boot is never blank).
    assert_eq!(blank_screen_persisted(&blank, &blank, false), None);
}
#[test]
fn groundtruth_emits_only_grounded_keyboard_gaps() {
    let mut gt = Groundtruth::new();
    assert!(gt.record(
        "sig",
        GtElement {
            id: "bracket:2,4".into(),
            gesture_kind: "mouse",
            keyboard_operable: false,
        },
    ));
    assert!(!gt.record(
        "sig",
        GtElement {
            id: "bracket:2,4".into(),
            gesture_kind: "mouse",
            keyboard_operable: false,
        },
    ));
    let element = gt.by_state["sig"].values().next().unwrap();
    assert!(!element.keyboard_operable);
}

#[test]
fn parse_invariant_marker_reads_violations_and_ignores_noise() {
    // A well-formed marker yields the SDK sig + the violated (id, message).
    let (sig, items) = parse_invariant_marker(concat!(
        r#"REPROIT_INVARIANT {"sig":"abc","items":["#,
        r#"{"id":"cart-total","message":"went negative"}]}"#
    ))
    .expect("a marker line parses");
    assert_eq!(sig, "abc");
    assert_eq!(items, vec![("cart-total".into(), "went negative".into())]);
    // message is optional (empty allowed).
    let (_, items) =
        parse_invariant_marker(r#"noise REPROIT_INVARIANT {"sig":"","items":[{"id":"x"}]}"#)
            .unwrap();
    assert_eq!(items, vec![("x".into(), String::new())]);
    // A non-marker line, a malformed body, and an empty item list are all
    // silent (a clean settle emits no marker, so None is the clean direction).
    assert!(parse_invariant_marker("just a rendered frame").is_none());
    assert!(parse_invariant_marker("REPROIT_INVARIANT {not json").is_none());
    assert!(
        parse_invariant_marker(r#"REPROIT_INVARIANT {"sig":"a","items":[]}"#).is_none(),
        "empty items => nothing to report"
    );
}

#[test]
fn invariant_scrape_dedups_per_state_and_matches_sig() {
    let path = std::env::temp_dir().join(format!("reproit-inv-test-{}.ndjson", std::process::id()));
    std::fs::write(
        &path,
        "REPROIT_INVARIANT \
             {\"sig\":\"s1\",\"items\":[{\"id\":\"inv\",\"message\":\"boom\"}]}\n",
    )
    .unwrap();
    let mut scr = InvariantScrape::new(&path.to_string_lossy());
    // Violating state s1 reports once, keyed by the SDK sig; a clean state s2
    // reports nothing; re-visiting s1 is de-duped (no repeat every settle).
    assert_eq!(
        scr.pending_for("s1"),
        Some(vec![("inv".into(), "boom".into())]),
        "violating state fires"
    );
    assert_eq!(scr.pending_for("s2"), None, "clean state is silent");
    assert_eq!(scr.pending_for("s1"), None, "same state does not repeat");
    // An empty-sig marker is attributed to the runner's next observed state.
    std::fs::write(
        &path,
        "REPROIT_INVARIANT {\"sig\":\"\",\"items\":[{\"id\":\"g\",\"message\":\"\"}]}\n",
    )
    .unwrap();
    scr.offset = 0; // re-read the rewritten file from the top
    assert_eq!(
        scr.pending_for("s9"),
        Some(vec![("g".into(), String::new())]),
        "empty-sig marker lands on the current runner sig"
    );
    let _ = std::fs::remove_file(&path);
}
