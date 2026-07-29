# windows-wpf field campaign: flowlauncher-system-language-4518

The selected Flow Launcher defect reproduced on native x86_64 Windows and the
fixed revision removed it.

- Affected revision: `447fbea4882b7d1df9bf6c510752dc236a30a115`
- Fixed revision: `eb3da1edb942983c14b7c78a4d50dee368c23002`
- Automation: Windows UI Automation in logged-on session 1
- Profile: fresh portable `UserData` for every run
- Trigger: set the current user culture to `fr-FR`, keep `Language` set to
  `system`, and launch the application
- Observable: `TitleTextBlock.Name` in `FlowWelcomeWindow`

The affected code found the supported `fr` language and then fell through to
the English default. The fix returns after the match. UI Automation reported
`Welcome to Flow Launcher` in all three affected runs and
`Bienvenue dans Flow Launcher` in all three fixed runs.

| Role | Run | Culture | UIA title | Result |
|---|---:|---|---|---|
| affected | 1 | fr-FR | Welcome to Flow Launcher | reproduced |
| affected | 2 | fr-FR | Welcome to Flow Launcher | reproduced |
| affected | 3 | fr-FR | Welcome to Flow Launcher | reproduced |
| fixed | 1 | fr-FR | Bienvenue dans Flow Launcher | fixed |
| fixed | 2 | fr-FR | Bienvenue dans Flow Launcher | fixed |
| fixed | 3 | fr-FR | Bienvenue dans Flow Launcher | fixed |
| affected control | 1 | en-US | Welcome to Flow Launcher | legal |
| fixed control | 1 | en-US | Welcome to Flow Launcher | legal |

The English control proves that the affected build is not simply failing to
load all language resources. The one-action trigger is minimal: creating the
portable profile and launching the application is sufficient. No keyboard or
mouse input is needed.

Every record verified the exact `Flow.Launcher.Core.dll` hash, the expected
revision label, interactive session ownership, UIA readiness, and generated
`Settings.json` value `Language="system"`. Every cleanup reported zero
remaining owned processes, removed the portable profile, and restored the
original `en-US` culture.

The executable was built with .NET SDK 9.0.316. Both build logs report a
successful build with zero errors. Network was not isolated because the
limited interactive task cannot install a machine firewall rule; the
minimized launch and UIA read require no network.

This completes the Flow Launcher half of the two-application field floor. The
independent ScreenToGif campaign supplies the other half. Target promotion
remains a separate corpus and support-manifest decision. The machine-readable
evidence and retained raw-record hashes are in
`flowlauncher-system-language-4518.json`. The retained JSON files, concatenated
in lexical filename order, hash to
`f89529894d444414c1cc84d0a8ead8e76eb6c1b257ff065be2e2a2b3fddececc`.
