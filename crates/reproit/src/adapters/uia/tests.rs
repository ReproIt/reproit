use super::*;
use std::sync::{Arc, Mutex};

#[test]
fn tofu_detail_flags_a_rendered_replacement_char_with_context() {
    // A rendered U+FFFD is broken text encoding: flagged, with a clipped
    // excerpt around the char as the human detail.
    assert_eq!(
        tofu_detail("glitch \u{FFFD} here").as_deref(),
        Some("glitch \u{FFFD} here")
    );
    // Long text clips to a bounded excerpt that still shows the char.
    let long = format!("{}{}{}", "a".repeat(60), '\u{FFFD}', "b".repeat(60));
    let ex = tofu_detail(&long).expect("long tofu text must flag");
    assert!(ex.chars().count() <= 41 && ex.contains('\u{FFFD}'));
}

#[test]
fn tofu_detail_stays_silent_on_clean_text() {
    // No U+FFFD, no finding: plain, empty, and non-ASCII labels are clean.
    assert_eq!(tofu_detail(""), None);
    assert_eq!(tofu_detail("Save changes"), None);
    assert_eq!(tofu_detail("caf\u{e9} \u{4f60}\u{597d} \u{1f600}"), None);
}

#[test]
fn caption_and_system_buttons_are_never_tapped() {
    // WinUI/UWP shape (Calculator): the caption strip is a WindowControl whose
    // AutomationId is 'TitleBar', holding plain Buttons id='Close'/'Minimize'/
    // 'Maximize'. The WindowControl roots the skip, so its whole subtree (system
    // menu + caption buttons) is dropped before it can be tapped.
    assert!(is_titlebar_root(50032, Some("TitleBar")));
    // Win32 shape: a TitleBarControl (50037) holds the system MenuBar + Close.
    assert!(is_titlebar_root(TITLEBAR_CONTROL_TYPE, None));
    // A caption Button that surfaces outside a recognised title-bar subtree is
    // still excluded by its documented AutomationId, language-independently.
    assert!(is_caption_button(BUTTON_CONTROL_TYPE, Some("Close")));
    assert!(is_caption_button(BUTTON_CONTROL_TYPE, Some("Minimize")));
    assert!(is_caption_button(BUTTON_CONTROL_TYPE, Some("Maximize")));
    assert!(is_caption_button(BUTTON_CONTROL_TYPE, Some("Restore")));
}

#[test]
fn ordinary_controls_and_the_planted_crash_stay_tappable() {
    // Neither guard may swallow an in-app control. The WPF fixture's planted
    // crash button (id='Trigger Bug') and Calculator's own keys must stay
    // reachable so the real crash still fires and coverage is unharmed.
    assert!(!is_titlebar_root(BUTTON_CONTROL_TYPE, Some("Trigger Bug")));
    assert!(!is_caption_button(BUTTON_CONTROL_TYPE, Some("Trigger Bug")));
    assert!(!is_caption_button(BUTTON_CONTROL_TYPE, Some("equalButton")));
    assert!(!is_caption_button(BUTTON_CONTROL_TYPE, Some("num7Button")));
    // A non-button carrying a caption-like id is not a caption button, and a
    // control with no AutomationId (the root window, content panes) is not
    // chrome.
    assert!(!is_caption_button(50032, Some("Close")));
    assert!(!is_titlebar_root(50032, None));
    assert!(!is_caption_button(BUTTON_CONTROL_TYPE, None));
}

#[test]
fn content_bug_flags_leak_artifacts_but_not_prose() {
    // The classic artifacts ARE the label (bare, or a short field prefix): flag.
    assert_eq!(content_bug_reason("null"), Some("null"));
    assert_eq!(content_bug_reason("Price: null"), Some("null"));
    assert_eq!(content_bug_reason("undefined"), Some("undefined"));
    assert_eq!(content_bug_reason("Qty: undefined"), Some("undefined"));
    assert_eq!(content_bug_reason("NaN"), Some("nan"));
    assert_eq!(content_bug_reason("Total: NaN"), Some("nan"));
    // Prose that merely mentions the word inside a sentence is not a leak: the
    // .NET unhandled-exception dialog body raised by the WPF 'Trigger Bug'.
    assert_eq!(
        content_bug_reason("repro demo crash: null inventory record."),
        None
    );
    assert_eq!(
        content_bug_reason("The undefined behavior here is intentional and documented."),
        None
    );
    assert_eq!(
        content_bug_reason("Parsing produced NaN because the field was blank, so we retried."),
        None
    );
    // Templates are always artifacts, guard or not; whole-word only, so a word
    // that merely contains the token ("annulled") is clean.
    assert_eq!(
        content_bug_reason("Hello {{name}}"),
        Some("unrendered-template")
    );
    assert_eq!(content_bug_reason("annulled"), None);
}

#[test]
fn parse_invariant_marker_reads_violations_and_ignores_noise() {
    let (sig, items) = parse_invariant_marker(
        r#"REPROIT_INVARIANT {"sig":"s1","items":[{"id":"total","message":"NaN"}]}"#,
    )
    .expect("a marker parses");
    assert_eq!(sig, "s1");
    assert_eq!(items, vec![("total".into(), "NaN".into())]);
    assert!(parse_invariant_marker("ordinary stderr line").is_none());
    assert!(parse_invariant_marker("REPROIT_INVARIANT {oops").is_none());
    assert!(parse_invariant_marker(r#"REPROIT_INVARIANT {"items":[]}"#).is_none());
}

#[test]
fn invariant_scrape_dedups_per_state_and_matches_sig() {
    let mut state = InvariantState::default();
    state
        .by_sig
        .insert("s1".into(), vec![("inv".into(), "boom".into())]);
    state.fallback = Some(vec![("g".into(), String::new())]);
    let mut scr = InvariantScrape {
        state: Arc::new(Mutex::new(state)),
        emitted: BTreeSet::new(),
    };
    assert_eq!(
        scr.pending_for("s1"),
        Some(vec![("inv".into(), "boom".into())])
    );
    assert_eq!(scr.pending_for("s1"), None, "no repeat on revisit");
    assert_eq!(
        scr.pending_for("s2"),
        Some(vec![("g".into(), String::new())]),
        "empty-sig fallback lands on the current runner sig"
    );
    assert_eq!(scr.pending_for("s3"), None, "fallback is consumed once");
}
