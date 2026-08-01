# linux-wxwidgets field campaign: poedit-source-view-no-line-900

- Repository: https://github.com/vslavik/poedit
- Issue: https://github.com/vslavik/poedit/issues/900
- Affected revision: c4fe890ef72c8c8cbc6b9b8cc1784dba10447798
- Fixed revision: f261986fe726a5c2a0ca0717179740c31875de57
- Expected identity: source-reference:line-less-reference-not-resolved
- Minimized action: open a catalog whose first entry references a source file with no line number, activate Show Code Occurrences, and read the code occurrence viewer back
- Neighboring legal behavior: the second entry references the same file with a line number and resolves on the affected build, so the viewer, the file and the base path are all sound and only the missing line number breaks it
- Worker image digest: sha256:39d93d824f94a79851e485466afc88c31792ada0a68a3cc6b8ece5e111d36c48
- Worker image assembly: 449 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: line-less reference errors=['Source code not found'] source shown=False; line-numbered reference errors=[] source shown=True
- fixed: line-less reference errors=[] source shown=True; line-numbered reference errors=[] source shown=True

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | source-reference:line-less-reference-not-resolved | true | true | 6.532 |
| affected | 2 | source-reference:line-less-reference-not-resolved | true | true | 6.529 |
| affected | 3 | source-reference:line-less-reference-not-resolved | true | true | 6.706 |
| fixed | 1 | none | true | true | 6.456 |
| fixed | 2 | none | true | true | 6.461 |
| fixed | 3 | none | true | true | 6.526 |
