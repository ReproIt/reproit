# Web Chromium production-to-local evidence

This directory retains the sanitized D5 production-to-local chain for
`web-chromium`.

- Qualification: `FixtureQualified`
- CLI revision: `git:8cd2e7f63dd86740f1686d59e047cd8def7eeae1`
- SDK revision: `git:8cd2e7f63dd86740f1686d59e047cd8def7eeae1`
- Cloud endpoint: `https://cloud.reproit.com`
- Retained record: `record.json`
- Chain SHA-256:
  `sha256:d26d19d94911ca53e2719e4459b2cd7a1abfe6f34d073b62f0c60b0c151e3c3d`

The run created a disposable project, ingested 500 strict protocol-v1
occurrences, retained redaction markers, materialized the resulting bucket,
reproduced it locally through both supported commands, and deleted the project.
All required stages are present and `qualificationBlockers` is empty.

The record binds every stage to its sanitized digest. Live account, project,
and publishable credentials are not retained.
