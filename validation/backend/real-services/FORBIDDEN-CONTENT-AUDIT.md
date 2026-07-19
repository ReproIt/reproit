# HTTP forbidden-content trial audit

Audited: 2026-07-19

## Result

ReproIt does not currently have authoritative transport evidence for the
proposed forbidden-content oracle. No new finding was added.

The existing `HttpExchangeEvidence.responseBody` model can hold exact bytes
from an external adapter, and the conditional-cache validator can flag a
non-empty 304 body when such evidence is supplied. That is a pure validator
test, not proof that the built-in transport can make the observation.

The real-service harness uses Node's `node:http` client and records chunks from
the parsed `IncomingMessage` stream. It does not capture HTTP framing bytes:

- every request in the harness is GET, so HEAD is not exercised;
- the response callback represents the final response, so the harness does not
  retain the complete sequence and boundaries of informational responses;
- 204 and 304 bodies are observed only after Node's HTTP parser applies response
  semantics;
- the harness uses only `node:http`, so it has no HTTP/2 frame capture;
- `HttpExchangeEvidence` records neither HTTP version nor whether capture was
  complete at the raw transport boundary.

Consequently, an empty `responseBody` can mean either that the server emitted no
content or that the client parser suppressed forbidden content. Treating it as
proof would create false SATISFIED results. The current `reqwest` headless
transport has the same authority problem because it exposes a decoded response
body, not HTTP/1.1 framing octets or HTTP/2 DATA frames.

## Coverage matrix

| Case | HTTP/1.1 | HTTP/2 | Current outcome |
| --- | --- | --- | --- |
| HEAD | No raw response framing | No stream-frame capture | ABSTAIN |
| 1xx | No complete interim-response capture | No stream-frame capture | ABSTAIN |
| 204 | Body exposed only after parser semantics | No stream-frame capture | ABSTAIN |
| 304 | Pure validator accepts supplied bytes, live capture is post-parser | No stream-frame capture | ABSTAIN |

The existing conditional-cache oracle remains valid for a violation only when
an adapter supplies a non-empty exact 304 response body. The checked-in
real-service broken fixture instead proves a different case: a 200 response
reuses one strong ETag for different exact decoded body bytes.

## Required evidence contract

A future implementation can report VIOLATION or SATISFIED only when all of the
following are present:

1. The HTTP version is explicit and supported.
2. Capture is complete for the response or HTTP/2 stream.
3. HTTP/1.1 evidence preserves framing boundaries, or HTTP/2 evidence preserves
   HEADERS and DATA frame type, stream id, order, flags, and payload length.
4. The response is attributed to the directly connected endpoint. If an
   intermediary may have transformed it, the finding must name the observed
   endpoint rather than the application framework.
5. The request method and every interim and final status are retained.

With that authority, the exact outcome boundary is:

- VIOLATION: forbidden HTTP/1.1 content octets or HTTP/2 DATA payload bytes are
  attributed to the relevant response.
- SATISFIED: a complete supported-protocol capture proves zero forbidden bytes
  or DATA payload bytes.
- ABSTAIN: the protocol is unsupported, capture is decoded or incomplete,
  framing is ambiguous, an interim boundary is missing, or attribution is
  unavailable.

The implementation must not mistake a permitted HEAD or 304 `Content-Length`
field for response content. HTTP/1.1 status 101 also requires an explicit
upgrade boundary so subsequent protocol bytes are not attributed to the HTTP
response. HTTP/3 remains outside this proposed trial and must abstain.
