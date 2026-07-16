#!/bin/sh
# reproit installer. Fetches the reproit binary and provisions the web runner so
# `reproit fuzz https://yoursite.com` works link-and-go, with no env vars and no
# manual `npm install`. macOS + Linux. (Windows: use install.ps1 from this repo,
# or `cargo install reproit` which self-heals on first run.)
#
#   curl -fsSL https://reproit.com/install.sh | sh
#
# Honors:
#   REPROIT_BIN_DIR   where the `reproit` binary lands     (default ~/.local/bin)
#   REPROIT_VERSION   tag to install, e.g. v0.1.2          (default: latest)
# Internal release gate:
#   REPROIT_RELEASE_BASE          asset base URL instead of GitHub Releases
#   REPROIT_SKIP_BROWSER_INSTALL  skip Playwright's browser download
set -eu

REPO="ReproIt/reproit"
BIN_DIR="${REPROIT_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# fetch URL DEST LABEL: download, failing loudly and distinguishing a missing
# release asset (HTTP 404) from a network/server problem. Without -f, curl exits
# 0 on an HTTP error and hands us the status code instead, so we can report it.
fetch() {
  f_url=$1; f_dest=$2; f_label=$3
  f_code="$(curl -sSL -o "$f_dest" -w '%{http_code}' "$f_url")" \
    || die "network error downloading $f_label from $f_url"
  case "$f_code" in
    200) ;;
    404) die "$f_label not found (HTTP 404) at $f_url
       the release may not carry this asset; check https://github.com/$REPO/releases" ;;
    *)   die "download failed for $f_label (HTTP $f_code) from $f_url" ;;
  esac
}

# fetch_optional URL DEST LABEL: like fetch, but a 404 returns 1 instead of
# dying (for .sha256 sidecars that older releases did not publish). Any other
# failure is still fatal.
fetch_optional() {
  f_url=$1; f_dest=$2; f_label=$3
  f_code="$(curl -sSL -o "$f_dest" -w '%{http_code}' "$f_url")" \
    || die "network error downloading $f_label from $f_url"
  case "$f_code" in
    200) return 0 ;;
    404) rm -f "$f_dest"; return 1 ;;
    *)   die "download failed for $f_label (HTTP $f_code) from $f_url" ;;
  esac
}

# verify_sha256 FILE SUMFILE LABEL: compare FILE's SHA-256 against the first
# field of SUMFILE (standard "<hex>  <name>" shasum/sha256sum layout).
verify_sha256() {
  v_file=$1; v_sumfile=$2; v_label=$3
  v_want="$(awk '{print $1; exit}' "$v_sumfile" | tr 'A-F' 'a-f')"
  [ -n "$v_want" ] || die "empty checksum file for $v_label"
  if have sha256sum; then
    v_got="$(sha256sum "$v_file" | awk '{print $1}')"
  elif have shasum; then
    v_got="$(shasum -a 256 "$v_file" | awk '{print $1}')"
  else
    say "  (neither sha256sum nor shasum found; skipping checksum verification for $v_label)"
    return 0
  fi
  [ "$v_got" = "$v_want" ] \
    || die "checksum mismatch for $v_label (expected $v_want, got $v_got); refusing to install"
  say "  verified sha256: $v_label"
}

have curl || die "curl is required"
have tar  || die "tar is required"

have_linux_atspi() {
  for p in /usr/lib/libatspi.so.0 /usr/lib64/libatspi.so.0 /usr/lib/*/libatspi.so.0; do
    [ -e "$p" ] && return 0
  done
  return 1
}

install_linux_atspi() {
  have_linux_atspi && return 0
  say "installing the Linux accessibility runtime..."
  if [ "$(id -u)" -eq 0 ]; then
    as_root=""
  elif have sudo; then
    as_root="sudo"
  else
    die "libatspi.so.0 is required; install your distribution's AT-SPI runtime package, then retry"
  fi

  if have apt-get; then
    $as_root apt-get update -qq
    $as_root apt-get install -y libatspi2.0-0
  elif have dnf; then
    $as_root dnf install -y at-spi2-core
  elif have yum; then
    $as_root yum install -y at-spi2-core
  elif have pacman; then
    $as_root pacman -Sy --needed --noconfirm at-spi2-core
  else
    die "libatspi.so.0 is required; install your distribution's AT-SPI runtime package, then retry"
  fi
  have_linux_atspi || die "the AT-SPI runtime was installed but libatspi.so.0 is still unavailable"
}

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
    esac
    install_linux_atspi ;;
  *) die "unsupported OS: $os (Windows: run install.ps1 from this repo instead)" ;;
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
  [ -z "${REPROIT_RELEASE_BASE:-}" ] \
    || die "REPROIT_VERSION is required with REPROIT_RELEASE_BASE"
  say "resolving the latest release..."
  api="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")" \
    || die "could not reach the GitHub API to resolve the latest release (network problem or rate limit); retry, or pin a tag with REPROIT_VERSION=vX.Y.Z"
  tag="$(printf '%s\n' "$api" \
    | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || die "could not resolve the latest release tag from the GitHub API response"
fi
say "installing reproit $tag ($target)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ -n "${REPROIT_RELEASE_BASE:-}" ]; then
  dl="${REPROIT_RELEASE_BASE%/}"
else
  dl="https://github.com/$REPO/releases/download/$tag"
fi

# --- the binary --------------------------------------------------------------
bin_asset="reproit-$tag-$target.tar.gz"
say "  downloading $bin_asset"
fetch "$dl/$bin_asset" "$tmp/bin.tar.gz" "$bin_asset"
if fetch_optional "$dl/$bin_asset.sha256" "$tmp/bin.tar.gz.sha256" "$bin_asset.sha256"; then
  verify_sha256 "$tmp/bin.tar.gz" "$tmp/bin.tar.gz.sha256" "$bin_asset"
else
  say "  (no .sha256 published for $tag; skipping checksum verification)"
fi
mkdir -p "$BIN_DIR"
tar -xzf "$tmp/bin.tar.gz" -C "$BIN_DIR" || die "could not extract $bin_asset"
chmod +x "$BIN_DIR/reproit"
say "  installed -> $BIN_DIR/reproit"

# --- the web runner bundle (runner + node_modules), extracted flat -----------
runner_asset="reproit-web-runner.tar.gz"
say "  downloading web runner"
fetch "$dl/$runner_asset" "$tmp/web.tar.gz" "$runner_asset"
if fetch_optional "$dl/$runner_asset.sha256" "$tmp/web.tar.gz.sha256" "$runner_asset.sha256"; then
  verify_sha256 "$tmp/web.tar.gz" "$tmp/web.tar.gz.sha256" "$runner_asset"
else
  say "  (no .sha256 published for $tag; skipping checksum verification)"
fi
mkdir -p "$web_dir"
tar -xzf "$tmp/web.tar.gz" -C "$web_dir" || die "could not extract $runner_asset"
say "  installed web runner -> $web_dir"

# --- one-time headless browser fetch (skipped if Node is absent) -------------
if [ -n "${REPROIT_SKIP_BROWSER_INSTALL:-}" ]; then
  say "  skipping browser fetch for release validation"
elif have node; then
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
say "Done. Try:  reproit scan https://yoursite.com"
