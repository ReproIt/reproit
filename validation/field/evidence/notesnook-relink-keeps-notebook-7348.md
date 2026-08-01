# react-native-android field campaign: notesnook-relink-keeps-notebook-7348

- Repository: https://github.com/streetwriters/notesnook
- Issue: https://github.com/streetwriters/notesnook/issues/7348
- Affected revision: 14f727d6e630f60299f1ceae42e48685e87cba8f
- Fixed revision: 7c3fdab6eec0c083ef4c3b12ff16ba0d2f8aff2c
- Affected APK: sha256:0e4cc6f1e804b5a40da4e7e798a3201c9c88bfa2c1e8babc5c5e3b42a2d24805
- Fixed APK: sha256:06302f8e554df5460d2f870a76809dc4c2b8374e269b0ad509d83397f6dc09d8
- Expected identity: react-native-state:single-select-relink-keeps-previous-notebook
- Minimized action: create notebooks Alpha and Beta, create one note, link it to Alpha, then reopen the link screen in single-select mode, tap Beta, and save
- Neighboring legal behavior: linking the same note to one notebook and not relinking it leaves the note in that notebook alone on both revisions
- Worker image: reproit-android-x86-07cae9bc60e8a40263ee@sha256:df8ee9949675531bfebd509d3a4774bd2377c11227a039cd1a6519e0e0bc8ea0
- Campaign setup: 308 s of wall time on the worker outside the observations themselves, which is eleven recreated AVDs and their boots plus the runtime preflight. The pinned worker image was already built on this worker and its assembly step took 2 s, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none, one recreated pixel_6 AVD per observation
- Seconds below are the observation's own install-to-verdict time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1, read from the note row's own content-desc:

- affected: linked to Alpha, Beta
- fixed: linked to Beta

| Revision | Run | Identity | Notebooks after the relink | Observation reached | Seconds |
|---|---|---|---|---|---|
| affected | 1 | react-native-state:single-select-relink-keeps-previous-notebook | Alpha, Beta | true | 61.936 |
| affected | 2 | react-native-state:single-select-relink-keeps-previous-notebook | Alpha, Beta | true | 61.781 |
| affected | 3 | react-native-state:single-select-relink-keeps-previous-notebook | Alpha, Beta | true | 63.944 |
| fixed | 1 | none | Beta | true | 62.623 |
| fixed | 2 | none | Beta | true | 63.303 |
| fixed | 3 | none | Beta | true | 62.453 |

Controls, each a full run on a recreated AVD:

| Control | Revision or variant | Identity | Notebooks after the trigger |
|---|---|---|---|
| neighboring first link | affected | none | Alpha |
| neighboring first link | fixed | none | Alpha |
| corpus cleanFirstLink | first-link | none | Alpha |
| corpus adversarialRestoredSelection | adversarial-restored-selection | none | Alpha |
| corpus adversarialMultiSelect | adversarial-multi-select | none | Alpha, Beta, Gamma |

Fixed run 1 took two infrastructure attempts. The first Appium session was
refused with `adb: device offline` while the driver was clearing the hidden API
policy, before any step of the trigger ran, so the bounded retry recreated the
AVD and ran the whole observation again. That is the harness's own device
readiness and not a property of either revision; the reason string is retained
verbatim in the runs file, and every other observation in this campaign took
one attempt.
