FROM golang:1.23.12-bookworm AS go-toolchain
FROM oven/bun:1.2.21-debian AS bun-toolchain
FROM node:20.19.4-bookworm

COPY --from=go-toolchain /usr/local/go /usr/local/go
COPY --from=bun-toolchain /usr/local/bin/bun /usr/local/bin/bun

ENV DEBIAN_FRONTEND=noninteractive \
    PATH=/usr/local/go/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        git \
        libncursesw5-dev \
        libreadline-dev \
        pkg-config \
        python3 \
        python3-pyte \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY runners/web/package.json runners/web/package-lock.json /opt/web/
RUN cd /opt/web \
    && npm ci --ignore-scripts \
    && npx playwright install --with-deps chromium firefox webkit \
    && corepack enable \
    && corepack prepare pnpm@9.1.3 --activate
RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux xauth \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work
