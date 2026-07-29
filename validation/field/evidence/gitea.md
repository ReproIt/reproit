# Gitea backend-contract field evidence

- Issue: https://github.com/go-gitea/gitea/issues/35886
- Affected revision: `98c61942aa433342eacf08e4040ded80b1d0efe1`
- Fixed revision: `4812e354866a066dcb899af667b0fad5fa094065`
- Affected image ids: `sha256:2b6aaf46753ee21bf0c1b282f6d30636aabe08080cff75b27d7d23b7cc2df021`
- Fixed image ids: `sha256:9c7d9ffbef357a510b8bc6f9ddd5b28657ea6f4acf9039983121d15bb6ec3b97`
- Oracle identity: `filtered-commit-total-count-ignores-bounds`
- Runs: three affected and three fixed, each from a fresh SQLite data directory.
- Containment: an internal per-run Docker network, read-only root filesystem,
  loopback-only ephemeral host binding, and cleanup verification.
- Contract revision check: `templates/swagger/v1_json.tmpl` have no diff across the affected
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
