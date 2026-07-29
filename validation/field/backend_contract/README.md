# Backend-contract field campaign

This campaign validates two independent open-source applications at exact
affected and fixed revisions:

- Gitea issue 35886, filtered commit pagination reports an unfiltered
  `X-Total-Count`.
- Memos issue 5443, the authored anonymous public-memo operation incorrectly
  requires authentication.

`build-images.sh` fetches only the four full commit ids and builds them with
digest-pinned Dockerfile frontend, Go, and Alpine inputs. Override
`REPROIT_FIELD_PLATFORM` only when validating an additional Docker platform.

```sh
validation/field/backend_contract/build-images.sh
```

The runtime campaign performs three affected and three fixed runs for each
application. Every run gets:

- a fresh owned SQLite directory;
- a fresh internal Docker network with no external egress;
- a read-only container root filesystem;
- an ephemeral host binding configured for `127.0.0.1` only;
- bounded readiness and request timeouts;
- deterministic seeded accounts and application data;
- container, network, and state-directory cleanup on success or failure.

The HTTP probe container is pinned by digest. No production service or
third-party API is contacted during runtime.

```sh
python3 validation/field/backend_contract/run_campaign.py
python3 validation/field/backend_contract/write_records.py \
  target/reproit-validation/backend-contract-field/summary.json
python3 validation/field/check-benchmark.py \
  validation/field/backend-contract.json
python3 validation/field/check-corpus.py \
  validation/field/corpus/backend-contract.json
validation/backend/cli-e2e/run.sh
validation/backend/run-linux-docker.sh
```

The raw run records and service logs stay under
`target/reproit-validation/backend-contract-field`. The writer reduces them to
reviewable evidence records whose hashes bind every benchmark and corpus
observation to one raw run.
