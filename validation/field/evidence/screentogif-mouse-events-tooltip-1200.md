# windows-wpf field campaign: screentogif-mouse-events-tooltip-1200

The selected ScreenToGif defect reproduced on native x86_64 Windows and the
fixed revision removed it.

- Affected revision: `e44f1f1ef0086fcb3cd85d55156e94a2957e3dd1`
- Fixed revision: `d71dfbcf02749da7d9303d9c716d76372339acb0`
- Automation: Windows UI Automation plus pointer hover in logged-on session 1
- Profile: fresh portable `Settings.xaml` and owned run root for every run
- Trigger: open one one-pixel frame, select the Image tab, and hover Mouse Events
- Observable: the live WPF `ControlType.ToolTip` name

The exact diff changes only
`ScreenToGif/Resources/Localization/StringResources.fr.xaml`. The affected
French resource says `Clics de souris`; the fixed resource says
`Événements de souris`.

| Role | Run | Tooltip | Result |
|---|---:|---|---|
| affected | 1 | Clics de souris (Alt + I) | reproduced |
| affected | 2 | Clics de souris (Alt + I) | reproduced |
| affected | 3 | Clics de souris (Alt + I) | reproduced |
| fixed | 1 | Événements de souris (Alt + I) | fixed |
| fixed | 2 | Événements de souris (Alt + I) | fixed |
| fixed | 3 | Événements de souris (Alt + I) | fixed |
| affected control | 1 | Ajouter des formes (Alt + J) | legal |
| fixed control | 1 | Ajouter des formes (Alt + J) | legal |

The adjacent Shapes control proves that the application loaded the French
resources on both revisions and that the change is specific to the Mouse
Events command. The trigger needs no recording, input hooks, encoder, media
download, or network access.

Every record verified the exact `ScreenToGif.dll` hash, expected revision
label, interactive session ownership, UIA readiness, and portable language
code `fr`. Every cleanup reported zero remaining owned processes and removed
both the portable settings and owned run root. The scheduled task was removed
after the campaign.

Both revisions were built with the isolated .NET SDK 6.0.428 and runtime
6.0.36. Both build logs report a successful build with zero errors. Raw
records remain on the VM, and their ordered byte hash is recorded in the
machine-readable evidence. The retained JSON files are concatenated in lexical
filename order for that hash.

Together with the independent Flow Launcher campaign, this satisfies the
two-application native field-campaign floor. Target promotion remains a
separate corpus and support-manifest decision.
