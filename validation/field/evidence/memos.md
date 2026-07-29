# Memos backend-contract field evidence

- Issue: https://github.com/usememos/memos/issues/5443
- Affected revision: `14fb38f37560541bf2719647e7e8b1468937f8ef`
- Fixed revision: `7c3fcc297d8e5a955d9c0bc4f3ca917854132e8e`
- Affected image ids: `sha256:23c3fe0410aa43cc095c7f54c528e0b2077e1583dfb0c14a7a858b52f1232c60`
- Fixed image ids: `sha256:ce68d20f33603a0e4a7f77c41fb699df36bb5c43ec1039eaa95a3ad452cf12ce`
- Oracle identity: `public-memo-list-requires-authentication`
- Runs: three affected and three fixed, each from a fresh SQLite data directory.
- Containment: an internal per-run Docker network, read-only root filesystem,
  loopback-only ephemeral host binding, and cleanup verification.
- Contract revision check: `proto/api/v1` and `proto/gen/openapi.yaml` have no diff across the affected
  and fixed revisions. `build-images.sh` enforces that with `cmp` and `diff`.

Run the campaign and validators from the repository root:

```sh
python3 validation/field/backend_contract/run_campaign.py
python3 validation/field/backend_contract/write_records.py \
  target/reproit-validation/backend-contract-field/summary.json
python3 validation/field/check-benchmark.py validation/field/backend-contract.json
python3 validation/field/check-corpus.py validation/field/corpus/backend-contract.json
validation/backend/cli-e2e/run.sh
validation/backend/run-linux-docker.sh
```
