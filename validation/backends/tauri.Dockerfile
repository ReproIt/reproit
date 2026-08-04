FROM rust:bookworm AS rust-toolchain
RUN rustup update stable && rustup default stable
FROM node:22-bookworm AS node-toolchain
FROM ubuntu:24.04

COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
COPY --from=node-toolchain /usr/local /usr/local

ENV PATH=/usr/local/cargo/bin:/usr/local/bin:$PATH \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config ca-certificates curl \
    libssl-dev libwebkit2gtk-4.1-dev webkit2gtk-driver \
    libayatana-appindicator3-dev librsvg2-dev \
    xvfb dbus dbus-x11 at-spi2-core ffmpeg xdotool \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install tauri-driver --locked
