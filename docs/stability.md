# Reproit 1.x stability surface

Reproit 1.x keeps the smallest complete bug-to-regression workflow stable.
Compatibility applies to the documented flags, exit behavior, JSON fields, and
persisted formats used by this surface on the stable Chromium web target:

- `init`, `doctor`, and `auth`;
- `capture` for application demonstrations and bounded commands;
- `find`, plus the compatible `scan` and `fuzz` phase commands;
- direct `fnd_...`, `rep_...`, `bkt_...`, and `@saved-name` replay;
- direct proof, inspection, playback, and simplification flags;
- `list`, `keep`, and `check`, plus their existing compatibility aliases;
- `login`, `bugs`, `triage`, `timeline`, and `resolution-events`;
- `reproit.yaml`, saved repros, event protocol version 1, and published release
  archives; and
- the web (Chromium) production SDK source API and wire behavior documented
  under `sdk/` for the 1.0 tag.

Patch releases may add optional JSON fields. They do not remove fields, change a
field's meaning, reinterpret an exit code, or broaden a finding predicate.
Unknown fields must continue to be tolerated only where the documented format
allows them.

## Preview and experimental surfaces

The following remain available and exact-commit release-gated, but are outside
the 1.x compatibility promise until their contracts have field evidence from
at least two independent uses:

- Firefox, WebKit, mobile, desktop, terminal, Electron, and Tauri adapters;
- backend contract oracles, runtime capture, and backend `inspect`. The real
  backend release gate executes current-server scan, fuzz, exact replay,
  server-error, authored-invariant, stateful, and proof controls. Legacy
  `reproit-backend-*` packages that remain at 0.0.0 are unpublished and are not
  install claims. The source-neutral Rust, Node, and .NET recorders use the
  universal capture contract documented under `sdk/`;
- specialist oracles selected explicitly with `--only`;
- hidden compatibility commands and internal diagnostic views;
- multi-actor coordination and advanced causal environment reduction; and
- registry package coordinates that are not listed as published in `sdk/README.md`.

Experimental behavior must fail closed, remain explicitly labeled, and cannot
silently promote a candidate into a confirmed regression guard.

`validation/support-manifest.json` is the canonical atomic maturity contract.
`validation/compatibility/check.py` validates it and generates the reviewable
status. Every owned platform gate is release-required. A Stable entry must name
a complete field benchmark with at least two independent applications, three
clean affected runs, three reached-observation fixed controls, exact identity,
and verified minimization. Changing documentation cannot bypass those checks.
