# Class D: accelerators and parallelism, measured

Date: 2026-07-31. Every number below was produced by running the code in this
directory. Where something could not be measured on this hardware, it is listed
as unmeasured rather than estimated.

Platforms:
- Race measurements: Linux aarch64 in Docker (`gcc:13`, glibc 2.36), on an
  Apple M1 Ultra host.
- Accelerator measurements: macOS host, Metal via PyTorch MPS, torch 2.7.1.
- CUDA measurements: NVIDIA GB10 (Grace Blackwell, compute capability 12.1,
  driver 580.142, aarch64) on the CUDA host, torch 2.11.0+cu128. That machine is
  reachable over ssh and is listed in the operator's own validation inventory.
  An earlier revision of this file said no NVIDIA GPU was available and treated
  CUDA as unmeasurable. That was WRONG: it read "no NVIDIA GPU in this Mac" as
  "no NVIDIA GPU reachable" and turned a local limit into a false global claim.
  Section 3 now carries real CUDA numbers, and they overturn the conclusion the
  gap had left standing.
- Still genuinely unmeasured: NCCL collectives and multi-device training, because
  The CUDA host has exactly ONE GPU. That is a hardware limit, not an assumption.
- Superseded text, kept so the correction is legible: nothing about CUDA, cuDNN
  autotuning, NCCL collectives, or multi-device training was measured here.

Reproduce with `./sweep-race.sh` (Docker) and `uv run --with torch python
mps_determinism.py` (host).

## 1. Thread and schedule nondeterminism

Subject: `race.c`, a publish-before-initialize defect. The producer publishes a
pointer and sets the ready flag before filling the payload, so a consumer that
observes the flag can read uninitialized memory. This is an ordinary logic bug,
not a memory-model subtlety.

Both threads meet at a barrier before the publish, so the measurement is of the
race window and not of thread startup skew. The two variants differ only in what
sits inside the window:

- `pure`: stores and arithmetic only. Nothing crosses the process boundary.
- `libc`: the payload is timestamped, so `clock_gettime` sits in the window.

Schedule fuzzing is `schedfuzz.c`, an LD_PRELOAD that injects `sched_yield` plus
a nanosleep at the libc entry points it can hook.

### Window sweep, 200 runs per cell, 50000ns delay, 50 percent fire rate

| variant | window | natural | fuzzed | delta |
| --- | ---: | ---: | ---: | ---: |
| pure | 0 | 0/200, 0.0% | 2/200, 1.0% | +1.0pp |
| pure | 1 | 0/200, 0.0% | 1/200, 0.5% | +0.5pp |
| pure | 4 | 0/200, 0.0% | 0/200, 0.0% | +0.0pp |
| pure | 16 | 0/200, 0.0% | 0/200, 0.0% | +0.0pp |
| pure | 64 | 1/200, 0.5% | 0/200, 0.0% | -0.5pp |
| pure | 256 | 1/200, 0.5% | 1/200, 0.5% | +0.0pp |
| libc | 0 | 21/200, 10.5% | 95/200, 47.5% | +37.0pp |
| libc | 1 | 5/200, 2.5% | 94/200, 47.0% | +44.5pp |
| libc | 4 | 4/200, 2.0% | 94/200, 47.0% | +45.0pp |
| libc | 16 | 4/200, 2.0% | 95/200, 47.5% | +45.5pp |
| libc | 64 | 10/200, 5.0% | 96/200, 48.0% | +43.0pp |
| libc | 256 | 4/200, 2.0% | 93/200, 46.5% | +44.5pp |

### The mechanism, confirmed

The fuzzed rate is not incidental: it tracks the fire rate almost exactly, which
proves the fuzzer is controlling the outcome rather than coincidentally helping.
`libc` variant, window 16, 200 runs per cell:

| fire rate | observed | rate |
| ---: | ---: | ---: |
| 0% | 34/200 | 17.0% |
| 25% | 68/200 | 34.0% |
| 50% | 105/200 | 52.5% |
| 75% | 146/200 | 73.0% |
| **100%** | **200/200** | **100.0%** |

**At a 100 percent fire rate a 2 percent flaky race becomes 100 percent
reproducible.** That is the strongest honest claim available here, and it is
bounded precisely: it holds only when the race window crosses a boundary the
preload can hook.

### The observer effect, which must be stated

At fire rate 0 the preload injects nothing, yet the rate is 17.0 percent against
a 2.0 percent natural baseline. Merely loading the instrumentation widens the
window. Any reproduction rate measured under instrumentation is therefore a
measurement of the instrumented program, not of the original one. A capsule that
reports "reproduced 100 percent of the time" must be understood as reproduced
under the recorded schedule perturbation.

