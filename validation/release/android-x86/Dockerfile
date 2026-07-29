FROM rust@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0 AS rust
FROM node@sha256:5711a0d445a1af54af9589066c646df387d1831a608226f4cd694fc59e745059 AS node
FROM ubuntu@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90

ARG FLUTTER_ARCHIVE=flutter_linux_3.41.6-stable.tar.xz
ARG FLUTTER_SHA256=503b3e6b7d352fca5d21b6474eca95ad544d8fc3b053782eab63a360c7fc7569

ENV DEBIAN_FRONTEND=noninteractive \
    PATH=/opt/flutter/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTUP_HOME=/usr/local/rustup

COPY --from=rust /usr/local/cargo /usr/local/cargo
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=node /usr/local /usr/local

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        jq \
        libdbus-1-3 \
        libegl-mesa0 \
        libfontconfig1 \
        libgl1-mesa-dri \
        libglx-mesa0 \
        libnss3 \
        libpulse0 \
        libx11-6 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxi6 \
        libxkbcommon0 \
        libxkbcommon-x11-0 \
        libxrandr2 \
        libxrender1 \
        libxcb-cursor0 \
        libxcb1 \
        mesa-utils \
        ninja-build \
        openjdk-17-jdk-headless \
        pkg-config \
        procps \
        python3 \
        unzip \
        util-linux \
        xauth \
        xdg-utils \
        xz-utils \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

RUN curl --fail --location --retry 3 --output "/tmp/${FLUTTER_ARCHIVE}" \
        "https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/${FLUTTER_ARCHIVE}" \
    && printf '%s  %s\n' "$FLUTTER_SHA256" "/tmp/${FLUTTER_ARCHIVE}" \
        | sha256sum --check --strict \
    && tar -xJf "/tmp/${FLUTTER_ARCHIVE}" -C /opt \
    && rm "/tmp/${FLUTTER_ARCHIVE}" \
    && git config --global --add safe.directory /opt/flutter \
    && flutter config --no-analytics \
    && flutter precache --android

RUN npm install --global --no-audit --no-fund appium@3.5.2 \
    && appium driver install uiautomator2@8.0.0 \
    && npm cache clean --force

RUN apt-get update \
    && apt-get install -y --no-install-recommends libatspi2.0-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /repo
