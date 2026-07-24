use serde_json::Value;
use std::collections::BTreeSet;

/// The oracle categories a finding can belong to (docs/cli.md "Oracles").
/// `--only`/`--no` filter on these. `as_str` is the canonical lowercase tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Oracle {
    /// A finding whose taxonomy is not recognized by this CLI build. Unclassified
    /// findings must never inherit the crash oracle: doing so would turn a
    /// parser/registry drift into a confirmed product bug.
    Unclassified,
    Crash,
    Jank,
    Leak,
    Visual,
    /// Transient render glitch within a single run: a frame that diverges
    /// sharply from both neighbors then resolves (a flash/flicker),
    /// detected frame-to-frame in the repro video. Distinct from `Visual`
    /// (cross-run baseline regression).
    Flicker,
    Divergence,
    /// Broken rendered content: a label showing a stringify/template artifact
    /// ([object Object], a bare undefined/null/NaN, an unrendered {{...}}). A
    /// deterministic DOM/label finding from the web runner, built-in (no custom
    /// invariant needed).
    ContentBug,
    /// App-owned content lies outside an explicitly bounded container in two
    /// settled layout samples. Ambiguous containment or ownership abstains.
    Overflow,
    /// Main-thread freeze / no-progress hang: an action that blocked the main
    /// thread past the hang floor. Deterministic, keyed off the Long Tasks
    /// trace.
    Hang,
    /// A visible control whose hit target is covered by a foreign element.
    Occlusion,
    /// Explicitly declared indicator no longer stays attached to its declared
    /// owner/container. Missing or ambiguous relationships abstain.
    DetachedIndicator,
    /// A native control's authoritative live state contradicts the computed
    /// accessibility state for the exact same DOM node in two settled samples.
    AccessibilityState,
    /// Choice-anomaly: one option of a multi-choice component
    /// (tab/radio/select/ button-cluster) shifts the global layout when its
    /// siblings do not. A differential specialist signal from the web
    /// runner. An outlier is not proof that sibling choices were intended
    /// to have identical effects.
    ChoiceAnomaly,
    /// Broken route: the app links to a URL whose document responds 404/410.
    /// Objective HTTP evidence, but not proof that the response was unintended,
    /// so it remains a specialist signal unless backed by a route contract.
    BrokenRoute,
    /// Security hygiene: a pure DOM/URL predicate for a client-side security
    /// smell -- a cross-origin `target=_blank` link without `rel=noopener`
    /// (reverse tabnabbing), an HTTPS page with an `http:` form action or an
    /// `http:` subresource (mixed content). Deterministic specialist hygiene,
    /// not application-intent proof.
    Security,
    /// Stuck soft keyboard: the on-screen keyboard is visible while no text
    /// input is focused, so it covers content the user never asked it to cover
    /// (navigated away from a field and the IME never dismissed). Ground-truth
    /// state observation. Some apps intentionally retain an IME across focus
    /// handoff, so it remains an environment specialist without an app
    /// contract. Native mobile only
    /// (Flutter and Appium explorers); desktop and web have no soft keyboard.
    StuckKeyboard,
    /// Duplicate submit: a submit-like control that, tapped twice in rapid
    /// succession, fires the SAME first-party non-GET request twice -- the
    /// handler has no double-activation guard, so an impatient double click
    /// places the order/payment/post twice. From the web runner's opt-in
    /// double-dispatch probe (REPROIT_DUPSUBMIT=1: double-firing real submits
    /// changes exploration semantics, so a normal walk never does it).
    DuplicateSubmit,
    /// Focus loss: a tap that does not navigate yet leaves
    /// document.activeElement on <body> while the tapped control still
    /// exists -- the interaction's re-render dropped keyboard focus, so a
    /// keyboard user loses their place. The runner excludes common
    /// intentional cases, but cannot prove the app's desired post-action
    /// focus target. Specialist candidate (web only).
    FocusLoss,
    /// Blank screen: a structurally empty state corroborated by independent
    /// application-failure authority on the same URL. Visual emptiness alone
    /// abstains (web only).
    BlankScreen,
    /// Broken asset: a dead or browser-rejected critical subresource in the
    /// state. Includes visible dead images/tofu and same-origin stylesheet
    /// or application script failures. Deterministic browser/DOM facts (web
    /// only).
    BrokenAsset,
    /// Zoom reflow: a route that breaks at 200% zoom (WCAG 1.4.10 Reflow,
    /// EAA-mandatory). The runner re-renders the route at half the viewport's
    /// CSS size (the reflow-equivalent of 200% zoom) and flags content that
    /// then requires TWO-DIMENSIONAL scrolling (a horizontal scrollbar on a
    /// vertically-scrolling document) or a previously visible tappable whose
    /// hit rect collapses below 1px. Pure layout measurement at a fixed
    /// zoomed viewport, deterministic (web only).
    ZoomReflow,
    /// App-registered invariant: a predicate the app itself declared via the
    /// SDK (`ReproIt.invariant("name", () => bool)`) that must hold in every
    /// visited state. The SDK evaluates its registered predicates on each
    /// state-settle and, only when it detects it is running under the fuzzer,
    /// emits a violation the runner surfaces; the CLI turns it into a finding.
    /// This is the app's own domain rule (a cart total never negative, a
    /// selected tab always highlighted), so it ports to every backend whose SDK
    /// has a state hook. Distinct from the CLI-config `custom` regex rules,
    /// which are declarative predicates written in a reproit config file.
    Invariant,
    /// User-declared structural state or temporal property evaluated over the
    /// normalized cross-platform observation trace.
    Contract,
    /// Rotation-stability: a metamorphic relation across a device-orientation
    /// transform. The explorer rotates the surface (portrait <-> landscape /
    /// split-screen), lets it reflow, then rotates BACK to the original
    /// orientation and re-snapshots. A correct screen reflows but rebuilds the
    /// SAME structure once the original orientation is restored; an app that
    /// mishandles the orientation/resize lifecycle -- dropping content or state
    /// that never comes back -- regresses the structural signature. Round-trip
    /// identity makes this high signal, but an app may intentionally react to
    /// an orientation lifecycle event. It remains an environment candidate
    /// unless the app declares round-trip identity. Value-state is excluded
    /// from the compared signature. Native (Flutter, Appium) + Chromium
    /// (web, electron, tauri) explorers that can drive the transform.
    Rotation,
    /// Background-restore-stability: a metamorphic relation across the app
    /// background -> foreground lifecycle. The explorer sends the app to the
    /// background (paused/hidden) then restores it (resumed/visible) and
    /// re-snapshots. A correct app returns to the SAME screen with its state
    /// intact; one that drops you on a different screen or loses state across
    /// the lifecycle regresses the structural signature. No size change, so a
    /// direct before/after comparison is high signal, but resume may
    /// intentionally lock or redirect. It remains a candidate without an
    /// app contract; value-state is excluded from the compared signature.
    /// Native (Flutter, Appium) + Chromium (web, electron, tauri) explorers
    /// that expose a lifecycle hook.
    BackgroundRestore,
    /// Scroll round-trip: in a scrollable list the content at a pinned offset
    /// is NOT identical after scrolling away and back -- a list-recycling /
    /// virtualization bug rebinds a different row to the same position. A
    /// metamorphic relation across a scroll transform (scroll down then back is
    /// an identity for content at a fixed offset), asserted by the explorers
    /// that can scroll (Flutter, Appium, web). Structural content comparison at
    /// pinned offsets that ignores dynamic value-state. Live/reordered lists
    /// can still change intentionally, so this is an environment candidate.
    ScrollRoundTrip,
    /// Wakelock leak: a wakelock (or a window FLAG_KEEP_SCREEN_ON) acquired on
    /// a screen is STILL held after the user navigates away from that
    /// screen -- a battery drain that keeps the CPU/screen awake off the
    /// video/map/call screen that needed it. Ground truth is `dumpsys
    /// power` (the app-owned held wake locks) plus the focused window's
    /// keep-screen-on flag, sampled before vs after leaving the screen. It
    /// is an environment candidate because an app can intentionally
    /// transfer a lock across screens; a declared ownership contract is
    /// required for confirmation. Android / Appium only: iOS exposes no
    /// public wakelock introspection and web / desktop / TUI have no
    /// wakelock concept.
    WakeLock,
    /// Safe-area collision: an interactive control whose hit rect intersects a
    /// device safe-area inset -- the status bar / notch / Dynamic Island (top),
    /// the home indicator (bottom), or a landscape notch / rounded corner (left
    /// or right) -- so the control is partly obscured or hard to hit. Ground
    /// truth is the platform inset geometry (Flutter MediaQuery.viewPadding /
    /// Appium safe-area insets) versus the control's hit rect. Edge-to-edge
    /// apps can intentionally place controls there, so it remains a
    /// candidate without an authored exclusion/containment contract. Native
    /// mobile only: desktop has no device insets, and the headless web the
    /// runner drives reports every env(safe-area-inset-*) as 0 (no
    /// display-cutout ground truth).
    SafeArea,
    /// Permission dead-end: under a runtime-permission DENIAL sweep, a screen
    /// the app reached after the denial is a genuine graph sink -- a stuck
    /// "please enable X" screen with no working way forward. Uses the
    /// internal sink predicate (the reached screen must itself be trapped)
    /// and attributes the trap to the denied permission, so a team sees the
    /// exact permission that strands the user. Sequence/environment
    /// dependent (it only exists under a denied permission), so it belongs
    /// to the environment sweep, not the single-screen scan crawl. Native
    /// mobile only: Appium denies permissions and Flutter mocks the
    /// permission platform channel; a backend that cannot deny a permission
    /// (web / desktop / TUI) is excluded.
    PermissionWalk,
    /// Zero-contrast invisible content: a run of rendered glyphs whose
    /// resolved foreground color exactly equals its resolved background
    /// color, in a selection/emphasis context (inverse video or an
    /// explicitly styled background), so the content is invisible where
    /// visibility is structurally required. Pure colorimetric equality on
    /// the attributes the app itself emitted; no luminance thresholds and
    /// no aesthetics. Default-background runs are excluded so deliberate
    /// hide-by-matching tricks on the terminal default never fire. TUI
    /// first; web/desktop variants require the same equality bar.
    ZeroContrast,
    /// Dead input: a synthetic input the runner itself injected provably
    /// vanished where an effect is structurally required. Web keystroke
    /// subset: a trusted printable key into a focused, enabled, non-full
    /// editable produces no beforeinput/input event, no value or DOM delta,
    /// no selection move, AND no handler called preventDefault (an
    /// intentional filter/mask/custom editor abstains). Web scroll subset:
    /// a wheel over a scrollable region with room in that direction fires
    /// no scroll event anywhere and moves nothing, with no wheel handler
    /// preventing default. The runner controls the input and observes the
    /// whole event pipeline, so "known input, zero effect, nobody consumed
    /// it" is an equality check, not a judgment.
    DeadInput,
    /// Backend contract family: one category per backend evaluate/ check.
    /// Every one requires a declared or schema-owned contract plus a runtime
    /// witness correlated to the exact operation, and replays exactly (the
    /// backend replay harness re-evaluates the accumulated event sequence
    /// through the same pure check). Findings carry the per-check id below;
    /// legacy artifacts stamped with the umbrella id `backend-contract`
    /// still read back and classify as `Contract`.
    /// Repeatable 5xx for a request satisfying the schema-owned contract.
    BackendServerError,
    /// Successful status outside the declared success statuses.
    BackendResponseStatus,
    /// Operation accepted input outside its declared input domain.
    BackendAcceptedInvalidInput,
    /// Successful output outside the declared output domain.
    BackendResponseShape,
    /// GraphQL selection contradicted by the returned payload.
    BackendResponseSelection,
    /// Read-only operation produced a persistent write or delete effect.
    BackendReadOnlyMutation,
    /// Promised effect missing from a successful, effects-complete call.
    BackendMissingEffect,
    /// More effects than the contract's declared maximum.
    BackendExcessEffect,
    /// Effect crossed the operation's declared tenant boundary.
    BackendTenantIsolation,
    /// Authored application invariant contradicted by a response.
    BackendAuthoredInvariant,
    /// Paginated query violated its authored page semantics.
    BackendQueryPagination,
    /// Concatenated pinned pages differ from the reference operation.
    BackendQueryPaginationReference,
    /// Silent write loss or sibling corruption over the event sequence.
    BackendDataLoss,
    /// Acknowledged create not visible on the declared read operation.
    BackendResourceCreateMissing,
    /// Deleted resource still visible on the declared read operation.
    BackendResourceDeleteVisible,
    /// Resource read returned an entity with the wrong identity.
    BackendResourceIdentity,
    /// Resource read contradicted the declared field state.
    BackendResourceState,
    /// Declared codec projection failed its round trip.
    BackendCodecRoundTrip,
    /// Declared authorization matrix contradicted by a decision.
    BackendAuthorizationMatrix,
    /// Declared transaction committed only part of its effects.
    BackendTransactionAtomicity,
    /// Lost update under declared concurrent access.
    BackendConcurrentUpdate,
    /// Conserved quantity broken under declared concurrent access.
    BackendConcurrentConservation,
    /// Declared resource write/read round trip failed.
    BackendResourceRoundTrip,
    /// Repeating an idempotency key changed the persistent final effect.
    BackendIdempotency,
    /// Fleet invariant broken: mixed builds or config across the fleet.
    BackendFleetConsistency,
}

