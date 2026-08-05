# Reproduction and CI

The line worth understanding before anything else: **Cloud never runs your code.** It has no
checkout, no build, and no simulator. A reproduction is dispatched to infrastructure you control,
and only the verdict comes back.

## The two ways a bug gets reproduced

**On your machine.** `reproit occ_...` pulls the occurrence and re-executes it locally. This needs
nothing from CI and is the fast path while you are actually fixing something.

**In your CI.** Requesting a reproduction from the dashboard (or
`POST /v1/apps/{app}/buckets/{bucket}/reproduce`) dispatches to your repository, your workflow runs
the same `reproit` binary a developer runs, and it posts the verdict back to the same bucket. Your
runner, your checkout, your secrets.

For private infrastructure that cannot accept an inbound dispatch, workers pull instead: they claim
work from `/v1/worker/claim`, heartbeat, and post results. Nothing needs to reach into your network.

## What comes back

The same four verdicts the CLI reports, because it is the same binary: reproduced, fixed, diverged
(the code no longer makes the captured calls, named), or inconclusive. A different failure is never
counted as a reproduction. Diverged and inconclusive fail closed.

Verdicts accumulate on the bucket as reproduction history, which is what turns "we think this is
fixed" into a dated record with a build attached.

## Regression sweep

Cloud re-checks resolved bugs and reports status transitions, optionally to a webhook. A bug that
starts occurring again after being marked fixed is a regression, and it says so rather than opening
a new unrelated bug.

## The gate is still local

None of the above replaces `reproit check` in your own pipeline. Cloud tells you which bugs matter;
the committed guard suite is what stops them from coming back, and it runs with no credentials at
all. See [Repro It in CI](../ci.md).
