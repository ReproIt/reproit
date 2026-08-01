# windows-winui field campaign: dlss-swapper-minimized-window-position-829

The selected DLSS Swapper defect reproduced on native x86_64 Windows and the
fixed revision removed it.

- Affected revision: `8cd0ccccce1470844d4e3f9294eecbbad3c25672`
- Fixed revision: `09bfaa388cbab84cf8f416576b6e401d64f98e10`
- Automation: Windows UI Automation in logged-on session 1
- Application model: unpackaged WinUI 3, `WindowsPackageType None` with a
  self-contained Windows App SDK, built with the .NET 10.0.302 SDK
- Profile: the portable `StoredData` directory is removed before and after
  every run
- Trigger: minimize, close, relaunch, all through the window pattern
- Observable: the `BoundingRectangle` the relaunched window reports

Windows parks a minimized window at the sentinel coordinate -32000. On the
affected revision the persistence code stores that position on exit and
restores it on the next launch, so the application comes back permanently
offscreen. Pull request 830 changes `src/Data/WindowPositionRect.cs` so the
sentinel is never written.

Both the minimize and the close go through the UI Automation window pattern,
never a raw Win32 call, so the trigger never leaves the target's own automation
tree. That was the open question the candidate record raised, and this is the
answer to it.

| Role | Run | First launch origin | Relaunch origin | Result |
|---|---:|---|---|---|
| affected | 1 | 26,26 | -32000,-32000 | reproduced |
| affected | 2 | 78,78 | -32000,-32000 | reproduced |
| affected | 3 | 130,130 | -32000,-32000 | reproduced |
| fixed | 1 | 26,26 | 26,26 | not reproduced |
| fixed | 2 | 78,78 | 78,78 | not reproduced |
| fixed | 3 | 130,130 | 130,130 | not reproduced |
| affected control | 1 | 26,26 | 26,26 | legal |
| fixed control | 1 | 78,78 | 78,78 | legal |

Every affected run landed on the single identity
`restored-window-parks-at-minimized-sentinel-position`, with the relaunched
window reporting `WindowVisualState Normal` while sitting at -32000,-32000:
the window is not minimized, it is placed where a minimized window lives.
Every fixed run reached the same observation point and returned the window to
the on-screen origin the first launch had used.

The neighbouring legal control closes the window through the same window
pattern from its normal visual state, so the identical persist-on-exit path
runs without the minimize step. On both exact revisions the relaunched window
comes back on screen, so the observation is not simply reporting every close.

The first-launch origin cascades across runs, 26, 78, 130, because Windows
staggers successive top-level windows. That is why the fixed control asserts
the relaunch matches its own first launch rather than a fixed constant, and it
is also independent evidence that each run really did start a fresh window
rather than reattaching to a previous one.

Containment: the portable configuration keeps every byte of its state in
`<publish>\StoredData`, which the harness removes before the run and again in
its cleanup block, so nothing outside the run root is touched. Program-scoped
inbound and outbound Windows Firewall rules block the published executable,
which stops the manifest and release downloads, and both rules are removed on
exit.

One setup step is recorded rather than hidden: the first launch of a fresh
portable profile shows a multiplayer advisory content dialog. Every run
dismissed exactly one notice through its `CloseButton` and installed nothing.

Every record verified the exact `DLSS Swapper.dll` hash, the absence of any
pre-existing owned process, interactive session ownership, that the first
launch was on screen before the trigger, that the window advertised the
minimize capability, that it reached the minimized visual state, and that the
process exited after the pattern closed it. Every cleanup reported zero
remaining owned processes, a removed `StoredData` directory and removed
firewall rules.

Raw records remain on the VM and each run's byte hash is recorded in the
machine-readable evidence.

Together with the independent UniGetUI campaign, this satisfies the
two-application native field-campaign floor for `windows-winui`.
