# UI issue evidence ledger

This ledger records public issue-report facts and Reproit execution evidence. Reviewing an issue is
not detection or reproduction. No application in this table was cloned, built, or run as part of
this review, so every Reproit result is unverified.

| Repository and application surface | Public issue report | Reproit execution | Observed evidence |
| --- | --- | --- | --- |
| Immich, web photo management | [#19359](https://github.com/immich-app/immich/issues/19359) reports a web UI freeze after creating a share link | Not run | No Reproit finding or replay was produced |
| Immich, Android media editing | [#11800](https://github.com/immich-app/immich/issues/11800) reports a null failure during crop or rotate | Not run | No Reproit finding or replay was produced |
| Mattermost Desktop, desktop notifications | [#3132](https://github.com/mattermost/desktop/issues/3132) reports stale UI after notification activation | Not run | No Reproit finding or replay was produced |
| Visual Studio Code, Linux desktop startup | [#216856](https://github.com/microsoft/vscode/issues/216856) reports a startup crash involving user data or shared memory | Not run | No Reproit finding or replay was produced |
| Lazygit, terminal rendering | [#3930](https://github.com/jesseduffield/lazygit/issues/3930) reports different rendering across terminal hosts | Not run | No Reproit finding or replay was produced |
| PowerToys, Windows protocol activation | [#39735](https://github.com/microsoft/PowerToys/issues/39735) reports failure to open settings through protocol activation | Not run | No Reproit finding or replay was produced |

The linked issue pages are the sources for the report descriptions. This ledger makes no statement
about cause, detectability, reproducibility, required product changes, or release readiness.
