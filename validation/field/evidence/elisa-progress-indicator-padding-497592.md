# linux-qt-quick field campaign: elisa-progress-indicator-padding-497592

- Repository: https://invent.kde.org/multimedia/elisa
- Issue: https://bugs.kde.org/show_bug.cgi?id=497592
- Affected revision: 8286818ff1c55e9f45c0f64d4600e11655898a90
- Fixed revision: cf0f8b41917ec2de61fe6fc89335cf0939568600
- Expected identity: progress-indicator:elapsed-time-minutes-not-zero-padded
- Minimized action: open a short silent track and read the elapsed-position heading immediately before the Duration slider
- Neighboring legal behavior: the track title heading in the same window reads identically on both revisions, so the same tree read returns unchanged strings for labels the formatter does not produce
- Worker image digest: sha256:8371c2351f251f4d7f41165a959f706ae7dc8d81222059e19520749fa1d67ffa
- Worker image assembly: 116 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: elapsed heading '0:06', total heading '0:30', zero padded=False
- fixed: elapsed heading '00:06', total heading '00:30', zero padded=True

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | progress-indicator:elapsed-time-minutes-not-zero-padded | true | true | 7.223 |
| affected | 2 | progress-indicator:elapsed-time-minutes-not-zero-padded | true | true | 7.156 |
| affected | 3 | progress-indicator:elapsed-time-minutes-not-zero-padded | true | true | 7.176 |
| fixed | 1 | none | true | true | 7.093 |
| fixed | 2 | none | true | true | 7.142 |
| fixed | 3 | none | true | true | 7.107 |
