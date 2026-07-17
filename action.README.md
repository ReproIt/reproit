# reproit GitHub Action

Add reproit to CI in five lines. On every pull request it fuzzes your app, and on a finding it
uploads a minimized, annotated repro to the cloud and posts the PR comment: summary, suspected
`file:line`, cohort, inline clip, dashboard link.

```yaml
- uses: reproit/reproit@v1
  with:
    app: http://localhost:3000
    bucket: web-pr
    api-key: ${{ secrets.REPROIT_CLOUD_KEY }}
```

See `.github/workflows/reproit-example.yml` for the full job (checkout, start the app, then the
action) that you copy into your own repo.

## Inputs

| Input       | Required | Default                     | Description                                                                                                |
| ----------- | -------- | --------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `app`       | yes      |                             | What to fuzz: a URL (`http://localhost:3000`) or a build dir. Zero-config.                                 |
| `bucket`    | yes      |                             | Cloud bucket the finding's evidence is published to (e.g. `web-pr`).                                       |
| `api-key`   | yes      |                             | reproit cloud API key. Pass a secret.                                                                      |
| `cloud-url` | no       | `https://cloud.reproit.com` | Cloud base URL.                                                                                            |
| `journey`   | no       | `explore`                   | Explorer journey to drive.                                                                                 |
| `runs`      | no       | `20`                        | Number of fuzz seeds to try.                                                                               |
| `version`   | no       | `latest`                    | reproit release to install (e.g. `v0.1.88`), or `latest`.                                                  |
| `only`      | no       | (empty)                     | Restrict to these oracle categories (comma list: `crash,jank,leak,visual,divergence,a11y,graph`).          |
| `no`        | no       | (empty)                     | Exclude these oracle categories (comma list). Applied after `only`.                                        |
| `record`    | no       | `false`                     | On a finding, confirm and record the repro on the simulator tier. Needs a runner with a simulator (macOS). |
| `cloud-app` | no       | (empty)                     | Cloud app id to attach evidence to. Only needed on CLI versions that require it alongside the bucket.      |

There are no action outputs. The action's product is the uploaded evidence and the posted PR
comment; results live in the dashboard the comment links to.

## Permissions

The comment poster needs write access to the PR. Grant it in the consuming job:

```yaml
permissions:
  contents: read
  pull-requests: write
  issues: write
```

The default `GITHUB_TOKEN` posts the comment; no extra secret is needed for that. Store your cloud
key as the `REPROIT_CLOUD_KEY` repository secret.

## How it installs

The action installs a released reproit binary (plus the web runner bundle) via the repo's
`install.sh`, pinned to `version`. It does not compile from source, so onboarding is seconds. For
web targets it also fetches chromium and its system libraries with `--with-deps` so headless Chrome
runs on a clean CI image.

## Composite action vs the reusable workflow

This repo ships two ways to run reproit in CI, on purpose:

- **This action** (`uses: reproit/reproit@v1`, `action.yml`): the onboarding tier. Composite,
  runner-agnostic, installs a released binary. Step-level `uses:` you drop into any job. Start here.
- **`.github/workflows/reproit-pr.yml`**: a `workflow_call` reusable workflow that builds reproit
  from source on macOS with an iOS simulator, the tier that records repro video. Job-level `uses:`
  with a `secrets:` block.

They are kept separate rather than one calling the other because they differ in both install
strategy (released binary vs from-source build) and runner (any runner vs macOS-with-simulator), and
they are consumed differently (step-level `with:` vs job-level `secrets:`). Collapsing them would
regress the reusable workflow's from-source dogfooding and its simulator recording tier. Use the
action for onboarding; use the reusable workflow when you specifically want the from-source,
simulator-recorded tier.
