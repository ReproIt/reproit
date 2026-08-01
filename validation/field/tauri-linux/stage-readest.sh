#!/usr/bin/env bash
# Build phase for readest. Checks out one exact revision and produces an
# unpackaged Linux x86_64 Tauri binary. The revision arrives in `revision`, set
# by the campaign adapter's per-run phase context.
#
# readest is a pnpm workspace whose Tauri frontend is a Next.js static export,
# so the build has three steps the cc-switch build does not:
#   1. setup-vendors copies pdfjs, simplecc, and jieba into public/vendor,
#      without which the export references files that do not exist offline;
#   2. the export is driven by NEXT_PUBLIC_APP_PLATFORM=tauri, which is what
#      `dotenv -e .env.tauri` supplies, and which selects output: 'export';
#   3. the repository's beforeBuildCommand also uploads sourcemaps, which needs
#      the network and an upstream token, so it is replaced with the export.
set -euo pipefail

test -n "${revision:-}" || { echo "revision is not set" >&2; exit 2; }

cd /work
git checkout -q --force "$revision"
git clean -qfd apps/readest-app/src apps/readest-app/src-tauri/src
# packages/foliate-js is a submodule, and the pdfjs vendor assets the export
# needs live inside it. Without this the vendor step fails with a postcss
# "valid list of files" error that looks like a tooling problem and is not.
git submodule update --init --recursive --depth 1

export CI=true
export CARGO_TERM_COLOR=never
# readest's dependency graph is large enough that one rustc per core exhausts
# the container's memory, and an out-of-memory rustc leaves half-written
# artifacts behind. Cargo then reports "can't find crate for cc" inside build
# scripts, which reads like a toolchain fault and is not one. Bound the jobs.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export NEXT_PUBLIC_APP_PLATFORM=tauri
export NEXT_TELEMETRY_DISABLED=1

corepack enable >/dev/null 2>&1 || true
pnpm install --frozen-lockfile --config.dangerouslyAllowAllBuilds=true
pnpm --filter @readest/readest-app setup-vendors

cd apps/readest-app
pnpm exec next build
pnpm exec tauri build --debug --no-bundle \
  --config '{"build":{"beforeBuildCommand":""}}'

# readest is a cargo workspace, so the artifact lands in the workspace target
# directory at the repository root, not under src-tauri.
binary="$(find /work/target/debug -maxdepth 1 -type f -perm -u+x -name readest | head -1)"
test -n "$binary" || { echo "no Tauri binary was produced" >&2; exit 1; }
echo "staged $revision -> $binary"
