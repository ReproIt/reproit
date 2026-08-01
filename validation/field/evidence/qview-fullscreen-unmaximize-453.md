# linux-qt-widgets field campaign: qview-fullscreen-unmaximize-453

- Repository: https://github.com/jurplel/qView
- Issue: https://github.com/jurplel/qView/issues/453
- Affected revision: 9f6c225451bb060af8fafd948839432a6de32f4a
- Fixed revision: e28cbe7b8521959777f40ad6a43b62b4ee243b28
- Expected identity: window-state:resized-after-fullscreen-round-trip
- Minimized action: open the View menu, click Enter Full Screen, then activate Exit Full Screen, and read the frame extents back before and after the round trip
- Neighboring legal behavior: the same full-screen round trip performed from a maximized window restores both the maximized geometry and the X11 maximized state on the affected build, so only the window-size restore path is wrong
- Worker image digest: sha256:68a93f1dee3a9f2efd4bd9d961ea1e24af11a0032dbf1eeaf374a1dbad1e4641
- Worker image assembly: 6 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: frame extents before the round trip [162, 142, 316, 196] and after [482, 302, 316, 196]. The fixture image already matches the window size, so setWindowSize() shows up here as the window being moved rather than scaled
- fixed: frame extents before the round trip [162, 142, 316, 196] and after [162, 142, 316, 196]. The fixture image already matches the window size, so setWindowSize() shows up here as the window being moved rather than scaled

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | window-state:resized-after-fullscreen-round-trip | true | true | 41.119 |
| affected | 2 | window-state:resized-after-fullscreen-round-trip | true | true | 0.908 |
| affected | 3 | window-state:resized-after-fullscreen-round-trip | true | true | 0.918 |
| fixed | 1 | none | true | true | 0.92 |
| fixed | 2 | none | true | true | 0.904 |
| fixed | 3 | none | true | true | 0.923 |
