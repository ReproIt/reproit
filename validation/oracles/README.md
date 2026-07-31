# Per-runner oracle coverage ledger

`coverage.json` states, for every UI oracle marker the Rust map parser reads and
every runner that could emit it, one of three claims:

1. `evaluated`, with the source file that emits the marker;
2. `unavailable`, with the reason the platform cannot express the oracle; or
3. `unimplemented`, meaning it could be written on this tier and has not been.

The ledger exists because silence is ambiguous. A runner that never emits an
oracle's marker produces no finding for it, and a report with no finding is
indistinguishable from a checked, clean result. Prose in `docs/oracles.md` used
to be the only record, and it was wrong in both directions: it claimed `jank` on
the Flutter explorer, which emits no jank marker at all, and claimed
`broken-asset` and `blank-screen` for web only when six to nine runners emit
them.

`check.py` proves every claim against the runner sources instead of trusting the
ledger, and fails closed: a marker the parser learns to read with no row, a row
for a marker nobody parses, a runner stated twice or not at all, an `unavailable`
claim with no reason, or an `evaluated` claim whose runner emits nothing.

Run:

```sh
python3 validation/oracles/check.py
python3 validation/oracles/check.py --check-generated
python3 -m unittest discover -s validation/oracles -p 'test_*.py'
```

`--write` regenerates the coverage table inside `docs/oracles.md`; the doc is
generated from the ledger so it cannot drift again.
