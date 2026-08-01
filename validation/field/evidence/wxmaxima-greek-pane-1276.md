# linux-wxwidgets field campaign: wxmaxima-greek-pane-1276

- Repository: https://github.com/wxMaxima-developers/wxmaxima
- Issue: https://github.com/wxMaxima-developers/wxmaxima/issues/1276
- Affected revision: e5c410e884c3f1b24c54ac1179c16c3cf0283247
- Fixed revision: 684d2ed4e106fc0fc174f5537129bfd13ba68b93
- Expected identity: aui-perspective:greek-pane-forced-open-on-launch
- Minimized action: launch against a clean profile and read the checked state of the Greek Letters item on the View menu, which wxMaxima derives from the pane's own IsShown()
- Neighboring legal behavior: two neighbouring panes keep their own defaults at both revisions, Main Toolbar shown and Statistics hidden, and the Greek pane still responds to its own View toggle, so the reader is neither blind nor reporting a stale snapshot
- Worker image digest: sha256:39d93d824f94a79851e485466afc88c31792ada0a68a3cc6b8ece5e111d36c48
- Worker image assembly: 449 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: Greek Letters view item checked=True on a clean profile; after its own toggle checked=False
- fixed: Greek Letters view item checked=False on a clean profile; after its own toggle checked=True

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | aui-perspective:greek-pane-forced-open-on-launch | true | true | 0.533 |
| affected | 2 | aui-perspective:greek-pane-forced-open-on-launch | true | true | 0.529 |
| affected | 3 | aui-perspective:greek-pane-forced-open-on-launch | true | true | 0.537 |
| fixed | 1 | none | true | true | 0.531 |
| fixed | 2 | none | true | true | 0.545 |
| fixed | 3 | none | true | true | 0.557 |
