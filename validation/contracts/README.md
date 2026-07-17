# Temporal contract validation

This fixture proves the complete web path. A structural action triggers a bounded eventual contract,
the app violates it, Reproit confirms the same identity on replay, and the run writes normalized
contract evidence.

Run:

```sh
./validation/contracts/run-web.sh
./validation/contracts/run-multi-web.sh
```

The app intentionally renders `Queued` after `Send message`, while the contract requires
`Message delivered` within two observations.

The multi-actor fixture launches independent Alice and Bob browser sessions. Alice sends a message
and Bob must observe it within the contract bound.
