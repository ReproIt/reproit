# reproit linux-atspi validation harness.
#
# Stands up a headless Linux desktop (Xvfb) with the AT-SPI2 accessibility bus
# running, a GTK3 fixture app with stable accessible-ids and a planted dead-end
# bug, and BOTH ways to drive it:
#   1. the raw runner: runners/linux-atspi.py (live AT-SPI capture + drive)
#   2. the reproit binary itself, built natively for linux from this workspace,
#      so `reproit map` / `reproit fuzz` can orchestrate the runner in-container.
#
# Build context is the repo root (so the workspace + runners are available).
# Build:  docker build -f examples/linux-atspi/Dockerfile -t reproit-atspi .
# Run:    docker run --rm reproit-atspi            (full validation)

# ---- stage 1: build the reproit binary for linux ----------------------------
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY templates ./templates
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && \
    rm -rf /var/lib/apt/lists/*
# Only the `reproit` binary is needed for the desktop loop (no cloud/db deps).
RUN cargo build --release -p reproit

# ---- stage 2: the a11y runtime ----------------------------------------------
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
# Force the GTK atk-bridge on and point apps + the runner at a session bus we
# launch ourselves (no systemd / no real login session in a container).
ENV GTK_MODULES=gail:atk-bridge \
    NO_AT_BRIDGE=0 \
    GTK_A11Y=atspi \
    DISPLAY=:99 \
    PYTHONUNBUFFERED=1

RUN apt-get update && apt-get install -y --no-install-recommends \
      xvfb \
      dbus dbus-x11 \
      at-spi2-core \
      libgtk-3-0 gir1.2-gtk-3.0 \
      gir1.2-atspi-2.0 \
      python3 python3-gi python3-gi-cairo \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# reproit binary from the build stage.
COPY --from=build /src/target/release/reproit /usr/local/bin/reproit

# reproit's DesktopAtspi backend spawns the runner with `uv run <script>`.
# We don't need uv's resolver here (PyGObject comes from apt's python3-gi, not
# from the script's inline pip dep), so shim `uv run` -> system python3.
RUN printf '#!/bin/sh\nif [ "$1" = "run" ]; then shift; exec python3 "$@"; fi\nexec python3 "$@"\n' \
      > /usr/local/bin/uv && chmod +x /usr/local/bin/uv

# The runners (the live AT-SPI driver lives here) and the fixture + scripts.
WORKDIR /work
COPY runners /work/runners
COPY signature_vectors.json /work/signature_vectors.json
COPY examples/linux-atspi/fixture_app.py /work/fixture_app.py
COPY examples/linux-atspi/run.sh /work/run.sh
COPY examples/linux-atspi/reproit.yaml /work/reproit.yaml
RUN chmod +x /work/run.sh

ENTRYPOINT ["/work/run.sh"]
