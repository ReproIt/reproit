# linux-gtk field campaign: gnome-clocks-world-dialog-default-focus-393

- Repository: https://gitlab.gnome.org/GNOME/gnome-clocks
- Issue: https://gitlab.gnome.org/GNOME/gnome-clocks/-/issues/393
- Affected revision: 1283eb4668d83fd710e9b272abca1443f96ff21f
- Fixed revision: 6055f282826d3ac817697e33697142899989c269
- Expected identity: dialog-focus:initial-focus-on-cancel-instead-of-entry
- Minimized action: activate Add World Clock and read which object holds the AT-SPI focused state once the dialog has mapped, before any input
- Neighboring legal behavior: one Tab moves focus to the location entry on the affected build, so the entry is focusable and the dialog's focus chain is sound; only the initial assignment is wrong
- Worker image digest: sha256:7b4a7e619624fdf48befd1f7d2d871dc5f726cc1a57b15ec7b0850f24eb1a553
- Worker image assembly: 220 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: focused on dialog map=[('button', 'Cancel')], entry reachable by Tab=True
- fixed: focused on dialog map=[('entry', '')], entry reachable by Tab=True

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | dialog-focus:initial-focus-on-cancel-instead-of-entry | true | true | 3.716 |
| affected | 2 | dialog-focus:initial-focus-on-cancel-instead-of-entry | true | true | 3.7 |
| affected | 3 | dialog-focus:initial-focus-on-cancel-instead-of-entry | true | true | 3.71 |
| fixed | 1 | none | true | true | 3.048 |
| fixed | 2 | none | true | true | 2.954 |
| fixed | 3 | none | true | true | 2.946 |
