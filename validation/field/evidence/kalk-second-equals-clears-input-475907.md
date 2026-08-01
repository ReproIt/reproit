# linux-qt-quick field campaign: kalk-second-equals-clears-input-475907

- Repository: https://invent.kde.org/utilities/kalk
- Issue: https://bugs.kde.org/show_bug.cgi?id=475907
- Affected revision: 67e5d3dd76425bf946687586288463c6df9508fe
- Fixed revision: 662aa91d58160fca8538f01d7a35253d351214c4
- Expected identity: input-state:second-equals-clears-the-result
- Minimized action: type 1+1, press equals, then press equals a second time, and read the display back through the AT-SPI text interface
- Neighboring legal behavior: the first equals yields 2 on both revisions, so the equals path and the display read are both sound and only the second press differs
- Worker image digest: sha256:8371c2351f251f4d7f41165a959f706ae7dc8d81222059e19520749fa1d67ffa
- Worker image assembly: 116 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: after typing ['1+1', '2'], after the first equals ['2'], after Return []
- fixed: after typing ['1+1', '2'], after the first equals ['2'], after Return ['2']

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | input-state:second-equals-clears-the-result | true | true | 12.611 |
| affected | 2 | input-state:second-equals-clears-the-result | true | true | 12.731 |
| affected | 3 | input-state:second-equals-clears-the-result | true | true | 12.722 |
| fixed | 1 | none | true | true | 12.741 |
| fixed | 2 | none | true | true | 12.721 |
| fixed | 3 | none | true | true | 12.725 |
