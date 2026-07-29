# Stable schema-3 ratchet, native x86_64 diagnostic

## Scope

This diagnostic attempted the missing clean and adversarial corpus gate for:

- `web-chromium`
- `web-firefox`
- `web-webkit`
- `tui`

It ran on the native Fedora x86_64 host reached through
`black@zgx-5a09.local -> strix`. The harness prepared exact fixed revisions of
VERT, Slidev, fx, and nnn with network access, then started the runtime cases
in owned Docker containers with `--network none`.

## Command

```sh
ssh black@zgx-5a09.local \
  'ssh strix "cd .cache/reproit-stable-corpus-run/repo &&
  bash validation/field/run-stable-corpus.sh"'
```

## Initial result

The exact VERT fixed revision built successfully. Both exact Slidev fixed
revisions installed and built successfully, including the retained hash-route
and Monaco neighboring-behavior fixtures. fx and nnn also compiled at their
exact fixed revisions.

The first offline browser case did not start because the worker lacked the
`xauth` executable required by `xvfb-run`:

```text
xvfb-run: error: xauth command not found
```

The Dockerfile was corrected to declare `xauth`, then the campaign received
one authorized corrected rerun.

## Corrected rerun

The corrected worker built with `xauth`. The exact VERT and Slidev revisions
built again, including both retained Slidev fixtures, and fx and nnn compiled
again at their exact revisions.

The first offline browser runtime then failed before observation because the
harness used the wrong in-container probe path:

```text
Error: Cannot find module '/field/probe-web.mjs'
...
Node.js v20.19.4
```

The probe is stored at `/field/stable-corpus/probe-web.mjs`. Per the corrected
rerun authorization, execution stopped at this failure. No browser or TUI
observation was accepted, no corpus record was generated, and none of the four
targets was promoted.

## Probe-path correction

The probe path was corrected and the bounded campaign was executed again on
the same native x86_64 worker. All four probes reached their intended
observations, and the cleanup trap removed every owned runtime container and
worker image. The generated records then failed the corpus validator because
the known-good subjects produced false-positive identities:

| Target | Confirmed false positives | Primary signal |
| --- | ---: | --- |
| `web-chromium` | 3 | VERT's optional FFmpeg CDN import failed offline; Slidev reported a denied wake lock |
| `web-firefox` | 2 | VERT's optional FFmpeg CDN import failed offline |
| `web-webkit` | 2 | VERT's optional FFmpeg CDN import failed offline |
| `tui` | 2 | fx rendered an invalid empty-file percentage; the nnn neighboring filter produced no observable screen |

The validator rejected every generated record, so none was copied into the
canonical corpus directory. The records and their hashes are retained only
under `target/reproit-validation/stable-transfer/result` for diagnosis. This is
now a product-oracle and subject-selection blocker rather than missing worker
infrastructure.

## Cleanup evidence

After the corrected rerun failed, the owned cleanup trap removed the worker
image and runtime containers. A read-only audit returned:

```text
processes=0
containers=0
images=0
generated=electron-linux.json,
```

The final line proves that the failed diagnostic did not leave a corpus record
for any of the four targets.

After the probe-path correction, a second cleanup audit reported:

```text
containers=0
images=0
```
