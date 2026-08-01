# linux-gtk field campaign: gnome-text-editor-search-entry-a11y-771

- Repository: https://gitlab.gnome.org/GNOME/gnome-text-editor
- Issue: https://gitlab.gnome.org/GNOME/gnome-text-editor/-/issues/771
- Affected revision: 8732544897aada0500e32df6dba1a7259f9ddc7b
- Fixed revision: bf3a1414dc8ab39349c1d24beec89ea417a058b0
- Expected identity: accessibility:search-and-replace-entries-unlabeled
- Minimized action: open a document, press Ctrl+H to open the search bar in replace mode, and read the accessible names of the entries it realises
- Neighboring legal behavior: every button in the same window reports its own accessible name on both revisions, so the reader is not simply blind to the search bar
- Worker image digest: sha256:7b4a7e619624fdf48befd1f7d2d871dc5f726cc1a57b15ec7b0850f24eb1a553
- Worker image assembly: 220 s of wall time on the worker for the whole image, every application and both revisions. The worker reuses any layer it already holds, so this is not a cold-build cost
- Worker: linux/amd64 container on the native x86_64 host, --network none
- Seconds below are the probe's own trigger-to-observation time inside an already-running container, not the container lifetime

Observed difference, affected run 1 versus fixed run 1:

- affected: search entry labeled=False, replace entry labeled=False, entries=[('text', ''), ('text', ''), ('text', '')]
- fixed: search entry labeled=True, replace entry labeled=True, entries=[('text', ''), ('text', 'Search'), ('text', 'Replace')]

| Revision | Run | Identity | Observation reached | Clean launch | Seconds |
|---|---|---|---|---|---|
| affected | 1 | accessibility:search-and-replace-entries-unlabeled | true | true | 1.249 |
| affected | 2 | accessibility:search-and-replace-entries-unlabeled | true | true | 1.244 |
| affected | 3 | accessibility:search-and-replace-entries-unlabeled | true | true | 1.245 |
| fixed | 1 | none | true | true | 1.229 |
| fixed | 2 | none | true | true | 1.252 |
| fixed | 3 | none | true | true | 1.242 |
