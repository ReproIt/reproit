# Repro It Cloud

Cloud groups production failures into stable bugs and hands out occurrence ids that reproduce on
your machine:

```sh
reproit occ_8f3a2c91    # the production failure, re-executed locally
```

It never executes your application and never holds your source. Reproduction runs in your CI or on
your machine; Cloud orchestrates and stores the result.

- [Start here](start.md) — signup to your first reproduced bug
- [SDKs](sdks.md) — install and init, per platform
- [The dashboard](dashboard.md) — bugs, priority, resolution states
- [Reproduction and CI](reproduction.md) — how a bug becomes a run
- [API and agents](api.md) — keys, `/v1/work`, and the bucket routes
- [Team, retention, and deletion](workspace.md)
- [Security and data handling](security.md)
