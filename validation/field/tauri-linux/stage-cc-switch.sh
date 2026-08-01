#!/usr/bin/env bash
# Build phase for cc-switch. Checks out one exact revision and produces an
# unpackaged Linux x86_64 Tauri binary. The revision arrives in `revision`, set
# by the campaign adapter's per-run phase context.
#
# `tauri build` is not used: it bundles deb/AppImage artifacts the campaign does
# not need, and the campaign drives the binary directly through tauri-driver.
set -euo pipefail

test -n "${revision:-}" || { echo "revision is not set" >&2; exit 2; }

cd /work
git checkout -q --force "$revision"
git clean -qfd src src-tauri

export CI=true
export CARGO_TERM_COLOR=never

corepack enable >/dev/null 2>&1 || true
# pnpm 10 refuses to run dependency build scripts unless they are approved, and
# exits non-zero when it skips them. esbuild needs its postinstall to place its
# platform binary, so approval is passed on the command line rather than by
# editing the subject repository.
pnpm install --frozen-lockfile --config.dangerouslyAllowAllBuilds=true
pnpm run build:renderer
# `cargo build` alone produces a binary that resolves the frontend to the dev
# server URL, which does not exist offline. `tauri build` resolves it to the
# bundled dist instead; --debug keeps the faster profile and --no-bundle skips
# the deb and AppImage artifacts the campaign does not drive.
pnpm exec tauri build --debug --no-bundle

binary="$(find src-tauri/target/debug -maxdepth 1 -type f -perm -u+x \
  ! -name '*.d' ! -name '*.so' | head -1)"
test -n "$binary" || { echo "no Tauri binary was produced" >&2; exit 1; }
echo "staged $revision -> $binary"
