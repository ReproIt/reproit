# Repro It: Reproducible AI QA

## One-liner

AI QA that turns natural-language user flows into reproducible integration tests, runs them deterministically with full evidence capture (video, action log, execution trace), detects functional, visual, accessibility, and longevity regressions, and opens source-code PRs for fixable issues.

## Core thesis

Most AI QA tools are flaky because the agent improvises every run. Most E2E suites are expensive because engineers write and maintain every test. Repro It uses AI to discover and author tests, but deterministic runners to execute them.

```
Natural language
  -> structured test plan
  -> generated test file (real code, in the customer's repo)
  -> deterministic CI run
  -> evidence bundle (video + action log + execution trace, synchronized)
  -> optional source-code fix PR
```

The AI explores once. The AI writes a durable test. The runner reruns it deterministically. The AI repairs selectors or source code when needed. The AI is never in the execution loop.

## Origin

Repro It is a generalization of a hand-built test harness for a single Flutter app. That harness already implements, for one app: two-simulator concurrent journeys with real cross-device synchronization, per-device video recording with ffmpeg side-by-side compositing, determinism pinning (9:41 status bar, pinned GPS, pre-granted permissions), full state reset (Redis flush + dev endpoints), marker-driven OS-level screenshots, tolerance-based visual diffing with baselines and advisory screens, and scroll stitching. Repro It extracts that machinery into a standalone product and adds the layers the harness lacks: AI authoring, white-box instrumentation, fix PRs, fuzzing, and soak testing.

| Source harness artifact | Repro It component |
|---|---|
| `scripts/walkthrough.sh` | Multi-actor orchestrator + evidence collector |
| `scripts/lib.sh` (`sim_pin_determinism`, `flush_all_redis`) | Determinism pinning + state-reset steps |
| `/dev/*` backend endpoints (seed, clear, claim-role, reset-pair) | The state-reset contract |
| `scripts/screenshots.sh` (`SHOOT:` markers) | Marker-driven screenshot capture |
| `scripts/diff_screenshots.py` | Visual differ (tolerance, baselines, advisory) |
| `integration_test/journey_*.dart` | Target format for generated tests |
| `test_driver/screenshot_driver.dart` | Plain drive driver |

## Platform strategy

**Flutter mobile-first.** Dogfood target: the user's own Flutter apps. Web (Playwright) is a later mode; the architecture is platform-agnostic except the runner layer.

Flutter gives several core primitives natively:

- Settlement: `pumpAndSettle()` (wrapped with a bound; it hangs on infinite animations).
- Visual baselines: golden tests, with perceptual tolerance instead of exact pixels (Impeller rendering and text anti-aliasing vary).
- Leak detection: `leak_tracker` (per-scenario not-disposed / not-GCed detection).
- A11y audits: `meetsGuideline` (`textContrastGuideline`, `androidTapTargetGuideline`, `labeledTapTargetGuideline`).
- Clock travel: the `clock` package with injection; `FakeAsync` in widget tests.
- Runtime introspection: the Dart VM service (coverage, exceptions, heap, timeline) in debug/profile builds.

The strategic constraint: Flutter draws on a canvas, so automation sees only what the app exposes through the **Semantics layer**. An app with sparse `Semantics` is invisible to black-box drivers. Therefore the first fix-PR class is "add missing Semantics labels": it bootstraps explorability itself.

Web equivalences (for the later web mode): Playwright contexts for multi-actor, `getByRole`/`getByLabel` selectors, `page.clock` for time travel, HAR record/replay for network determinism, CDP for heap/coverage, axe-core for a11y.

## The trust ladder

Each rung earns adoption for the next. The runner must work standalone with hand-written tests; the authoring agent is a client of the runner, never fused into it.

1. **Evidence runner** (no AI trust required): run your existing integration tests under Repro It; get video, synchronized action log, execution trace, fault localization.
2. **AI-authored tests**: natural language in, journey-style test file out, stability-gated.
3. **Fix PRs**: constrained, reviewable source edits for known issue classes.
4. **Fuzz mode**: autonomous coverage-guided exploration against invariants.
5. **Soak mode**: years of usage compressed; leaks and accumulation bugs.

