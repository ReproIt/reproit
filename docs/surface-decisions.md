# Surface decisions: the costs, so the call is made on numbers

`plan-structural-reduction.md` step D. Restructuring redistributes complexity;
only dropping surface removes it. This file makes each surface's ongoing cost
concrete so the founder can decide from real figures rather than feel.

Nothing here is retired unilaterally. Recommendations are marked, the decision
is not mine.

## How the costs were measured

- lines: source only, excluding `node_modules`, `target`, `.venv`, `dist`
- CI: the per job duration from run 30635617132, a normal push
- defect history: commits touching that directory, all time, as a proxy for how
  much attention the surface has needed
- release gate: whether `validation/release/package-platform-sdks.sh` ships it

## SDKs

The `tier` column this table used to carry is gone, because the tiers are gone:
every SDK is fully supported and every one blocks a release. The `CI job` column
now records where each is gated (`sdk/INVENTORY.json` is the enforced version of
it). The recommendations below are unchanged and still the founder's to make;
carrying a surface and gating it are separate decisions, and gating it is what
tells you honestly what carrying it costs.

| SDK | lines | commits | CI job | released | recommendation |
| --- | ---: | ---: | --- | --- | --- |
| backend-node | 2,703 | 6 | sdk-backend-reference | no | keep |
| backend-rs | 3,575 | 14 | sdk-backend-reference | no | keep |
| backend-py | 3,124 | 7 | sdk-backend-reference | no | keep |
| backend-go | 4,405 | 6 | sdk-backend-reference | no | keep |
| backend-java | 3,952 | 7 | sdk-backend-ports (new) | no | keep |
| backend-dotnet | 4,221 | 8 | sdk-backend-ports (new) | no | keep |
| backend-rb | 2,605 | 7 | sdk-backend-ports (new) | no | keep |
| backend-php | 3,938 | 7 | sdk-backend-ports (new) | no | keep |
| android | 6,085 | 15 | android-sdk, 76s | yes | keep |
| ios | 6,890 | 16 | apple-sdk, 374s | yes | keep |
| react-native | 5,936 | 17 | react-native-sdk, 52s | yes | keep |
| linux | 2,435 | 9 | inside signature-parity | yes | **decide** |
| windows | 4,600 | 9 | windows-sdk, 22s | yes | **decide** |
| tauri | 419 | 4 | tauri-and-recorder-sdks (new) | yes | keep, trivial |
| tui-go | 1,785 | 4 | tui-go-sdk, 26s | yes | **candidate to retire** |
| tui-py | 1,680 | 7 | tui-py-sdk, 5s | yes | **candidate to retire** |
| tui-rs | 945 | 5 | rust (named explicitly) | yes | **candidate to retire** |
| tui-ts | 1,667 | 5 | tui-ts-sdk, 6s | yes | **candidate to retire** |
| recorder-node | 755 | 1 | tauri-and-recorder-sdks (new) | no | keep, it is a dependency |
| flutter | 6,657 | 16 | flutter-sdk, 242s | yes | **decide** |

The four TUI SDKs are 6,077 lines and four separate ports of one small idea, for
a target (Terminal UI) that is already Stable and served by the TUI runner. They
are the clearest retirement candidate on this table: retiring three of the four
and keeping one reference port would remove roughly 4,300 lines, two CI jobs,
and three release artifacts.

## UI targets

Eleven of twenty one targets are Preview, each with two or three named
blockers, all frozen behind the backend IndependentQualified qualification per
`docs/compatibility.md`.

| target | maturity | blockers | recommendation |
| --- | --- | ---: | --- |
| Backend contracts, Terminal UI, Web Chromium, Web Firefox, Web WebKit | Stable | 0 | keep, these are the product |
| Electron Linux, Windows WPF, Windows Avalonia, Jetpack Compose Android, Flutter iOS | Stable | 0 | keep |
| Linux GTK, Linux Qt Quick/QML, Linux Qt Widgets, Linux wxWidgets | Preview | 2 to 3 | **four Linux desktop toolkits is the widest Preview bet; recommend keeping one and retiring three** |
| React Native Android, React Native iOS, SwiftUI iOS, Flutter Android | Preview | 2 to 3 | keep, they pair with released mobile SDKs |
| macOS Accessibility, Tauri Linux, Windows WinUI 3 | Preview | 2 to 3 | **decide** |

The freeze already stops these from consuming promotion effort. The open
question is whether a frozen Preview target should still cost a CI job and a
compatibility row, or be retired outright.

## Runners

The four `operability-golden-*` jobs (appkit, wpf, qt, gtk) total 124 seconds
and exist to pin per toolkit golden output. They are cheap and they are the
evidence behind the Preview rows above; retiring a target should retire its
golden job at the same time, or the evidence outlives the claim.

The three Appium jobs (ios smoke 610s, swiftui smoke 530s, android smoke 144s)
are 1,284 seconds, comfortably the largest recurring CI cost after
windows-build at 738s. They gate the mobile targets that pair with released
SDKs, so the cost is defensible, but it is the place to look first if CI wall
clock becomes a problem.

## What a decision would actually save

Retiring the three surplus TUI SDKs and three of the four Linux desktop
toolkits, the two most defensible candidates on this page, would remove roughly
4,300 lines of SDK, two CI jobs, three release artifacts, three compatibility
rows and their golden jobs, and every future defect in them.

That is a real reduction and it is small compared to what the plans call the
honest ceiling, which is the point `plan-structural-reduction.md` makes: the
system is large because it is broad, and only narrowing it makes it smaller.
