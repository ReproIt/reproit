# windows-winui field campaign: unigetui-mainview-keyup-3298

The selected UniGetUI defect reproduced on native x86_64 Windows and the fixed
revision removed it.

- Affected revision: `fecaf3d46cfdb32ed396a2e903a29d6dec40c2b5`
- Fixed revision: `6bd24003bb3292d5f23760a3f36d652881dcb30c`
- Automation: Windows UI Automation in logged-on session 1
- Application model: unpackaged WinUI 3, `WindowsPackageType None` with a
  self-contained Windows App SDK, so no Windows App Runtime is installed
- Profile: fresh `%LOCALAPPDATA%\UniGetUI` flag directory for every run
- Trigger: focus the search box, hold F1, observe, release F1, observe
- Observable: which page the WinUI navigation is showing

Pull request 3354 moves the MainView shortcut handler from `KeyUp` to
`KeyDown`. F1 is the only shortcut in that handler that takes no modifier and
navigates, so holding it without releasing separates the two edges. The hold
never leaves the target's own automation tree, which is what the candidate
record demanded instead of switching focus to a foreign window.

The Discover page is identified by its `MainTitle` text block. The Help page is
identified positively rather than by absence: it is a `WebView` pane with
`BackButton`, `HomeButton`, `ReloadButton` and `BrowserButton`.

| Role | Run | While F1 held | After F1 released | Result |
|---|---:|---|---|---|
| affected | 1 | Discover Packages | help-browser | reproduced |
| affected | 2 | Discover Packages | help-browser | reproduced |
| affected | 3 | Discover Packages | help-browser | reproduced |
| fixed | 1 | help-browser | help-browser | not reproduced |
| fixed | 2 | help-browser | help-browser | not reproduced |
| fixed | 3 | help-browser | help-browser | not reproduced |
| affected control | 1 | Discover Packages | Discover Packages | legal |
| fixed control | 1 | Discover Packages | Discover Packages | legal |

Every affected run landed on the single identity
`navigation-shortcut-fires-on-key-release-not-key-press`. Every fixed run
reached the same observation point, with the navigation already complete while
the key was still down, and reported no identity.

The neighbouring legal control holds and releases F7, which the same MainView
handler does not bind. On both exact revisions the page stays on Discover
Packages through both key edges, so the observation is not simply reporting
every key event.

Containment was measured rather than assumed. UniGetUI keeps each setting as a
flag file, so 22 `Disable<Manager>` and notice flags are written into a fresh
settings directory before every launch. An uncontained launch spawns
`winget.exe`; no campaign run spawned any of winget, scoop, choco, npm, cargo
or pip. Program-scoped inbound and outbound Windows Firewall rules block the
published executable, which stops the self-update download, and both rules are
removed in the cleanup block.

One setup step is recorded rather than hidden: the administrator notice is a
modal content dialog that stands between launch and the page under test, and
it is acknowledged through its `PrimaryButton`. The Cargo dependency dialog
that appeared during exploration did not appear in any campaign run, because
the manager flags suppress it; every record carries
`dependencyDialogDismissed` false.

Every record verified the exact `UniGetUI.dll` hash, the absence of any
pre-existing owned process, interactive session ownership, that keyboard focus
landed inside MainView, and that the run started on the Discover page. Every
cleanup reported zero remaining owned processes, a removed settings directory
and removed firewall rules.

Raw records remain on the VM and each run's byte hash is recorded in the
machine-readable evidence.

Together with the independent DLSS Swapper campaign, this satisfies the
two-application native field-campaign floor for `windows-winui`.
