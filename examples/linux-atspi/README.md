# linux-atspi validation harness

A repeatable Docker harness that stands up a headless Linux desktop with the
AT-SPI2 accessibility bus and validates the **live** reproit Linux desktop loop
(`DesktopAtspi` backend, `runners/linux-atspi.py`) against a real GTK3 app.

This is the Linux twin of `~/code/idea/winvm`: where winvm makes a Windows UIA
session repeatable, this makes the Linux AT-SPI session repeatable, in a
container, on any machine with Docker.

## What it proves

1. **Signature parity** (`runners/test_signature.py`): both Python runners
   reproduce all 15 canonical golden vectors bit-for-bit (same as the Rust
   oracle). Runs first, needs no a11y stack.
2. **Live AT-SPI capture + drive** (`runners/linux-atspi.py`): the runner walks
   the real AT-SPI tree of a GTK3 app, computes the canonical structural
   signature, drives taps through the accessibility action interface, and emits
   the `EXPLORE:STATE` / `EXPLORE:EDGE` marker protocol reproit parses.
3. **Full reproit orchestration**: the `reproit` binary, built natively for
   Linux inside the image, runs `reproit map` (drives the runner, builds
   `.reproit/appmap.json`) and `reproit fuzz` (finds the planted bug).
4. **A planted non-crash bug is caught**: the fixture has a dead-end screen
   (reachable via "Get Stuck", no way out). `reproit fuzz --frontier` reaches it
   and the **`no-dead-end` invariant oracle fires** with a deterministic repro.
   A stuck/sink state, not a crash, so the run stays clean.

## Files

- `Dockerfile` - two stages: (1) build the `reproit` binary for linux from the
  workspace, (2) an `ubuntu:24.04` runtime with Xvfb + dbus + at-spi2-core +
  GTK3 + PyGObject + `gir1.2-atspi-2.0`, the runners, the fixture, and a `uv`
  shim (reproit spawns the runner via `uv run`; the shim maps that to the
  system `python3` so apt's `python3-gi` is used).
- `fixture_app.py` - a GTK3 app (GtkStack of Home / Settings / Help / Dead End)
  with stable accessible-ids on every control via
  `widget.get_accessible().set_accessible_id(...)`. The Dead End page is the
  planted bug: reachable, no forward exit.
- `run.sh` - the entrypoint: brings up Xvfb (`:99`), a private D-Bus session
  bus (`dbus-launch`), the AT-SPI bus (`at-spi-bus-launcher
  --launch-immediately`) and registry (`at-spi2-registryd`), launches the
  fixture, then runs all phases.
- `reproit.yaml` - `platform: gtk` (resolves to `DesktopAtspi` on Linux),
  `executable: Fixture` (the AT-SPI app-name substring the runner matches).
- `debug_tree.py` - diagnostic: dumps the live AT-SPI tree and drives a few
  taps. Run with the dir bind-mounted (see below); not part of the image.

## Run it

```sh
# from the repo root (build context must be the workspace)
docker build -f examples/linux-atspi/Dockerfile -t reproit-atspi .
docker run --rm reproit-atspi
```

Modes (first arg): `all` (default), `runner` (phase A only), `reproit`
(phase B only), `shell` (set up the desktop + bus, then drop into bash).

```sh
docker run --rm reproit-atspi runner
docker run --rm -it reproit-atspi shell
```

For ad-hoc tree inspection, bind-mount this dir and override the entrypoint:

```sh
docker run --rm -it --entrypoint bash \
  -v "$PWD/examples/linux-atspi:/dbg" reproit-atspi -c '
    Xvfb :99 -screen 0 1280x900x24 >/tmp/x.log 2>&1 & sleep 1.5
    eval "$(dbus-launch --sh-syntax)"; export DBUS_SESSION_BUS_ADDRESS
    /usr/libexec/at-spi-bus-launcher --launch-immediately >/tmp/b.log 2>&1 &
    /usr/libexec/at-spi2-registryd --use-gnome-session >/tmp/r.log 2>&1 &
    export GTK_MODULES=gail:atk-bridge NO_AT_BRIDGE=0; sleep 1.5
    python3 /work/fixture_app.py >/tmp/f.log 2>&1 & sleep 3
    python3 /dbg/debug_tree.py Fixture'
```

## Expected output (verdict)

```
PASS: linux-atspi 15/15 vectors
...
PHASE A  runner: EXPLORE:STATE/EDGE over 3 states, JOURNEY DONE
PHASE B  reproit map: 3 states, 4 transitions -> .reproit/appmap.json
PHASE B2 reproit fuzz: 6 seed(s) multi-seed batch, frontier steering,
         seed 1 FINDING (1 violation(s): no-dead-end x1)
DONE  parity=0 runner=0 reproit-map=0
```

## Notes / gotchas (learned standing this up)

- **accessible-id on GTK3**: `gtk_widget_set_name` does NOT surface as the
  AT-SPI `accessible-id`. Use `widget.get_accessible().set_accessible_id(...)`
  (`Atk.Object.set_accessible_id`, present on GTK 3.24.41). Without a stable id,
  the Home and Help screens hash to the *same* structural signature (same roles,
  localized text excluded by design) and the explorer cannot tell them apart.
- **app name**: a PyGObject app registers its AT-SPI application name as
  `python3` unless you call `GLib.set_prgname(...)`. The fixture sets it to
  `Fixture` so `REPROIT_TARGET=Fixture` resolves via the runner's substring
  match.
- **`reproit fuzz` batching (multi-seed)**: reproit's default multi-seed batch
  writes a `{"batch":[...]}` config and expects the runner to wrap each seed in
  `SEED:BEGIN <seed>` / `SEED:END <seed>` markers. `linux-atspi.py` now parses
  that batch shape and emits those markers per seed (the same contract as the
  Flutter/web/RN runners), so the harness drives fuzz on the **default
  multi-seed path** (no `--batch 1` workaround) with correct frontier steering
  and per-seed coverage. Between seeds the runner does a best-effort reset
  (several Escape presses) since AT-SPI has no widget-tree reset; the legacy
  single-seed `{"seed":..}` shape still runs with no SEED markers, unchanged.

## What ran where

Everything ran **inside the container** (Linux arm64): the AT-SPI stack and the
fixture are Linux-only, and the `reproit` binary is built for linux in the image
so the whole `map` -> `fuzz` loop runs natively against the live a11y bus. The
host (macOS) only builds the image and the host `reproit` binary
(`cargo build -p reproit`), which cannot drive AT-SPI itself.
```
