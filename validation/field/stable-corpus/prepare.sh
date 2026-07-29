#!/usr/bin/env bash
set -euo pipefail

FIELD=/field
WORK=/work
mode="${1:-all}"

case "$mode" in
  all|web|tui) ;;
  *)
    echo "usage: prepare.sh [all|web|tui]" >&2
    exit 2
    ;;
esac

clone_exact() {
  local name="$1"
  local repository="$2"
  local revision="$3"
  local destination="$WORK/$name"

  git clone --filter=blob:none --no-checkout "$repository" "$destination"
  git -C "$destination" fetch --depth 1 origin "$revision"
  git -C "$destination" checkout --detach "$revision"
  test "$(git -C "$destination" rev-parse HEAD)" = "$revision"
}

build_slidev() {
  local source="$1"
  local fixture="$2"
  local output="$3"

  cd "$WORK/$source"
  export CYPRESS_INSTALL_BINARY=0
  export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
  pnpm install --frozen-lockfile
  pnpm run build
  cp "$FIELD/stable-corpus/$fixture" demo/starter/corpus-slides.md
  pnpm -C demo/starter exec slidev build \
    corpus-slides.md \
    --out "$WORK/$output"
}

if [[ "$mode" != tui ]]; then
  clone_exact \
    vert \
    https://github.com/VERT-sh/VERT.git \
    a8386ee3f1efc40c37828f780e75cb3a8df4b12b
  clone_exact \
    slidev-monaco-source \
    https://github.com/slidevjs/slidev.git \
    7d7aad8d2e0c3117227ed8e8840439723568c1ae
  clone_exact \
    slidev-hash-source \
    https://github.com/slidevjs/slidev.git \
    8b7ccf13358b904636d476072a0b67a857115a10

  (
    cd "$WORK/vert"
    bun install --frozen-lockfile
    PUB_ENV=production \
      PUB_HOSTNAME=localhost \
      PUB_PLAUSIBLE_URL='' \
      bun run build
  )
  (build_slidev slidev-hash-source slidev-hash.md slidev-hash)
  (build_slidev slidev-monaco-source slidev-monaco.md slidev-monaco)

  test -f "$WORK/vert/build/index.html"
  test -f "$WORK/slidev-hash/index.html"
  test -f "$WORK/slidev-monaco/index.html"
fi

if [[ "$mode" != web ]]; then
  clone_exact \
    fx \
    https://github.com/antonmedv/fx.git \
    14b2139b55627a823201aac8972699daf90076ce
  clone_exact \
    nnn \
    https://github.com/jarun/nnn.git \
    c73600a0da993b4675a6e6c7357546d5de22b4d1

  mkdir -p "$WORK/bin"
  (
    cd "$WORK/fx"
    go build -trimpath -o "$WORK/bin/fx" .
  )
  (
    cd "$WORK/nnn"
    make -j2
    cp nnn "$WORK/bin/nnn"
  )

  test -x "$WORK/bin/fx"
  test -x "$WORK/bin/nnn"
fi
