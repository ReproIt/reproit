# macos-ax: why the application campaign could not be executed

The `macos-ax` promotion blocker says no application campaign has been executed,
and separately that the required-CI gate stalls because this host's `syspolicyd`
is wedged. Those were treated as two independent gaps, with the campaign
believed to be executable without the runner. It is not. The same daemon blocks
both, because a campaign has to launch applications.

## What was attempted

Both qualified candidate applications were cloned: Platypus at
`726175ad41da530d7e482f945811b54004530662` and CotEditor at its default branch.
The plan was the established shape: build each of two applications at its
affected and fixed revision, prove the pair discriminates with one run per side
before spending six, then run three affected reproductions and three fixed
controls each.

The first thing built was a two-line Swift program that prints
`AXIsProcessTrusted()`. `swiftc` compiled it in 0.895 seconds. Running it never
returned. It was sampled twice and killed after more than 29 minutes:

```
Call graph:
    858 Thread_168661072: Main Thread   DispatchQueue_<multiple>
      858 _dyld_start  (in dyld) + 0  [0x104a809c0]
```

Zero CPU, nothing executed, stuck before `main`.

## What separates a binary that runs from one that does not

Ten bounded launches, each a deliberately different guess at the cause:

| subject | outcome |
|---|---|
| `swiftc`-built arm64 binary | stalled, killed after 29m |
| `clang`-built arm64 binary, linker ad-hoc signature | stalled at 40s |
| the same file after `codesign -f -s -` | stalled at 40s |
| the same file after trying to strip `com.apple.provenance` | stalled at 30s |
| a `clang`-built binary under `$HOME` rather than `/private/tmp` | stalled at 30s |
| `cp /usr/bin/true ./true-copy` | stalled at 20s |
| `clang -arch x86_64`, to test the Rosetta exec path | stalled at 40s |
| byte-identical copy written by a shell redirect | stalled at 30s |
| byte-identical copy written by `python3` | stalled at 30s |
| `/usr/bin/true`, untouched, in place | exit 0 in 0s |

The sixth row is the decisive one. `true-copy` has the same bytes and therefore
the same code identity as a binary that runs instantly on this host. Only the
file is new. So the assessment that stalls is keyed on the file, not on the
signature, the architecture, the location, or the process that wrote it.

`com.apple.provenance` is on every file in the tree, including the shell scripts
that run fine, so it is not the discriminator either, and `xattr -d` cannot
remove it. `spctl --status` reports assessments enabled, and `syspolicyd` has
been at 98.5 percent CPU for 25 days, which matches the 6 July onset already
recorded in `validation/self-dogfood/not-a-fix/ax-gate-cold-build-bound.json`.

## It also reaches builds, not just launches

`xcodebuild` was run against Platypus anyway. Two real build recipe findings
came out of it before it stopped, and they are recorded so the next attempt does
not rediscover them: the scheme configurations are `Development` and
`Deployment`, not `Release`, and every target pins `CONFIGURATION_TEMP_DIR` to
the empty string, so the build writes to `/platypus_clt.build/...` and fails
until `SYMROOT`, `OBJROOT` and `CONFIGURATION_TEMP_DIR` are all overridden on the
command line.

With those fixed the build compiled and then stopped in the `ScriptExec`
run-script phase. The stalled process is `/bin/sh`, an Apple platform binary,
running a generated script, sampled at 10m24s with the same `_dyld_start`
signature and no children of its own. So the fault is not confined to the
application binaries a campaign produces; it reaches ordinary build script
phases, and cannot be worked around by pre-warming any particular file.

## What this does and does not establish

It establishes that no `macos-ax` field run can be performed on this host until
`syspolicyd` is repaired, which needs root.

It establishes nothing about the nine qualified candidates. None was built, no
accessibility tree was dumped, and no revision pair was tested for
discrimination, so no candidate is disqualified by this and none is confirmed.

It also retracts an assumption rather than confirming it: `AXIsProcessTrusted()`
was not observed during this work, because the probe that answers it is itself a
new Mach-O and did not launch. Accessibility permission on this host is neither
confirmed nor refuted here.
