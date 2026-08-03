# Start here

Signup to a production failure you can run on your laptop. Five steps, one path.

## 1. Create a workspace

Sign up at [cloud.reproit.com](https://cloud.reproit.com). There are no passwords: sign-in is a
single-use link sent to your mailbox, or your identity provider. Your workspace gets its own
database on creation.

## 2. Get your keys

A project is created with two keys:

- **`pk_live_...`, publishable.** Write-only, and accepted only on ingest. This is the one that
  ships inside your application.
- **`sk_live_...`, secret.** Full management access to `/v1/*`. Server-side and CI only.

Both are shown once. Store them where you keep other secrets.

## 3. Add the SDK

One line plus init. The dashboard shows the exact snippet for your platform with your ids already
filled in; [SDKs](sdks.md) has the same per platform. Web, for example:

```js
import './vendor/reproit-web.js';

ReproIt.start({
  appId: 'your-app',
  key: 'pk_live_...',
  endpoint: 'https://cloud.reproit.com/v1/events',
  build: { version: '1.4.2', commit: 'abc123' },
});
```

Set `build` from your real version and commit. It is what ties an occurrence back to the code that
produced it.

## 4. Ship it, and wait for a real failure

Cloud groups equivalent occurrences into one bug, ranked by impact. Ingest rejects error events
that carry no well-formed oracle id, so what lands on the list is typed evidence rather than raw
telemetry.

## 5. Reproduce it locally

```sh
reproit login
reproit list --state bugs     # ranked, with occurrence ids
reproit occ_8f3a2c91          # re-execute that failure here
```

Fix the bug, run the same command again to confirm, then keep it so it cannot come back:

```sh
reproit keep occ_8f3a2c91
reproit check                 # every kept repro, in CI
```

That last step is the point of the whole loop. See [ReproIt in CI](../ci.md).
