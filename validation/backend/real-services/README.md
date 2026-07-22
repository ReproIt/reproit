# Deterministic real-service backend gate

This versioned gate runs actual HTTP implementations rather than schema-derived
loopback mocks. Three small services implement the same externally observable
contract using Node.js, Python's native HTTP server, and Go's native HTTP server.
Each service is exercised in three modes:

- `clean`: exact codec projection, coherent conditional cache, valid JSON media,
  and correctly ordered lifecycle evidence;
- `broken`: precision loss, a strong ETag reused for different bytes, invalid
  JSON under `application/json`, and a callback after scope closure;
- `incomplete`: missing codec output, mismatched `Vary` input, absent/bodyless
  media evidence, and an incomplete lifecycle trace. Every oracle must abstain.

The harness captures response chunks and headers after Node's HTTP/1.1 parser.
Those values are sufficient for the current cache, media-type, codec, and
lifecycle fixtures, but they are not raw framing evidence. In particular, this
harness cannot prove whether a HEAD, 1xx, 204, or 304 response carried forbidden
wire content. See [FORBIDDEN-CONTENT-AUDIT.md](FORBIDDEN-CONTENT-AUDIT.md).

The Rust validator calls ReproIt's public backend oracle API. A passing run
therefore means the clean and incomplete modes produced no violations and the
broken mode produced all four exact oracle identities. It does not establish
framework-wide or production compatibility.

Run locally:

```sh
node validation/backend/real-services/run.mjs
```

Use `REPROIT_SERVICES=node` where only Node is installed. Artifacts are written
under `artifacts/<host>/` and intentionally ignored because they include host
and toolchain-specific transport details.

Run the gate independently on each required architecture and retain each
`artifacts/<host>/` directory as native evidence.

Only the Node service is required on Windows. Run with
`REPROIT_SERVICES=node` and report missing Node/Rust as host capability failures,
not ReproIt failures.
