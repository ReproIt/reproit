# Experimental backend structural validation

This gate is intentionally separate from the public product documentation.
It validates the language-independent backend event protocol, schema importers,
function and effect graph, authoritative oracle boundary, frozen identities,
and exact replay integration.

Backend adapters emit one JSON object per line. Verdict evaluation consumes the
object after the `REPROIT:BACKEND ` marker in the runner log so it remains
attributed to the exact fuzz seed. A `backend-*.jsonl` file in the run directory
is also retained in the encrypted capsule as supporting evidence.

For web applications, enabling backend contracts makes the runner add
`x-reproit-trace`, `x-reproit-actor`, and `x-reproit-action` to first-party API
requests. Instrumented services return a base64url JSON event array in
`x-reproit-events`. The runner accepts only events bound to the exact request
trace. `sdk-node.mjs` is the zero-dependency reference adapter used by this gate.
It remains internal until the backend product passes the full release gate.
Sibling API hosts must be listed in `backend.origins`; no correlation header is
sent to any other origin. Those API hosts must allow the three request headers
through their CORS policy. The response evidence header is read by the browser
driver, so application JavaScript does not need access to it.

The minimum invocation is:

```json
{"sequence":1,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"start","input":{"body":"hello"}}
{"sequence":2,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"effect","effect":"write","resource":"messages","key":"m1","effectTenant":"team-a","after":{"id":"m1"}}
{"sequence":3,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"return","status":201,"success":true,"effectsComplete":true,"output":{"id":"m1"}}
```

`effectsComplete` may be true only when the adapter observed every effect in
the operation. Missing-effect contracts are silent without that proof. Positive
evidence such as a duplicate write or cross-tenant write remains actionable.

Run `./run.sh` on macOS. Run `./run-linux-docker.sh` for a clean Linux x86
gate. Run `run.ps1` in PowerShell on Windows. `run-windows-remote.sh` transfers
the source gate through the configured lab hosts and runs it on native Windows.
