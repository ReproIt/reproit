# Cloud release gates

These checks cover the production path from SDK capture to a local, deterministic replay.

## SDK performance and privacy

```sh
node validation/cloud/sdk-performance.js
```

The benchmark runs 100,000 privacy-preserving input fingerprints and 10,000 SDK flushes containing 50 events each. It fails if raw input content reaches the serialized fingerprint or if the local overhead exceeds the release ceilings.

## Production SDK to replay

Log in to hosted Cloud, install the web runner's Playwright browser, then run:

```sh
validation/cloud/run-production-loop.sh
```

The gate:

1. Creates a uniquely named disposable Cloud project.
2. Sends 500 SDK-shaped production error occurrences with sensitive sentinels.
3. Checks hosted ingest latency and throughput.
4. Fetches the structural bug and replay package.
5. Proves the raw sentinels are absent and redaction metadata remains.
6. Pulls the bucket into a clean source workspace and reproduces it locally.
7. Deletes the disposable project, including on failure.

Set `REPROIT_CLOUD_ACCOUNT_KEY` in CI. A local run can use the account token saved by `reproit login`.

The latest successful measurements are stored in `validation/cloud/artifacts/`.
