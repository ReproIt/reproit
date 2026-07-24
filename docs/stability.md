# Reproit 1.x stability surface

Reproit 1.x keeps the smallest complete bug-to-regression workflow stable.
Compatibility applies to the documented flags, exit behavior, JSON fields, and
persisted formats used by this surface on the stable Chromium web target:

- `init`, `doctor`, and `auth`;
- `scan` and `fuzz` with the default authoritative oracle set;
- direct `fnd_...`, `rep_...`, `bkt_...`, and `@saved-name` replay;
- `proof`, `candidates`, `keep`, `repros`, and `check`;
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

The following remain available, but are outside the 1.x compatibility promise
until their contracts have field evidence from at least two independent uses:

- Firefox, WebKit, mobile, desktop, terminal, Electron, and Tauri adapters;
- the entire backend pillar: contract oracles, production capture mode, the
  `reproit-backend-*` SDKs (Rust, Node, Python, Go, Ruby, PHP, Java, .NET, all
  versioned 0.0.0 and not yet published or install-smoked), and backend
  `inspect`. Backend is opt-in and its contracts may change before it is
  promoted to the stable surface;
- specialist oracles selected explicitly with `--only`;
- `debug map` analysis and contract suggestions;
- `baseline`, `screenshots`, and `import maestro`;
- multi-actor coordination and advanced causal environment reduction; and
- registry package coordinates that are not listed as published in `sdk/README.md`.

Experimental behavior must fail closed, remain explicitly labeled, and cannot
silently promote a candidate into a confirmed regression guard.
