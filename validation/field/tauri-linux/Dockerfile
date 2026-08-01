# Linux worker for the tauri-linux field campaign. Tauri renders in the system
# webview, so the campaign needs WebKitGTK, WebKitWebDriver, and tauri-driver
# rather than a bundled Chromium. This is the same recipe the repository's own
# tauri backend gate already proves works end to end; the campaign adds nothing
# to it but the harness mount.
#
# The platform is pinned in the file, not left to a build flag: the legacy
# builder silently ignores --platform and would produce a worker of a different
# architecture than the declared bound.
# Rust 1.90 is the floor, not a preference: readest pulls roaring 0.11.4, which
# refuses to build on 1.88 and fails the stage with a dependency-resolution
# error rather than a compile error.
FROM --platform=linux/amd64 rust:1.90-bookworm AS rust-toolchain
FROM --platform=linux/amd64 node:22-bookworm AS node-toolchain
FROM --platform=linux/amd64 ubuntu:24.04

COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
COPY --from=node-toolchain /usr/local /usr/local

ENV PATH=/usr/local/cargo/bin:/usr/local/bin:$PATH \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    DEBIAN_FRONTEND=noninteractive

# python3-pyatspi and the AT-SPI typelib are the second observation channel.
# A Tauri application is not only its webview: file choosers, message dialogs,
# and menus are native GTK windows that WebDriver cannot see at all. The webview
# channel stays primary; this one exists for the windows it cannot reach.
# gir1.2-gtk-3.0 is here for the channel's own fixture, not for any subject.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config ca-certificates curl git \
      libssl-dev libwebkit2gtk-4.1-dev webkit2gtk-driver \
      libayatana-appindicator3-dev librsvg2-dev \
      xvfb dbus dbus-x11 at-spi2-core \
      python3 python3-gi python3-pyatspi gir1.2-atspi-2.0 gir1.2-gtk-3.0 \
  && rm -rf /var/lib/apt/lists/*

RUN cargo install tauri-driver --locked

# The campaign launches the application offline, so the WebDriver client has to
# be baked in rather than installed at run time.
RUN mkdir -p /wd && cd /wd && npm install --no-audit --no-fund webdriverio@9

WORKDIR /work
