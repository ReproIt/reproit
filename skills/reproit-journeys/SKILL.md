---
name: reproit-journeys
description: >-
  Use when authoring a scripted or multi-user test journey for reproit: a
  declarative path through the app (single actor or several actors on separate
  devices, with logins, secrets, and assertions). Trigger on "write a test for
  this flow", "two users interacting", "multi-user / multi-actor test",
  "presence/chat/handoff test", or editing a journey YAML.
---

# Authoring a reproit journey

A journey is a declarative path: a `setup` (usually a login) plus ordered `steps`. Running a journey
is `reproit journey <name>`; the `reproit journey` command manages the files. Unlike `fuzz` (which
explores), a journey asserts a **specific** scripted flow, including flows that need more than one
user.

Start from `templates/journey.yaml` and adapt it. Keep the journey at the level of user intent, not
pixels.

## Single-actor journey

- `setup`: how the run begins, e.g. `login(guest)` or a named account.
- `steps`: each is an action (`do`) and optionally an assertion (`assert`).

Selectors resolve against **visible** elements only:

- `tap:key:testid:<id>` , `tap:key:<key>` , `tap:role:<role>#<index>`
- `type:<text>` into the focused field
- `assert:textPresent:<text>` , `assert:count:<selector>:<n>`
- `auth:<account>` to run that account's login prelude mid-journey

## Multi-actor journey (the distinctive part)

Two or more users on **separate devices**, coordinated so they cannot collide.

- `actors`: declare them as a list `[alice, bob]` or a map binding each to a login:
  `{alice: {login: alice}, bob: {login: bob}}`.
- Each actor gets its own login prelude and its own device. reproit assigns roles atomically and
  join-barriers them, so both are ready before the interaction starts. If a device is missing you
  get a clear error, you do not silently run both actors on one device.
- In `scenarios`, each step names the **speaker** (which actor acts). A step blocks at the barrier
  until its turn, so "alice posts, then bob sees it" is expressed directly and stays deterministic.

## Secrets, never hardcode

- Create account-backed creds with `reproit auth add <account> --strategy <kind>`.
- Reference them in a step as `secret:<KEY>` (e.g. `type:secret:ALICE_PW`). reproit expands the
  placeholder just before delivery and redacts the value from all captured logs. Runners never see
  raw secrets.
- TOTP/2FA: store the base32 seed; reproit generates the code at run time.

## Accounts and reset

- Named accounts (with optional `user_id`) live in config under `auth.accounts`.
- Reset steps can template account fields: `${account.alice.userId}`, `${account.alice.username}`.
  Use a reset prelude to put accounts in a known state before a multi-user run (e.g. clear
  alice/bob's prior data).

## Workflow

1. Write/adapt the YAML from the template.
2. Save/validate it via `reproit journey` (manages the files).
3. Run it: `reproit journey <name>`. Exit `0` pass, `1` fail, `2` flaky.
4. A flaky multi-user journey usually means a missing barrier/ordering, make the cross-actor
   dependency explicit rather than adding waits.
