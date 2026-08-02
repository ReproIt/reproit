#!/usr/bin/env bash
# Gate wrapper for the checkpoint anchoring acceptance (Class C).
#
# validation/process-checkpoint/run.sh proves that an anchor taken of a
# REPLAYING process skips the head of a long run, and that a tampered image, a
# missing image, and a tampered capsule are all refused. It has always been
# runnable by hand and was never wired into CI, which is exactly why an
# environment drift (criu 4.1.1) went unnoticed until someone ran it manually.
#
# Two things about the environment are load bearing, both measured rather than
# assumed (validation/process-checkpoint/MEASUREMENT.md):
#
#   1. The image must be BOOKWORM. criu 4.1.1, which trixie ships, hangs in
#      `criu restore` on this host until it is killed. criu 3.17.1, which
#      bookworm ships, restores the same image fine.
#   2. The reproit binary must be built against the SAME image it runs in. A
#      trixie-built binary needs GLIBC_2.39 and dies at exec on bookworm, which
#      surfaces as "capture did not produce a capsule" and reads like a product
#      defect when it is a loader error.
#
# criu needs CAP_SYS_ADMIN, so the container is privileged. It also refuses to
# dump a process whose files live on a bind mounted host path, so /work is
# deliberately left on the container's own filesystem and only the repository
# is mounted, read only.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
IMAGE="${REPROIT_CHECKPOINT_GATE_IMAGE:-reproit-checkpoint-e2e}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# SELinux hosts need :z on a shared mount; harmless elsewhere.
VOLUME_LABEL=""
if [ -f /sys/fs/selinux/enforce ]; then VOLUME_LABEL=",z"; fi

cat > "$WORK/Dockerfile" <<'EOF'
FROM debian:bookworm

# criu 3.17.1 comes from bookworm on purpose. See the header of this gate.
# libatspi and glib are link-time dependencies of the reproit binary itself,
# not of this gate. Without them the build fails at `cannot find -latspi`, and
# a binary linked against a library the runtime image lacks dies at exec, which
# run.sh reports as a loader error rather than as a failed capture.
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
       criu gcc libc6-dev curl ca-certificates pkg-config \
       libatspi2.0-dev libglib2.0-dev \
  && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain 1.88.0
EOF

cat > "$WORK/inner.sh" <<'EOF'
set -euo pipefail

# Built inside this image so the binary matches its loader. Building it here
# rather than on the host is the whole point: see item 2 in the gate header.
cargo build -p reproit --manifest-path /repo/Cargo.toml \
  --target-dir /tmp/reproit-target

export REPROIT_CHECKPOINT_SCOPE="${REPROIT_CHECKPOINT_SCOPE:-full}"
export REPROIT_ROOT=/repo
export REPROIT_BINARY=/tmp/reproit-target/debug/reproit
exec bash /repo/validation/process-checkpoint/run.sh
EOF

bash "$ROOT/validation/backends/docker-build-retry.sh" -t "$IMAGE" "$WORK"
docker run --rm --privileged \
  -e REPROIT_CHECKPOINT_SCOPE="${REPROIT_CHECKPOINT_SCOPE:-full}" \
  -v "$ROOT:/repo:ro$VOLUME_LABEL" \
  -v "$WORK:/gate:ro$VOLUME_LABEL" \
  "$IMAGE" bash /gate/inner.sh
