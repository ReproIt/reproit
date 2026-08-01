# linux-qt-widgets field campaign: keepassxc-autogenerate-charset-13073

- Repository: https://github.com/keepassxreboot/keepassxc
- Issue: https://github.com/keepassxreboot/keepassxc/issues/13073
- Affected revision: caa7d1476134d86c1cf769081d8460933f4cd11c
- Fixed revision: 58a2919650f814e042daf0f51fe7c76705f0288c
- Expected identity: generator-settings:new-entry-password-ignores-saved-length
- Minimized action: store a password-generator configuration with a distinctive length, open Entries then New Entry, and read the character count of the auto-generated password field
- Neighboring legal behavior: the same stored configuration used through the explicit Tools then Password Generator dialog is honoured on the affected build, so the settings write itself is not what fails
- Worker image digest: sha256:68a93f1dee3a9f2efd4bd9d961ea1e24af11a0032dbf1eeaf374a1dbad1e4641
- Worker image assembly: 6 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: stored generator length 7, new-entry password character count 32
- fixed: stored generator length 7, new-entry password character count 7

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | generator-settings:new-entry-password-ignores-saved-length | true | true | 3.937 |
| affected | 2 | generator-settings:new-entry-password-ignores-saved-length | true | true | 3.875 |
| affected | 3 | generator-settings:new-entry-password-ignores-saved-length | true | true | 3.85 |
| fixed | 1 | none | true | true | 3.943 |
| fixed | 2 | none | true | true | 3.891 |
| fixed | 3 | none | true | true | 3.908 |
