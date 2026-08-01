# React Native Android field campaign runner

`validation/field/android/react_native_android_campaign.py` and its container
entry point `run_react_native_android_field_driver.sh` have existed for both
qualified applications, joplin and music, with no committed caller at all: a
repository-wide search for `REPROIT_FIELD_APPLICATION` matched the entry point
and nothing else. The campaign therefore could not be started by anyone, on any
host, without hand-assembling a Docker invocation.

The caller is committed as two files, modelled directly on
`validation/release/run-android-x86-remote.sh` and its remote worker:

- `validation/field/android/run-react-native-android-field-remote.sh`, the host
  half. It validates the application, the two application archives and the run
  count, ships the tracked tree plus a payload archive holding
  `affected.apk`, `fixed.apk` and, for the music campaign, the media fixture
  directory, then collects the evidence archive into
  `target/reproit-validation/react-native-android-field/<application>`.
- `validation/field/android/react-native-android-field-worker.sh`, the remote
  half. It runs on the x86_64 host reached through the zgx gateway, verifies
  every uploaded digest including each application archive independently,
  refuses a pair whose two archives are identical, asserts the host and the
  Docker engine are native x86_64 with KVM, builds the same pinned worker image
  the release lane builds, and runs the campaign in one bounded named container
  with Docker network mode none.

## Why the remote route is not optional

The react-native-android bound is `android-emulator/x86_64`. Every Android
system image installed on the development machine is `arm64-v8a`, so this
campaign cannot run locally at all. The zgx gateway to strix is the only route
to a native x86_64 emulator with KVM, and it is the same route the Android
release gates already take, so the worker image, the SDK root and the download
cache are shared rather than duplicated.

## Bounds

- The source archive and the payload archive are each capped at 512 MiB, and so
  is the returned evidence archive.
- Each application archive is capped at 512 MiB before it is packed.
- `--runs` accepts 1 to 3, matching the campaign module's own bound.
- The remote command runs under `timeout 15000`.
- The owned remote directory name is matched against a fixed pattern before
  anything is written to it or removed from it.
- Evidence archive members are checked for absolute paths and `..` before
  extraction.
- Container, AVD, and remote run-directory cleanup all run from traps.

## What the first executed run found

The runner shipped unexercised, and the first real invocation of it, a music
campaign against APKs built at both selected revisions, did not reach a single
reproduction. It stopped in `validate_runtime` before the first emulator boot.
Three defects in the committed harness are answered here, each of which was
invisible to inspection and could only be found by running it:

1. `react_native_android_runtime.validate_runtime` required the environment
   variable `REPROIT_WORKER_IMAGE`, and no caller in the repository ever set
   it: a repository-wide search matched the assertion and nothing else. Every
   campaign would have aborted with `worker image provenance was None`. The
   worker now passes the identity of the image it actually built.
2. The same assertion compared that variable against a hardcoded content
   digest. The worker builds the image locally from
   `validation/release/android-x86/Dockerfile` and never pushes it, so its
   digest is a per-host build artifact and cannot be reproduced on another
   machine; the strix build produced
   `sha256:501c60972ec2f47beca5f6a6f609f0e3d60f1b21dc2117e3ab5262b5112744b5`
   against a pin of `sha256:8868695e...`. The pin is now the image NAME, which
   already embeds the first 20 hex of the Dockerfile digest and therefore pins
   the recipe exactly, and the content digest is retained as evidence instead of
   guessed in advance.
3. `REPROIT_FIELD_AVD_HOME` was the bind mount `/android-avd` itself.
   `Device.stop` removes its AVD home with `shutil.rmtree`, which cannot remove
   a mount point, so `cleanup_audit` reported `avdDirectoryExists: True` and the
   campaign failed its own cleanup audit unconditionally. The AVD home is now
   `/android-avd/run`, a directory inside the mount that the campaign creates
   and can therefore delete, which is also what the audit is trying to assert.

The first run is retained as the diagnostic it is: it proves the route, the
uploads, the digest checks, the x86_64 and KVM assertions and the worker image
build all work, and that the harness could never have got past its own preflight.
