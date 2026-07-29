# Linux AT-SPI amd64 local gate

This is diagnostic working-tree evidence, not exact-commit release evidence.

Command:

```sh
DOCKER_DEFAULT_PLATFORM=linux/amd64 \
  bash .github/scripts/atspi-scenario-e2e.sh
```

Environment:

- Docker Desktop on an arm64 macOS host
- Ubuntu 24.04 amd64 container
- Rust 1.88 bookworm toolchain
- GTK 4, Xvfb, D-Bus, and AT-SPI 2

Observed result:

- The amd64 image built successfully.
- The current ReproIt working tree compiled successfully inside the container.
- Both actors exited before the first action because the owned GTK fixture did
  not appear on the AT-SPI application bus.
- The runner reported `no AT-SPI application matching "/work/fixture"`.
- The same gate passed against the same working tree in the native arm64
  container.

Promotion consequence:

Linux GTK remains Preview until the amd64 worker proves fixture process
readiness and AT-SPI discovery, then completes the field and corpus gates.
