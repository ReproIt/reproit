# windows-avalonia field campaign: picview-corner-close-targets-320

The selected PicView corner-close defect reproduced three times on native
x86_64 Windows, and the exact fixed revision removed it three times.

- Affected revision: `fd7acc2535ef8b2e7edeeb9d6b8507f09e3b411c`
- Fixed revision: `00cd32fdcc2332fc48ba1465e600b852ca09ee25`
- Automation: Windows UI Automation in interactive session 1
- Subject: the changed `WinMainWindow` and its `CloseButton` peer
- Network: blocked inbound and outbound for the exact executable

The exact fix attaches `CaptionButtonCornerHandler` to `WinMainWindow`, extends
the title-bar button panel to the top and right edges, and marks the close
button as a window-decoration close target. The harness reads the main window
and close-button bounds from UIA, focuses the native window, then performs one
foreground `SendInput` click at screen coordinate `(757,1)`, the window's
top-right pixel.

| Role | Run | Close button bounds | Window closed | Result |
|---|---:|---|---|---|
| affected | 1 | `(727,1)` to `(757,32)` | no | defect reproduced |
| affected | 2 | `(727,1)` to `(757,32)` | no | defect reproduced |
| affected | 3 | `(727,1)` to `(757,32)` | no | defect reproduced |
| fixed | 1 | `(728,0)` to `(758,32)` | yes | defect removed |
| fixed | 2 | `(728,0)` to `(758,32)` | yes | defect removed |
| fixed | 3 | `(728,0)` to `(758,32)` | yes | defect removed |
| affected control | 1 | center `(742,16)` | yes | legal close works |

Every run verified exact application hashes, both active firewall block rules,
interactive UIA readiness, a reached window-presence observation, zero
remaining PicView processes, removal of the firewall rules, configuration, and
run root, and pointer restoration.

Raw JSON remains at `C:\lab\campaigns\avalonia\corner-evidence` in the native
VM. The seven records concatenated in lexical filename order hash to
`c3cfee2a295e900fdf9ff4a00c80beabd558bc7d01f1f9b1ead27226d6c9bf67`.