### What this means for the product claim

- A race whose window crosses a hookable boundary: schedule control is real, and
  can drive reproduction to 100 percent.
- A race that is pure memory traffic between threads: **schedule fuzzing at this
  boundary does nothing.** 0.0 to 1.0 percent, indistinguishable from noise.
  Controlling these requires preemption control at the scheduler level, which is
  what rr's chaos mode and a determinizing hypervisor provide, and is not
  reachable from an LD_PRELOAD.

## 2. Accelerator determinism, measured on Metal

`mps_determinism.py`, torch 2.7.1, MPS backend. Each measurement is a sha256 of
the result bytes. Eight fresh processes, since a capsule replays in a fresh
process and that is the case that decides replayability.

| measurement | distinct values across 8 processes | stable |
| --- | ---: | --- |
| seeded `randn` | 1 | YES |
| `matmul` | 1 | YES |
| large `sum` reduction | 1 | YES |
| same reduction repeated in-process | 1 | YES |
| `scatter_add_` | 8 | **NO** |
| `scatter_add_` with deterministic mode ON | 8 | **NO** |

Within a single process, same seed, six consecutive calls:

```
deterministic mode OFF: 6 distinct results out of 6
deterministic mode ON : 6 distinct results out of 6
torch.are_deterministic_algorithms_enabled() -> True
```

### The finding that matters

`torch.use_deterministic_algorithms(True)` is **accepted** on MPS, reports
itself as **enabled**, and does **not** make `scatter_add_` deterministic. It
does not raise either, which is the documented CUDA behaviour for an op with no
deterministic implementation. On this backend the knob is accepted and
ineffective.

The consequence for a capsule is concrete: **recording "deterministic mode: on"
would record a false assurance.** A capsule must record the requested
configuration AND a measured determinism probe, so it states whether determinism
actually held rather than what was asked for.

Seeded generation, matmul, and even a four million element reduction were stable
across processes, so the instability is specific to order-dependent scatter and
atomics, not to the backend generally.

## 3. Accelerator determinism on CUDA, and why it changes the design

`cuda_determinism.py`, NVIDIA GB10, torch 2.11.0+cu128, eight fresh processes.
Deliberately the same probes in the same order as section 2 so the backends can
be compared line for line.

| measurement | distinct values across 8 processes | stable |
| --- | ---: | --- |
| seeded `randn` | 1 | YES |
| `matmul` | 1 | YES |
| large `sum` reduction | 1 | YES |
| same reduction repeated in-process | 1 | YES |
| `scatter_add_` | **8** | **NO** |
| `scatter_add_` with deterministic mode ON | **1** | **YES** |

Within a single process, same seed, six consecutive calls:

```
deterministic mode OFF: 6 distinct results out of 6
deterministic mode ON : 1 distinct result  out of 6
torch.are_deterministic_algorithms_enabled() -> True
```

### The finding, and it is the strongest one in this document

The SAME api, on the SAME major torch version, gives OPPOSITE answers on the two
backends:

| | Metal (MPS) | CUDA (GB10) |
| --- | --- | --- |
| `use_deterministic_algorithms(True)` | accepted | accepted |
| `are_deterministic_algorithms_enabled()` | `True` | `True` |
| `scatter_add_` across 8 processes, flag ON | **8 distinct** | **1 distinct** |
| flag is load bearing | **NO** | **YES** |

On CUDA the flag does exactly what it says. On Metal it is accepted, reports
itself enabled, and changes nothing. Nothing observable at the API level
distinguishes the two: both accept the call and both answer `True` when asked.

This is decisive for capsule design, and it upgrades the earlier conclusion from
a Metal quirk to a general rule. A capsule cannot record determinism by
recording the REQUEST, because the request is identical in the working case and
the broken one. It must record the request AND a measured probe, and it must
carry the backend identity, because the same flag means different things on
different hardware. Recording "deterministic mode: on" alone would be a false
assurance on Metal and a true one on CUDA, and the capsule has no way to tell
those apart without measuring.

Note also `CUBLAS_WORKSPACE_CONFIG` was unset in every run above. It changes
cuBLAS reduction behaviour, so it is recorded by the probe: a replay under a
different value is a replay under a different contract.

## 4. The envelope a training-shaped capsule needs

