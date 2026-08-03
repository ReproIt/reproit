# Capture-to-replay capability ledger

`coverage.json` is the canonical fail-closed ledger for every
`CaptureCapabilityKind` in `reproit-protocol`. It separates four claims that
must not be conflated:

1. capture availability;
2. capture-to-requirement compilation;
3. executable replay provider support.

A capability modeled by the protocol is not described as captured. An authored
command wrapper is not described as a structured provider.

Run:

```sh
python3 validation/capabilities/check.py
python3 -m unittest discover -s validation/capabilities -p 'test_*.py'
```

The validator rejects protocol drift, missing or duplicate claims, unsupported
states, missing evidence, summary drift, and every incomplete claim without a
named blocker. CI runs the validator beside the complete self-dogfood guard
corpus.
