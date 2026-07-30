# Web Chromium production-to-local evidence

This directory retains the sanitized D5 production-to-local chain for
`web-chromium`.

- Qualification: `FixtureQualified`
- CLI revision: `git:d47b153d3f37c2ec3e9e703bd13d7d633a8d532e`
- SDK revision: `git:d47b153d3f37c2ec3e9e703bd13d7d633a8d532e`
- Playwright engine: `chromium`
- Cloud endpoint: `https://cloud.reproit.com` (Fly release v82, the build that
  serves the occurrence id the bucket detail advertises)
- Retained record: `record.json`
- Chain SHA-256:
  `sha256:860a019751d380bdfd18bf55fc94c7e1c0e22089ab55cadab1e93fdd07f3564f`

The run created a disposable project, ingested 500 strict protocol-v1
occurrences, retained redaction markers, materialized the resulting bucket,
reproduced it locally through both supported commands, and deleted the project.
All required stages are present and `qualificationBlockers` is empty.

The record binds the target to the Playwright engine and every stage to its
sanitized digest. Live account, project, and publishable credentials are not
retained.