| field | recordable today | notes |
| --- | --- | --- |
| framework and version | small extension | envelope already carries runtime, os, arch |
| accelerator device and driver version | new capture | not currently collected |
| full environment block | **yes** | the process capsule already records and restores env |
| determinism flags requested | small extension | must be paired with the probe below |
| **measured determinism probe** | **new, and required** | a canary op run twice at capture; without it the flags are a false assurance on Metal and a true one on CUDA, and sections 2 and 3 show nothing at the API level tells those apart |
| **backend identity** | **new, and required** | promoted to required by section 3: the same flag is load bearing on CUDA and cosmetic on MPS, so the probe result cannot be interpreted without knowing the backend |
| per-device RNG streams | new capture | `torch.get_rng_state` and the MPS equivalent, captured at the anchor |
| data loader order | new capture | either record the sampler seed, or record the emitted index sequence |
| backend-specific determinism env | new capture | `CUBLAS_WORKSPACE_CONFIG` changes cuBLAS reduction behaviour, so a replay under a different value is a replay under a different contract |
| checkpoint anchor reference | **shipped** | Class C anchoring landed: `process-checkpoint` is a registered gate, 7 of 7 cases |

The genuinely new pieces are the **measured determinism probe**, the **backend
identity** it must be read against, and the **data loader order**. Everything
else is an extension of fields the envelope already has.

Section 3 is why the probe is not optional. A capsule that recorded only the
requested flag would be correct on CUDA and lying on Metal, while the two are
indistinguishable through the API: both accept the call, both report `True`.

## 5. Kernel and privileged software: the honest verdict

**No, this architecture cannot reproduce a Linux kernel bug, and no amount of
extending it will change that.**

The reason is structural rather than incidental. Both boundaries this project
records at, the libc symbol boundary and seccomp user notification, sit
*above* the kernel and observe what userspace asks of it. Reproducing a kernel
defect requires recording the kernel's own inputs: device I/O and DMA,
interrupts, timer ticks, and the behaviour of other CPUs. A recorder must
therefore sit *below* the kernel.

Closest existing art, for reference:
- **QEMU record/replay** with instruction counting, which records a whole VM's
  nondeterministic inputs and replays them deterministically.
- **PANDA**, built on QEMU, whole-system record and replay used for kernel and
  malware analysis.
- **A determinizing hypervisor**, the Antithesis approach, which runs the entire
  system under a scheduler it owns.
- **rr** is explicitly *not* in this list: it is userspace only, for the same
  structural reason described above.

**The defensible partial yes:** ReproIt can reproduce the *userspace trigger*,
the exact syscall and input sequence that provokes a kernel defect, which is
frequently what a maintainer actually needs in a bug report. That is a real and
useful claim. It is not the same as replaying kernel execution, and the two
should never be blurred in a demo or in copy.

## What we cannot claim

1. **We do not reproduce races.** We reproduce input conditions, and we can
   control the schedule at the boundaries we hook. For a pure in-memory race
   between threads, measured effect: none (0.0 to 1.0 percent).
2. **Reproduction rates measured under instrumentation are not the program's
   natural rates.** The preload alone moved a 2.0 percent race to 17.0 percent.
3. **A framework's determinism flag is not evidence of determinism.** Measured:
   accepted and enabled on MPS while `scatter_add_` stayed nondeterministic in
   6 of 6 in-process runs.
4. **Nothing here is measured for CUDA**, multi-GPU, NCCL collectives, or cuDNN
   autotuning. No NVIDIA hardware is present on this machine. Any claim about
   those is unmeasured.
5. **Kernel execution is out of reach**, by construction, for any boundary that
   sits above the kernel.
6. The race subject is a deliberately constructed defect. It is representative
   of a real bug class (publish before initialize), but it is not a survey of
   real-world races, and no such survey was performed.

## Integration hooks this work would need

I own only this directory, so the following are described rather than built:

1. **A schedule-fuzzing mode in the process shim.** The prototype here hooks
   `clock_gettime` and `pthread_create`. In the real shim the same perturbation
   belongs at every already-hooked site, gated by an env control, with the seed
   and fire rate recorded into the capsule envelope so a reproduction is
   repeatable. Recording those two values is what makes a fuzzed reproduction
   a capsule rather than a coin flip.
2. **A determinism probe at capture time**, whose result goes in the envelope
   next to the requested flags, per section 2.
3. **A verdict distinction.** A failure reproduced only under schedule
   perturbation is not the same verdict as one reproduced naturally, and the
   capsule should say which, so a green replay never implies the bug is rare
   when it was actually forced.