## Architecture

Two layers:

- **Host orchestrator** (Rust CLI, this repo): boots and pins simulators, drives state reset, launches journeys on N devices concurrently, synchronizes multi-actor steps, records and composites video, captures marker screenshots, collects logs into an evidence bundle, runs the visual differ, and (later) runs the authoring agent, fuzzer, and soak loops.

The LLM is behind a **provider-agnostic llm layer** (`crates/llm`): one trait at the task level (prompt in, final text out), hot-swappable via `llm.provider` in reproit.yaml. The abstraction deliberately sits above the wire APIs, not across them; unifying provider wire formats is the lowest-common-denominator trap. Providers: `codex-cli` (`codex exec`, OpenAI via ChatGPT subscription, the default during development), `claude-cli` (`claude -p`, headless Claude Code via Claude subscription), `claude-api` (raw Messages API via the `claude` module in `crates/llm`, per-token, the prod/CI path; typed client with retries, streaming, tool loop, and verbatim passthrough of unknown content blocks). `openai-api` (Chat Completions over raw HTTP, per-token, explicit `model` required). The deterministic runner never touches the llm.
- **In-app layer** (Dart, in the customer's repo): integration_test/Patrol journey files plus a small helper library (`settle`, `waitFor`, `tapIf`, `claimRole`, resource probes). Generated tests are plain Dart files the team owns and can run without Repro It.

Components:

```
App model (memory layer)
  states, transitions, guards, invariants, signatures, selector history, prior failures

Scenario planner
  natural language -> structured steps against the app model

Test generator
  emits journey-style Dart with stable semantics-based finders and assertions

Orchestrator / runner
  N simulators, concurrent flutter drive, role claiming, marker protocol

Evidence collector
  video, screenshots, action log, device logs, VM-service trace, run manifest

Failure analyzer
  classifies the bug, fault-localizes via coverage spectra, drafts the report

Fix generator
  constrained source edits, opens PRs

Visual differ
  tolerance-based pixel diff, baselines, advisory screens, transition signatures
```

## The app model

**Map-first principle: the app map is the canonical artifact; every feature is a view of it.** Authoring is pathfinding (the LLM interprets the NL flow and selects a path; the test code is compiled mechanically from the path's edges, so assertions can only reference what exploration actually observed: hallucinated widget assumptions become structurally impossible). Fuzzing is seeded walks over the edges. Selector repair is a graph diff between app versions. Coverage is visited states/edges. The LLM's role narrows to exploration-time labeling: naming states, proposing invariants, guards, and reversibility, choosing sensible form inputs; the crawl is mostly mechanical (enumerate tappables from the semantics tree) and the runner verifies signatures empirically.

The single schema shared by all modes. Built during exploration (`reproit map`), verified and refined empirically by the runner.

Two representation rules that keep the graph tractable:

- **States are screen templates with parameters**, not concrete screens (profile-of-Alice and profile-of-Bob are one node parameterized by user). Signatures hash semantics-tree structure, not content; content binds to parameters. Otherwise the graph explodes.
- **Interrupts are not states.** One-off events (permission dialogs, push notifications, toasts, rating prompts) live in a separate interrupt set layered over the base graph: each has a recognizer signature and a policy (`dismiss`, `accept`, or `promote` to a real node when flow-relevant, e.g. a match dialog). On every settle the runner checks for known overlays first, applies the policy, logs it, and continues. The fuzzer may also INJECT interrupts deliberately (notification mid-composer, background/foreground mid-upload) as an action type: a rich bug class no scripted suite covers.

- **State**: a named, recognizable configuration of the UI. Signature = settled screenshot (perceptual hash) + semantics-tree fingerprint + route/page identity.
- **Transition**: an action (tap/type/scroll/system event) from state to state. Carries: finder, preconditions (guards), expected transition signature (diff bounding box, settle bound), observed timing stats.
- **Guard**: condition required to take a transition (logged in, item exists, role).
- **Invariant**: a property that must hold. Global (no uncaught exceptions, no console errors, semantics tree non-degenerate, a11y guidelines pass, no infinite spinner, back never dead-ends) or app-specific, LLM-proposed and human-confirmed ("once onboarding completes, the wizard is unreachable"; "cart badge equals cart line count").
- **Reversibility**: an edge is reversible iff some path returns to a state whose signature matches the pre-edge state. The LLM proposes the label ("looks like a payment, probably irreversible"); the fuzzer verifies it empirically in a sandbox. Mislabels self-correct. Irreversible edges require a state checkpoint to cross.

## Build-fidelity ladder

Which build does reproit test? Both, deliberately, at different rungs:

- **Dev build (debug, JIT), dev login, dev endpoints**: the default for the inner loop and most journeys. Skipping real auth via dev login is a feature, not a cheat: third-party IdPs are the classic E2E flake source, and most journeys test the app's flows, not the auth provider.
- **Profile build (`reproit run --profile`, AOT)**: required for representative performance evidence. Debug-mode frame timings are JIT-skewed and overstate jank; the frames feature's numbers should only gate anything when collected under --profile. VM-service introspection still works here (release strips it).
- **Prod-flavor app against staging backend**: the release-candidate rung. At least one journey must cover the REAL onboarding and login path that production users actually hit (the dev-menu quick-login never ships, so a suite that only uses it leaves first-run auth untested). The reset contract still applies: staging keeps dev endpoints; the app build just stops showing dev affordances.

Cadence: dev build per-change, profile nightly/per-PR for perf, prod-flavor with the real-onboarding journey before release. Warm reuse never crosses build modes.

## Determinism doctrine

- **Pinning**: fixed status bar time (9:41), battery, GPS; permissions pre-granted (regrant loop spans app reinstalls); keyboard intros disabled; seeded data only.
- **Settlement**: never assert on a frame, assert after settlement (no scheduled frames/animations, bounded). Visual transition checks are numeric: diff confined to expected bounding box, settles within bound, settled frame matches baseline within tolerance.
- **Two replay modes, clearly labeled**:
  - *Recorded backend*: network record/replay (proxy or app-level interceptor), virtual clock, seeded randomness. Fully deterministic frontend. Bit-stable replay.
  - *Live backend*: statistical replay (N repeats). "Fails 10/10 recorded, 3/10 live" is itself the diagnosis: frontend logic bug vs. backend race.
- **Stability gate**: a generated test must pass 5/5 consecutive runs against a fresh-reset environment before it is accepted or PR'd. This is the product's credibility in one number, and the 5 passing runs double as the passing-coverage corpus for fault localization.

## Evidence bundle

Three synchronized timelines per run:

1. **Video**: per-device recordings, composited side-by-side for multi-actor runs.
2. **Action log**: every step (tap, type, assert, marker) timestamped, parsed from the journey's structured log lines into `actions.jsonl`.
3. **Execution trace** (white-box, debug/profile builds): per-run line coverage and uncaught exceptions with stacks via the Dart VM service; frame timeline.

Plus: marker screenshots (`SHOOT:<name>`, scroll series stitched), device logs, run manifest (`manifest.json`: config snapshot, device UDIDs, timings, outcomes, artifact paths).

**Fault localization**: spectrum-based (Ochiai). Compare line-coverage spectra of failing runs against the stability gate's passing corpus; rank lines by "executed when failing, rarely when passing." The bug report reads: "at 0:14 in the video, the tap on Send fired, these lines in `chat_repository.dart` executed, this exception was swallowed at line 142." Ranked suspect lines are the fix generator's primary input.

## Modes

### 1. Flows (test what users told you about)

Natural-language flow in, journey Dart file out. The agent explores via semantics-tree dump + screenshot pairs, plans structured steps, emits code using the helper library, and must pass the stability gate. Multi-actor flows (Alice/Bob) are first-class: role claiming via a dev endpoint, each side waits on the other's observable effects (real cross-device propagation).

### 2. Fuzz (test what nobody thought of)

Model-based + stateful property-based testing, with the LLM doing what made MBT impractical (building the model, proposing invariants) and a deterministic machine doing everything else:

- **Seeded random walks** over the transition graph. Seed + model version fully determine the sequence: every fuzz case is replayable by construction. Zero LLM calls during execution.
- **Coverage-guided**: bias walks toward least-visited transitions and state-pairs (back-and-forth bugs live in rare pairs).
- **Invariant checks** are compiled code. The LLM is called only to explain a failure after the fact.
- **Shrinking**: on failure, deterministically delete actions and replay until minimal. Output: "open settings, go back, open settings again: the modal loses focus."
- **Replay-to-confirm**: rerun the shrunk sequence N times before reporting. Consistent failure: bug. Intermittent: reported explicitly as nondeterministic ("fails 3/10: race condition"), which for realtime apps is itself a finding.
- **Promotion**: a confirmed shrunk fuzz case becomes a permanent journey test, opened as a PR. The fuzzer is a test-discovery engine feeding the same pipeline.
- **Safety**: fuzzing only ever runs against staging/preview. Destructive/irreversible edges are gated behind state checkpoints, never improvised.

### 3. Soak (test what only shows up after years)

"10 years of usage in minutes" decomposes into three axes:

- **Action count**: thousands of actions fast (animations zeroed, timers fast-forwarded). Catches accumulation bugs: storage that appends and never evicts, listener leaks.
- **Clock time**: jump the app's injected clock years ahead mid-session. Token expiry, "posted 2 years ago" rendering, TTLs, subscription states, 2038. Backend time travel needs customer cooperation; aged fixtures substitute.
- **Data volume**: don't replay history, synthesize aged state. Seed a tenant with 100k records, then run flows and fuzz on top. Catches "works at n=20, dies at n=20,000."

The core invariant, free from the fuzzer's graph: **a reversible cycle, after forced GC, must be resource-neutral.** Run the cycle 200 times, snapshot JS, Dart heap, storage, listener counts at intervals, fit the slope. Growth that plateaus is caching; growth that doesn't is a leak. `leak_tracker` provides retainer analysis. Shrinking for accumulation bugs finds the minimal *loop*, not the minimal sequence: "open and close the emoji picker 50 times: heap grows 2.1MB, here is the retainer path."

Trend oracles, per interval: heap after forced GC, not-disposed objects, storage estimate, listener counts, and the latency of a fixed probe action (flagged on superlinear growth).

## Generated test format

Real files in the customer's repo, runnable without Repro It. Journey style with a small helper library:

```dart
testWidgets('alice sends bob a message', (tester) async {
  final role = await claimRole();        // multi-actor: backend alternates a/b
  await pumpApp(tester);
  await login(tester, role);
  if (isA(role)) {
    await send(tester, 'hello');
    await expectEventually(tester, find.text('hi back'));
  } else {
    await expectEventually(tester, find.text('hello'));
    await send(tester, 'hi back');
  }
});
```

Helper library contract (Dart, vendored into the customer repo): `settle(bounded)`, `waitFor`, `tapIf`, `expectEventually`, `claimRole`, `shoot(name)` (screenshot marker), structured action logging (`JOURNEY[role] step: ...` lines the orchestrator parses), resource probes (soak).

## Fix PRs: v1 issue classes

Constrained and reviewable only. Flutter-flavored:

1. Missing `Semantics` labels / accessible names (first, because it unlocks explorability).
2. Unlabeled form fields; missing `semanticLabel` on images/icons.
3. Tap targets below guideline size.
4. Contrast token fixes (obvious cases).
5. Focus not restored after dialog close; focus traps.
6. Missing error announcement (a11y live regions).
7. Flaky finders replaced with stable keys/semantics identifiers.
8. Missing `dispose()` for controllers/listeners identified by leak_tracker with an unambiguous retainer path.

Every fix PR carries before/after evidence (screenshot or trace) and the test that now passes.

## State-reset contract

The capability everything depends on (stability gate, fuzzing across irreversible edges, soak baselines). Implemented by the customer as test-only dev endpoints plus optional infra steps; Repro It orchestrates them. The contract, generalized from the source harness's `/dev/*` endpoints:

- `reset` steps: ordered list of HTTP calls and shell commands (e.g. Redis flush, `POST /dev/clear-presence`, `POST /dev/reset-pair`).
- `seed` steps: deterministic fixtures (optionally aged for soak).
- `claim-role`: alternating role assignment so one build serves N concurrent devices.
- Checkpoint/restore (later): snapshot before irreversible edges, restore after.

Designed as if a stranger had to adopt it: this contract is the product's customer-facing integration surface.

## Non-goals (v1)

- Native iOS/Android (non-Flutter), web (later mode), desktop.
- Full WCAG legal certification.
- Autonomous testing against production.
- Automatic merge to main; replacing manual QA; complex business-logic fixes.
- Dashboard, auth, billing: CLI only until generation quality is proven.

## Positioning

- Main: **Reproducible AI QA for modern apps.**
- Developer: *Describe user flows in English. Get real integration tests, deterministic runs, evidence bundles, and fix PRs.*
- vs. black-box AI agents: *AI exploration, deterministic execution. Tests your team owns.*
- vs. hand-written E2E: *Same reliable runner, far less authoring and maintenance.*
- vs. a11y scanners: *We test real workflows and fix source code, not just list violations.*
- The frame: Antithesis-style autonomous property testing, for app UIs, at SaaS prices, with the properties written for you. (Their pillar we substitute, full-system determinism, becomes: recorded-backend replay + statistical live replay, clearly labeled.)

Pricing sketch (later; unchanged from the original spec): Starter $99/mo, Team $299/mo, Pro $799+/mo (fix PRs, multi-actor orchestration, compliance evidence).

## Roadmap

1. **Extract** (this repo, now): generalize the source harness into the `reproit` CLI. Config-driven orchestrator, determinism pinning, reset steps, multi-device drive, video + compositing, marker screenshots, visual differ, evidence manifest. The first app runs as customer zero via `reproit.yaml`.
2. **Instrument**: VM-service layer: per-run coverage, exception capture, action-log timestamps synced to video. Fault localization against the stability-gate corpus.
2b. **Map** (`reproit map`): the explorer that builds the app graph: mechanical crawl via the driver (semantics tree + settled screenshot per state, enumerate tappables), LLM labeling pass (state names, invariants, reversibility proposals, interrupt policies). Author v2 (pathfinding + mechanical compilation) and fuzz mode both consume it.
3. **Author**: the LLM agent (explore via semantics + screenshots, emit journey Dart, stability gate 5/5). App-model schema populated as a side effect.
4. **Fix**: missing-Semantics detector and patcher; first PR against the dogfood app.
5. **Fuzz**: seeded walks, invariants, shrinking, promotion.
6. **Soak**: cycle leak detection, trend oracles, aged fixtures, clock travel.
7. **Web mode** (shipped v0): Playwright runner adapter on the same core. `app.platform: web-playwright`; a Node runner (web-runner/runner.mjs) drives Chromium and emits the identical marker protocol, so map/graph/fuzz/soak/a11y/evidence all work unchanged. Validated on a web bug-zoo: mapped 4 states, a11y detection caught an unlabeled control, and the fuzzer found a planted stateful exception with a JS stack trace and shrunk repro. The KEY architectural result: the marker protocol is the framework-agnostic contract; ~80% of reproit is platform-independent, and a new platform is a new runner that speaks markers, not a fork. Remaining web polish: page.clock determinism, HAR network replay, axe-core for richer a11y, CDP for memory/coverage probes.
8. **React Native mode** (shipped v0, structural): rn-appium runner (rn-runner/runner.mjs: Node + Appium/webdriverio, accessibility-tree explorer) emitting the same marker protocol. `app.platform: rn-appium`. Structurally complete and sharing the contract web + Flutter validated; needs an Appium-server + device end-to-end pass to certify. Demonstrates the thesis: a third framework was a runner, not a fork.
9b. **Cloud** (design: CLOUD.md): the platform the CLI earns. Workers run the same reproit binary against leased devices; the fleet sells massive parallel fuzz/soak (1000 devices at once), the platform sells history/graphs/evidence, self-hosted runners answer the enterprise-security question. Moat = data + fleet + platform, not engine secrecy.

9. **MCP server** (shipped v0: `reproit mcp`): reproit as the acceptance oracle inside coding-agent loops (edit, gate, analyze, fix, repeat). Strategically this inverts the coding-agent threat: agents that write UI code blind become consumers of the deterministic runner, regardless of which tool authored the test.
10. **Bug-zoo demo app**: a small Flutter app with one planted bug per capability (missing Semantics label, focus not restored, leaked controller, chat race, layout break). Doubles as the public demo and as the internal eval corpus for authoring success rate and fix-PR correctness.
11. **PR review mode** ("UI correctness review"): GitHub App that runs affected journeys + visual diff + a11y on each PR and comments with evidence inline. Analog: CodeRabbit reviews the diff for code correctness; reproit reviews the running app for UI correctness. PR-time gets the fast lane (visual + smoke journeys); the full gate runs nightly (Mac-minute economics).

12. **Graph rendering** (shipped v0: `reproit graph`): the app map as a human artifact: Mermaid (FigJam native import, GitHub markdown), DOT, and a self-contained interactive HTML viewer. Cross-team value: a living user-flow diagram for design/PM, generated rather than hand-drawn; later overlays: per-state coverage, a11y status, screenshots as node thumbnails.

Status: 1 shipped; 2b shipped as v0 (`reproit map`: semantics-tree explorer in templates/explorer.dart + host assembly + optional LLM state labeling; the first real crawl mapped 7 states / 7 transitions and incidentally surfaced a real RenderFlex overflow bug with file:line via the exception pipeline, validating the fuzz thesis); 3 and the analyzer half shipped as v0 (`reproit author`, `reproit analyze`); 9 and 12 shipped as v0 (`reproit mcp`, `reproit graph`); instrument v0 shipped (structured exceptions in `exceptions.jsonl`, fed to `analyze`). Next: fix PRs (4) grounded in map + exceptions data, VM-service coverage (instrument v1), explorer hardening (interrupt handling, deeper crawls, scroll actions). A product landing page lives in `site/index.html`.

## Repo layout

```
reproit/
  SPEC.md                    this document
  reproit.example.yaml        config schema by example
  Cargo.toml                 cargo workspace
  crates/
    reproit/                  the CLI
      src/
        main.rs              entrypoint: doctor, run, gate, visual, devices
        config.rs            config schema (serde) + loader, ${ENV} interpolation
        simctl.rs            simulator control: ensure, boot, pin, grant, record, composite
        orchestrator.rs      multi-device journey runs, marker protocol, evidence, manifest
        reset.rs             state-reset steps (http + command)
        drive.rs             flutter drive wrapper, log line parsing, actions.jsonl
        visual.rs            baseline pixel diff (tolerance, advisory, diff images)
        appmap.rs            app model schema (states, transitions, invariants)
        exec.rs              process helpers
    llm/                   provider-agnostic LLM seam (Provider trait + providers)
      src/
        lib.rs               Task, Provider trait, Spec, provider factory
        providers.rs         codex-cli, claude-cli (subscription) and claude-api (per-token)
    claude/                  thin internal Claude API client (raw HTTP)
      src/
        client.rs            auth, headers, retries; the only wire-protocol code
        types.rs             Messages API types; unknown blocks pass through verbatim
        stream.rs            SSE accumulator (long outputs must stream)
        runner.rs            agentic tool loop
        error.rs             typed errors incl. refusal stop reason
  templates/
    journey_helpers.dart     helper library vendored into customer repos
```