/// One row of oracle metadata: everything the CLI knows about a category
/// besides its evaluation logic. `as_str`, `parse`, `classify`, and the
/// stable default set all derive from `ORACLES`, and the drift tests pin the
/// table to `oracle-registry.json`, so adding an oracle is one row here plus
/// its registry entry (id, confidence tier, severity class).
pub struct OracleMeta {
    pub oracle: Oracle,
    /// Canonical lowercase tag stamped on findings; the registry id.
    pub id: &'static str,
    /// Extra `--only`/`--no` spellings accepted by `parse`.
    pub aliases: &'static [&'static str],
    /// Finding `invariant` ids that classify into this category.
    pub invariants: &'static [&'static str],
    /// Finding `kind` tokens (uppercase) that classify into this category.
    pub kinds: &'static [&'static str],
    /// Member of the stable default surface (registry `stable_defaults`);
    /// every stable oracle has an authoritative predicate and an exact
    /// replay branch.
    pub stable: bool,
}

/// The single oracle metadata table. Order matches the `Oracle` enum.
pub const ORACLES: &[OracleMeta] = &[
    OracleMeta {
        oracle: Oracle::Unclassified,
        id: "unclassified",
        aliases: &[],
        invariants: &[],
        kinds: &[],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Crash,
        id: "crash",
        aliases: &["exception", "exceptions"],
        invariants: &["no-exception"],
        kinds: &["EXCEPTION", "CRASH", "SIGNAL"],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::Jank,
        id: "jank",
        aliases: &["perf", "performance"],
        invariants: &["no-jank"],
        kinds: &["PERF"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Leak,
        id: "leak",
        aliases: &["memory"],
        // Listener/DOM-node leak across repeated route visits: the same Leak
        // oracle class as the soak/memory signal, just detected structurally.
        invariants: &["no-leak", "no-listener-leak"],
        kinds: &["LEAK", "LISTENERLEAK"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Visual,
        id: "visual",
        aliases: &[],
        invariants: &[],
        kinds: &["VISUAL"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Flicker,
        id: "flicker",
        aliases: &["flash"],
        invariants: &["rerender-flicker", "paint-flicker"],
        kinds: &["FLICKER"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Divergence,
        id: "divergence",
        aliases: &["diverge", "diff"],
        invariants: &[],
        kinds: &["DIVERGENCE"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::ContentBug,
        id: "content-bug",
        aliases: &["content", "contentbug", "broken-render"],
        invariants: &["no-broken-render"],
        kinds: &["CONTENTBUG"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Overflow,
        id: "overflow",
        aliases: &["layout-overflow", "clipping"],
        invariants: &["no-layout-overflow"],
        kinds: &["OVERFLOW"],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::Hang,
        id: "hang",
        aliases: &["freeze", "frozen", "no-progress"],
        invariants: &["no-hang"],
        kinds: &["HANG"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Occlusion,
        id: "occlusion",
        aliases: &["occluded", "blocked-control"],
        invariants: &["no-occluded-control"],
        kinds: &["OCCLUSION"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::DetachedIndicator,
        id: "detached-indicator",
        aliases: &["detachedindicator", "indicator", "badge"],
        invariants: &["no-detached-indicator"],
        kinds: &["DETACHEDINDICATOR"],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::AccessibilityState,
        id: "accessibility-state",
        aliases: &["a11y-state", "semantic-state"],
        invariants: &["no-accessibility-state-mismatch"],
        kinds: &["A11YSTATE"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::ChoiceAnomaly,
        id: "choice-anomaly",
        aliases: &["choice", "choicebug", "anomaly"],
        invariants: &["no-choice-anomaly"],
        kinds: &[],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::BrokenRoute,
        id: "broken-route",
        aliases: &["broken-link", "not-found", "404", "deadlink"],
        invariants: &["no-broken-route"],
        kinds: &[],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Security,
        id: "security",
        aliases: &["sec", "mixed-content", "tabnabbing"],
        invariants: &[],
        kinds: &["SECURITY"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::StuckKeyboard,
        id: "stuck-keyboard",
        aliases: &["keyboard", "ime", "soft-keyboard"],
        invariants: &["no-stuck-keyboard"],
        kinds: &["STUCKKEYBOARD"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::DuplicateSubmit,
        id: "duplicate-submit",
        aliases: &["dupsubmit", "double-submit"],
        invariants: &["no-duplicate-submit"],
        kinds: &["DUPSUBMIT"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::FocusLoss,
        id: "focus-loss",
        aliases: &["focusloss"],
        invariants: &["no-focus-loss"],
        kinds: &["FOCUSLOSS"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::BlankScreen,
        id: "blank-screen",
        aliases: &["blankscreen", "white-screen"],
        invariants: &["no-blank-screen"],
        kinds: &["BLANKSCREEN"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::BrokenAsset,
        id: "broken-asset",
        aliases: &["brokenasset", "dead-asset", "tofu"],
        invariants: &["no-broken-asset"],
        kinds: &["BROKENASSET"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::ZoomReflow,
        id: "zoom-reflow",
        aliases: &["zoomreflow", "reflow", "zoom"],
        invariants: &["no-reflow-break"],
        kinds: &["ZOOMREFLOW"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Invariant,
        id: "invariant",
        aliases: &["assertion", "app-invariant", "custom-invariant"],
        // Both the app-registered invariant path and the CLI-config `custom`
        // regex rules emit kind INVARIANT; bucket both here (previously they
        // fell through to Crash).
        invariants: &["app-invariant"],
        kinds: &["INVARIANT"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::Contract,
        id: "contract",
        aliases: &["temporal-contract", "property"],
        invariants: &[],
        kinds: &["TEMPORAL-CONTRACT"],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::Rotation,
        id: "rotation",
        aliases: &["rotate", "orientation", "split-screen"],
        invariants: &["no-rotation-loss"],
        kinds: &["ROTATION"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::BackgroundRestore,
        id: "background-restore",
        aliases: &["background", "bg-restore", "lifecycle", "backgrounded"],
        invariants: &["no-background-loss"],
        kinds: &["BGRESTORE"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::ScrollRoundTrip,
        id: "scroll-round-trip",
        aliases: &[
            "scrollroundtrip",
            "scroll-recycle",
            "list-recycle",
            "recycle",
        ],
        invariants: &["no-scroll-recycle"],
        kinds: &["SCROLLROUNDTRIP"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::WakeLock,
        id: "wakelock",
        aliases: &["wake-lock", "wakelocks", "keep-screen-on", "battery"],
        invariants: &["no-wakelock-leak"],
        kinds: &["WAKELOCK"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::SafeArea,
        id: "safe-area",
        aliases: &["safearea", "safe-area-inset", "notch"],
        invariants: &["no-safe-area-collision"],
        kinds: &["SAFEAREA"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::PermissionWalk,
        id: "permission-walk",
        aliases: &["permissionwalk", "permission-dead-end", "permission"],
        invariants: &["no-permission-dead-end"],
        kinds: &["PERMISSIONWALK"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::ZeroContrast,
        id: "zero-contrast",
        aliases: &["zerocontrast", "invisible-content", "invisible-text"],
        invariants: &["no-zero-contrast"],
        kinds: &["ZEROCONTRAST"],
        stable: false,
    },
    OracleMeta {
        oracle: Oracle::DeadInput,
        id: "dead-input",
        aliases: &["deadinput", "input-liveness", "swallowed-input"],
        invariants: &["no-dead-input"],
        kinds: &["DEADINPUT"],
        stable: false,
    },
    // Backend contract family. Ids are "backend-" + the per-check oracle
    // string stamped on BackendViolation, and every finding also carries
    // invariant "backend:<check>", which is the classification key here.
    // All are stable: each check is an authoritative contract predicate
    // with an exact replay branch (the backend replay harness).
    OracleMeta {
        oracle: Oracle::BackendServerError,
        id: "backend-server-error",
        aliases: &[],
        invariants: &["backend:server-error"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResponseStatus,
        id: "backend-response-status",
        aliases: &[],
        invariants: &["backend:response-status"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendAcceptedInvalidInput,
        id: "backend-accepted-invalid-input",
        aliases: &[],
        invariants: &["backend:accepted-invalid-input"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResponseShape,
        id: "backend-response-shape",
        aliases: &[],
        invariants: &["backend:response-shape"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResponseSelection,
        id: "backend-response-selection",
        aliases: &[],
        invariants: &["backend:response-selection"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendReadOnlyMutation,
        id: "backend-read-only-mutation",
        aliases: &[],
        invariants: &["backend:read-only-mutation"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendMissingEffect,
        id: "backend-missing-effect",
        aliases: &[],
        invariants: &["backend:missing-effect"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendExcessEffect,
        id: "backend-excess-effect",
        aliases: &[],
        invariants: &["backend:excess-effect"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendTenantIsolation,
        id: "backend-tenant-isolation",
        aliases: &[],
        invariants: &["backend:tenant-isolation"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendAuthoredInvariant,
        id: "backend-authored-invariant",
        aliases: &[],
        invariants: &["backend:authored-invariant"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendQueryPagination,
        id: "backend-query-pagination",
        aliases: &[],
        invariants: &["backend:query-pagination"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendQueryPaginationReference,
        id: "backend-query-pagination-reference",
        aliases: &[],
        invariants: &["backend:query-pagination-reference"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendDataLoss,
        id: "backend-data-loss",
        aliases: &[],
        invariants: &["backend:data-loss"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResourceCreateMissing,
        id: "backend-resource-create-missing",
        aliases: &[],
        invariants: &["backend:resource-create-missing"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResourceDeleteVisible,
        id: "backend-resource-delete-visible",
        aliases: &[],
        invariants: &["backend:resource-delete-visible"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResourceIdentity,
        id: "backend-resource-identity",
        aliases: &[],
        invariants: &["backend:resource-identity"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResourceState,
        id: "backend-resource-state",
        aliases: &[],
        invariants: &["backend:resource-state"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendCodecRoundTrip,
        id: "backend-codec-round-trip",
        aliases: &[],
        invariants: &["backend:codec-round-trip"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendAuthorizationMatrix,
        id: "backend-authorization-matrix",
        aliases: &[],
        invariants: &["backend:authorization-matrix"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendTransactionAtomicity,
        id: "backend-transaction-atomicity",
        aliases: &[],
        invariants: &["backend:transaction-atomicity"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendConcurrentUpdate,
        id: "backend-concurrent-update",
        aliases: &[],
        invariants: &["backend:concurrent-update"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendConcurrentConservation,
        id: "backend-concurrent-conservation",
        aliases: &[],
        invariants: &["backend:concurrent-conservation"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendResourceRoundTrip,
        id: "backend-resource-round-trip",
        aliases: &[],
        invariants: &["backend:resource-round-trip"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendIdempotency,
        id: "backend-idempotency",
        aliases: &[],
        invariants: &["backend:idempotency"],
        kinds: &[],
        stable: true,
    },
    OracleMeta {
        oracle: Oracle::BackendFleetConsistency,
        id: "backend-fleet-consistency",
        aliases: &[],
        invariants: &["backend:fleet-consistency"],
        kinds: &[],
        stable: true,
    },
];

impl Oracle {
    fn meta(self) -> &'static OracleMeta {
        ORACLES
            .iter()
            .find(|m| m.oracle == self)
            .expect("every Oracle variant has an ORACLES row")
    }

    pub fn as_str(self) -> &'static str {
        self.meta().id
    }

    /// Parse a category name (case-insensitive, with a few aliases) into an
    /// `Oracle`. Unrecognized names return None so the caller can warn.
    pub fn parse(name: &str) -> Option<Oracle> {
        let name = name.trim().to_ascii_lowercase();
        ORACLES
            .iter()
            .find(|m| m.id == name || m.aliases.contains(&name.as_str()))
            .map(|m| m.oracle)
    }
}

/// Classify a finding Value into an oracle category, mapping from the finding's
/// `invariant` id (preferred) or its `kind`. The invariant/kind taxonomy
/// already exists (modes/fuzz.rs, model/invariants.rs); this is the single
/// mapping from that taxonomy to the user-facing oracle categories.
pub fn classify(finding: &Value) -> Oracle {
    if matches!(
        finding.get("oracle").and_then(Value::as_str),
        Some("contract" | "backend-contract")
    ) {
        return Oracle::Contract;
    }
    let invariant = finding
        .get("invariant")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !invariant.is_empty() {
        if let Some(m) = ORACLES.iter().find(|m| m.invariants.contains(&invariant)) {
            return m.oracle;
        }
    }
    let kind = finding
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    // Raw framework exception blocks predate the named no-exception invariant
    // (kinds EXCEPTION/CRASH/SIGNAL plus the "EXCEPTION CAUGHT BY ..." prose
    // prefix). They are still objective crashes. Everything else stays ABSTAIN
    // so registry drift cannot masquerade as a confirmed crash.
    if kind.starts_with("EXCEPTION CAUGHT BY") {
        return Oracle::Crash;
    }
    ORACLES
        .iter()
        .find(|m| m.kinds.contains(&kind.as_str()))
        .map(|m| m.oracle)
        .unwrap_or(Oracle::Unclassified)
}

/// An oracle include/exclude filter built from `--only` / `--no`. Default
/// (neither set) is the stable, high-confidence product set. `--only` can opt
/// into any preview/experimental detector; `--no`
/// removes the listed set. When both are given, `--only` is applied first then
/// `--no` subtracts, so `--only crash,jank --no jank` == `crash`.
#[derive(Clone, Debug)]
pub struct OracleFilter {
    only: Option<BTreeSet<&'static str>>,
    no: BTreeSet<&'static str>,
}

impl OracleFilter {
    /// Build from the raw comma-separated `--only`/`--no` strings. Unrecognized
    /// category names are returned (second tuple element) so the caller can
    /// warn without failing the run.
    pub fn build(only: Option<&str>, no: Option<&str>) -> (OracleFilter, Vec<String>) {
        let mut unknown = Vec::new();
        let parse_set = |raw: &str, unknown: &mut Vec<String>| -> BTreeSet<&'static str> {
            let mut set = BTreeSet::new();
            for tok in raw.split(',') {
                let t = tok.trim();
                if t.is_empty() {
                    continue;
                }
                match Oracle::parse(t) {
                    Some(o) => {
                        set.insert(o.as_str());
                    }
                    None => unknown.push(t.to_string()),
                }
            }
            set
        };
        let only_set = match only {
            Some(s) => Some(parse_set(s, &mut unknown)),
            None => Some(Self::stable_set()),
        };
        let no_set = no.map(|s| parse_set(s, &mut unknown)).unwrap_or_default();
        (
            OracleFilter {
                only: only_set,
                no: no_set,
            },
            unknown,
        )
    }

    /// All categories on, nothing filtered (the default when no flags are set).
    pub fn all() -> Self {
        OracleFilter {
            only: None,
            no: BTreeSet::new(),
        }
    }

    /// Default product surface. These detectors describe an objective failure
    /// and have a direct replay predicate. Specialist, timing-sensitive, or
    /// heuristic detectors remain available through `--only` without being
    /// allowed to create noisy default findings.
    pub fn stable() -> Self {
        OracleFilter {
            only: Some(Self::stable_set()),
            no: BTreeSet::new(),
        }
    }

    pub(super) fn stable_set() -> BTreeSet<&'static str> {
        ORACLES.iter().filter(|m| m.stable).map(|m| m.id).collect()
    }

    /// Whether a given oracle category passes the filter.
    pub fn allows(&self, oracle: Oracle) -> bool {
        // Unclassified taxonomy is telemetry about registry drift, never a bug class.
        // It cannot be opted into, including through the internal all() filter.
        if oracle == Oracle::Unclassified {
            return false;
        }
        let tag = oracle.as_str();
        if let Some(only) = &self.only {
            if !only.contains(tag) {
                return false;
            }
        }
        !self.no.contains(tag)
    }

    /// Partition findings into (kept, dropped) by the filter, tagging every
    /// KEPT finding with its `oracle` category (in place). Dropped findings
    /// are returned untagged so the caller can count/report them.
    pub fn apply(&self, findings: Vec<Value>) -> (Vec<Value>, Vec<Value>) {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for mut f in findings {
            let oracle = classify(&f);
            if self.allows(oracle) {
                if let Some(obj) = f.as_object_mut() {
                    obj.insert(
                        "oracle".to_string(),
                        Value::String(oracle.as_str().to_string()),
                    );
                    // Preserve specialist detectors as useful output while
                    // enforcing the confirmation boundary before IDs, shrink,
                    // persistence, or Cloud upload. Repeatability alone cannot
                    // prove application intent.
                    if !Self::stable_set().contains(oracle.as_str()) {
                        obj.insert("advisory".to_string(), Value::Bool(true));
                        obj.insert(
                            "confidence".to_string(),
                            Value::String("candidate".to_string()),
                        );
                    }
                }
                kept.push(f);
            } else {
                dropped.push(f);
            }
        }
        (kept, dropped)
    }
}
