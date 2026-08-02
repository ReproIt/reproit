#!/usr/bin/env bash
set -euo pipefail

# Bounded retry around `docker build` for TRANSIENT registry failures only.
# Usage: docker-build-retry.sh [docker build arguments ...]
#
# Measured trigger (2026-08-02, linux-containers): resolving ubuntu:24.04 died
# with `dial tcp ...: i/o timeout` (DeadlineExceeded) before any layer built.
# Only that class of failure is retried; a real build failure exits at once
# with docker's own status. The build context must be a directory, not stdin:
# a consumed heredoc cannot be replayed on the second attempt.

if [[ "$#" -eq 0 ]]; then
  echo "usage: docker-build-retry.sh DOCKER_BUILD_ARG [ARG ...]" >&2
  exit 2
fi
for argument in "$@"; do
  if [[ "$argument" == "-" ]]; then
    echo "docker-build-retry: stdin build contexts are not retryable" >&2
    exit 2
  fi
done

ATTEMPTS=3
BACKOFF_SECONDS=(10 30)
TRANSIENT='DeadlineExceeded|i/o timeout|TLS handshake timeout'
TRANSIENT+='|connection reset by peer|temporary failure in name resolution'
TRANSIENT+='|failed to do request|unexpected EOF|503 Service Unavailable'

BUILD_LOG="$(mktemp)"
trap 'rm -f "$BUILD_LOG"' EXIT

for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
  set +e
  docker build "$@" 2>&1 | tee "$BUILD_LOG"
  status="${PIPESTATUS[0]}"
  set -e
  if [[ "$status" -eq 0 ]]; then
    exit 0
  fi
  if ((attempt == ATTEMPTS)) || ! grep -qiE "$TRANSIENT" "$BUILD_LOG"; then
    exit "$status"
  fi
  echo "docker build failed on a transient registry error" \
    "(attempt $attempt of $ATTEMPTS); retrying" >&2
  sleep "${BACKOFF_SECONDS[attempt - 1]}"
done

exit 1
