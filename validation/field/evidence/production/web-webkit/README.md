# Web WebKit production-to-local evidence

This directory retains the sanitized D5 production-to-local chain for
`web-webkit`.

- Qualification: `FixtureQualified`
- CLI revision: `git:52407f4d58671e05ac541cc8c674785032a2f3af`
- SDK revision: `git:52407f4d58671e05ac541cc8c674785032a2f3af`
- Playwright engine: `webkit`
- Cloud endpoint: `https://cloud.reproit.com`
- Retained record: `record.json`
- Chain SHA-256:
  `sha256:f28b80b146db3f70733d6c1d8046c5d646fad5d4f1438870cb814124982060be`

The run created a disposable project, ingested 500 strict protocol-v1
occurrences, retained redaction markers, materialized the resulting bucket,
reproduced it locally through both supported commands, and deleted the project.
All required stages are present and `qualificationBlockers` is empty.

The record binds the target to the Playwright engine and every stage to its
sanitized digest. Live account, project, and publishable credentials are not
retained.
