#!/bin/sh
# reproit installer. Fetches the reproit binary and provisions the web runner so
# `reproit fuzz https://yoursite.com` works link-and-go, with no env vars and no
# manual `npm install`. macOS + Linux. (Windows: download the .zip from the
# Releases page and run install.ps1, or use `cargo install reproit` which
# self-heals on first run.)
#
#   curl -fsSL https://reproit.com/install.sh | sh
#
# Honors:
#   REPROIT_BIN_DIR   where the `reproit` binary lands     (default ~/.local/bin)
#   REPROIT_VERSION   tag to install, e.g. v0.1.2          (default: latest)
set -eu

REPO="ReproIt/reproit"
BIN_DIR="${REPROIT_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

have curl || die "curl is required"
have tar  || die "tar is required"

# --- target triple from OS/arch (matches the release build matrix) -----------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64)        target="x86_64-apple-darwin" ;;
      *) die "unsupported macOS arch: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-gnu" ;;
      *) die "unsupported Linux arch: $arch (try: cargo install reproit)" ;;
    esac ;;
  *) die "unsupported OS: $os (Windows: download the .zip from the Releases page)" ;;
esac

# --- data dir, byte-for-byte the same path the binary self-heals into --------
# (see config::web_runner_data_dir)
if [ "$os" = "Darwin" ]; then
  data_base="$HOME/Library/Application Support"
else
  data_base="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
web_dir="$data_base/reproit/web"

# --- resolve the release tag -------------------------------------------------
tag="${REPROIT_VERSION:-}"
if [ -z "$tag" ]; then
  say "resolving the latest release..."
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || die "could not resolve the latest release tag"
fi
say "installing reproit $tag ($target)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# --- the binary --------------------------------------------------------------
bin_asset="reproit-$tag-$target.tar.gz"
say "  downloading $bin_asset"
curl -fsSL "https://github.com/$REPO/releases/download/$tag/$bin_asset" -o "$tmp/bin.tar.gz" \
  || die "download failed: $bin_asset"
mkdir -p "$BIN_DIR"
tar -xzf "$tmp/bin.tar.gz" -C "$BIN_DIR"
chmod +x "$BIN_DIR/reproit"
say "  installed -> $BIN_DIR/reproit"

# --- the web runner bundle (runner + node_modules), extracted flat -----------
say "  downloading web runner"
curl -fsSL "https://github.com/$REPO/releases/download/$tag/reproit-web-runner.tar.gz" \
  -o "$tmp/web.tar.gz" || die "download failed: reproit-web-runner.tar.gz"
mkdir -p "$web_dir"
tar -xzf "$tmp/web.tar.gz" -C "$web_dir"
say "  installed web runner -> $web_dir"

# --- one-time headless browser fetch (skipped if Node is absent) -------------
if have node; then
  cli="$web_dir/node_modules/playwright/cli.js"
  if [ -f "$cli" ]; then
    say "  fetching the headless browser (chromium, one-time)..."
    node "$cli" install chromium || say "  (browser fetch failed; it will retry on first fuzz)"
  fi
else
  say "  note: Node.js (18+) was not found. reproit's web fuzzer needs it;"
  say "        install Node, then the browser fetch runs on your first fuzz."
fi

# --- PATH hint ---------------------------------------------------------------
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say ""
     say "Add $BIN_DIR to your PATH:"
     say "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.profile && . ~/.profile" ;;
esac

say ""
say "Done. Try:  reproit fuzz https://yoursite.com"
