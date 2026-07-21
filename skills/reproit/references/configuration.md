# Authoring ReproIt configuration

Treat the agent as a contract authoring assistant, not an oracle. ReproIt reports a semantic bug
only from a committed declaration and exact runtime evidence.

## Workflow

1. Inspect authoritative sources: route registration, auth middleware, API schemas, existing tests,
   SDK registrations, and product policy supplied by the user.
2. Separate facts from proposed intent. A route table proves that a path exists. It does not prove
   which principal should access it.
3. Prepare the smallest `reproit.yaml` diff. For every semantic declaration, cite its source in the
   response or ask the user to confirm it. Do not silently activate guessed policy.
4. Run `reproit doctor` to validate configuration and local authority such as stored sessions.
5. Run the narrow check, then explain `SATISFIED`, `VIOLATION`, and `ABSTAIN` separately.

Never infer authorization from names such as `admin`, `private`, `owner`, or `member`. Never use an
LLM judgment, screenshot interpretation, or visible copy as the pass/fail predicate. Those inputs
may suggest a contract for review, but they cannot confirm a finding.

## Browser document access

Use `routeAccess` for direct browser navigation policy:

```yaml
auth:
  accounts:
    - name: member
      strategy: session
      storageRef: member-session
      validate: { route: /app }

routeAccess:
  - route: /login
    access:
      anonymous: allow
      member: { redirect: /app }
  - route: /app
    access:
      anonymous: { redirect: /login }
      member: allow
```

Only declare concrete same-origin paths without queries or fragments. Principals must be
`anonymous` or exact `auth.accounts` names. Valid outcomes are `allow`, an exact
`{ redirect: /path }`, or an exact `{ status: 403 }`.

Every named principal needs `validate.text`, `validate.state`, or `validate.route`. Prefer a stable
route or structural state over visible text. Run:

```sh
reproit scan --only route-access
```

Through ReproIt's MCP server, call `reproit_scan` with `{"only":"route-access"}`.

Use the backend `authorization-matrix` proof for API operations or protected response data. Do not
duplicate API policy as browser policy unless both surfaces independently own that contract.
