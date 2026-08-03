# Session capsule (Class B): what was measured

Umbrella plan track 5. The property Class A never needed: the trigger is a
TIMED INPUT STREAM, not a single event. The tick mechanics (input events
stamped with the ordinal of the program's clock reads, replay holding each
event back until the program reaches that tick again) were built and measured
in `validation/process/MEASUREMENT.md`, Phase 2. This gate makes that
machinery meet the track 5 acceptance: a third-party engine sample, the
portability bar, the fix flip, and a tamper that diverges NAMING THE TICK.

Reproduce every number with `validation/session/gate-session.sh`. Off Linux
it drives itself through Docker.

## What a program must do to be replayable this way

Stated plainly, because frame-perfect replay is not free:

- The program must take its time from the clock the capsule controls
  (`clock_gettime`, `gettimeofday`, `time`, whatever crosses the libc
  boundary). A fixed timestep loop already does: it reads the clock every
  frame, so the tick ordinal counts frames without the program exposing a
  frame counter.
- It must POLL its input rather than block on it. A program that blocks on
  input without ever reading its clock cannot be scheduled; it is served
  early after a bounded number of holds (`MAX_INPUT_HOLDS`) with
  `inputEarly` counted, never quietly reordered.
- An engine that free-runs on wall clock (no fixed timestep, timing read
  from a source the boundary cannot see) needs the one-call time source
  adoption the plan describes, the same way it would adopt a logger. The
  capsule does not pretend otherwise.

## The table

Platform for every row: Docker linux/arm64 on an arm64 host, image
`reproit-session-gate` (built by the gate's inline Dockerfile:
`rust:1.97.1-trixie` plus libsdl2-dev, python3, libatspi2.0-dev), Debian
trixie, glibc 2.41, seccomp completeness layer active. Subjects and
engine versions: SDL 2.32.4 (`validation/process/engine.c`, fixed timestep
loop on SDL's timer and event pump, `SDL_VIDEODRIVER=dummy`), and
bevy_app/bevy_ecs 0.16.1 (`fixtures/engine-session-bevy`, bevy's
`ScheduleRunnerPlugin` fixed timestep runner, headless by construction).
Both plant the SAME defect: a stale combo that fires only when presses
arrive FAR APART, so a replay that ignored the recorded schedule would not
reproduce it. The premise row pins that direction first.

| row | SDL2 engine | bevy_app 0.16.1 |
| --- | --- | --- |
| same bytes back to back | exit 0, survived (SAFE, by design) | same |
| spread session, captured | SIGABRT via assert, capsule written | exit 101 via panic, capsule written |
| portability: clean copy, different absolute path, original deleted, no input attached | reproduced by re-execution, exit 1 | same |
| fix (`REPROIT_FIXED` guard) | the program now exits cleanly, exit 0 | same |
| input tick moved by hand | DIVERGED `input-tick`, exit 3, refused before the program ran | same |
| gate total | 10/10 | (same run) |

The tamper divergence names the event and both ticks, e.g.:

```
input event 1 records tick=6 but the log places it at tick=239; the input
schedule was altered after capture
```

## The false certificate the tick check closed

Measured BEFORE the check existed, on the SDL subject: moving the last
press's recorded tick next to the first press's (so the schedule claims the
presses arrived back to back, the direction the premise row proves safe)
replayed as

```
PASS the program now exits cleanly    exit 0, 0 divergences
```

which is a false FIXED verdict from an edited capsule. The invariant that
closes it: in a single threaded recording every tick increment appends
exactly one clock or time entry, so the tick stamped on an input event
always equals the count of clock and time entries preceding it in the log.
Replay verifies that at load and refuses by name (`input-tick`, exit 3)
before the program runs, like `seccomp-required`. An untampered capsule
passes the same check on every replay, so every green row above also
exercises the invariant; it is not assumed.

What the check does NOT cover, named: editing an input event's BYTES is
consistent with any schedule and replays as the session those bytes
describe. That is the same trust class as editing a recorded file's content
anywhere else in this format; the capsule is evidence against drift, not a
cryptographic seal. Deleting an input event or a clock entry moves every
later event's position and is caught by the same count.

## A second run-to-run identity defect this subject exposed

The bevy row's first portability run reproduced the panic and still
verdicted INCONCLUSIVE: recorded exit 101, observed exit 101, and the
failure identities differed. Rust's default panic line embeds the OS thread
id (`thread 'main' (1681) panicked at ...`), which is new on every run, so
a panic identity could never match its own replay. Exactly that token is
now folded (`fold_thread_id` in the process capsule module); the thread
NAME is kept, because two panics on differently named threads are different
failures. Pinned by unit test alongside the existing only-hex-folds rule.

## Named limits

- Input is the stdin stream (fd 0), which is how a headless engine is
  driven in a container; evdev and window-server input devices are not
  interposed. A named gap, not a silent one: reads of unrecorded devices
  diverge like any other boundary miss.
- The tick invariant is a single-threaded property, like the shim's own
  state (no locking, by design). An engine that reads clocks from several
  threads is outside what this boundary records faithfully today.
- The input stream shares the capsule's entry bound (8192 entries, last
  slot reserved for the `capsule-entries` marker); a session past it
  refuses by name at load (`capsule-bound`), never a silent prefix.
- The recorded working directory must still exist at replay; a missing one
  abstains INCONCLUSIVE with that named cause (see the relative-path
  section of the process MEASUREMENT).
- The counters line the CLI prints after a replay can come from the `sh -c`
  wrapper's own shim instance rather than the engine's (both emit
  `REPROIT:PROCESS-REPLAY`, and `watch` keeps the last one seen:
  `crates/reproit/src/workflows/process_capsule/mod.rs`). The divergence
  LINES are the authority, as the process MEASUREMENT already states; the
  verdicts above do not depend on the counters.
- raylib with `PLATFORM_DRM` was considered as the third-party subject and
  not measured: it needs a DRM render node (`/dev/dri`) a plain container
  does not have. bevy_app runs headless with no device at all, which is why
  it is the measured engine row.
