# Authoring ReproIt configuration

Treat the agent as a contract authoring assistant, never as an oracle. ReproIt reports a semantic
bug only when an activated contract and bounded runtime evidence produce the same violation on
confirmation.

## Authority ledger

Classify every candidate before editing `reproit.yaml`:

| Class | Meaning | May become active automatically? |
| --- | --- | --- |
| `declared` | Explicit user policy or an app-owned test/assertion | Yes, for the exact rule |
| `derived` | Mechanical fact from code, schema, or runtime | Only if it proves the predicate |
| `suggested` | Model inference, convention, name, or visible copy | No |

Examples:

- A registered `/login` route is a derived route-existence fact. It does not prove redirect policy.
- Middleware that unconditionally redirects authenticated sessions from `/login` to `/app` is a
  derived implementation fact. An application-owned test or explicit user policy is stronger
  authority for the intended behavior.
- "Members probably should not see `/admin`" is suggested policy, even when the names look obvious.
- A closed OpenAPI response schema can mechanically authorize a response-shape contract for that
  operation. It does not authorize business rules absent from the schema.

Keep suggested rules in the response, never in `reproit.yaml`, generated test files, SDK
registrations, or CI. A user can approve one exact suggestion; record it as declared and activate
only that scope.

## Workflow

1. Inspect route registration, authorization middleware, API schemas, SDK registrations, existing
   tests, reset/seed mechanisms, and policy supplied by the user.
2. Build the ledger. Give each row a stable id, class, exact source reference, proposed contract,
   and activation blocker.
3. Detect contradictions. If two authoritative sources disagree, do not choose one silently. Keep
   the affected rule inactive and report the conflict.
4. Prepare the smallest `reproit.yaml` diff containing only activation-ready rows. Do not add
   framework abstractions, duplicate tests, or broad inferred policy.
5. Run `reproit doctor` to validate configuration, runner capabilities, reset behavior, and local
   authority such as stored sessions.
6. Run the narrowest applicable contract family. Use `reproit scan --only route-access` for browser
   document access, for example.
7. Report execution separately from coverage. List `SATISFIED`, `VIOLATION`, `ABSTAIN`, inactive
   suggestions, and unobservable declarations.

Never count an abstention or uncovered cell as a pass.

Use this compact ledger shape in the response before applying semantic configuration:

```text
id | class | source | proposed rule | activation
login-member | declared | tests/auth.spec.ts:42 | member /login -> /app | ready
admin-member | suggested | route name only | member /admin -> denied | needs policy
```

Do not use an LLM judgment, screenshot interpretation, visible copy, or a generated test as the
pass/fail predicate. Generated tests are implementations of already-authorized rules, not new
authority.

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
    authority:
      anonymous: declared
      member: declared
    access:
      anonymous: allow
      member: { redirect: /app }
  - route: /app
    authority:
      anonymous: declared
      member: declared
    access:
      anonymous: { redirect: /login }
      member: allow
```

Only declare concrete same-origin paths without queries or fragments. Principals must be
`anonymous` or exact `auth.accounts` names. Valid outcomes are `allow`, an exact
`{ redirect: /path }`, or an exact `{ status: 403 }`.

Record authority per route/principal cell. `suggested` is a safe non-executable state: ReproIt
returns `ABSTAIN` before starting a browser context. Keep suggestions out of committed config even
though this runtime guard exists. Omitted authority remains `declared` for existing configurations.

Every named principal needs `validate.text`, `validate.state`, or `validate.route`. Prefer a stable
route or structural state over visible text. Run:

```sh
reproit scan --only route-access
```

Through ReproIt's MCP server, call `reproit_scan` with `{"only":"route-access"}`.

Use the backend `authorization-matrix` proof for API operations or protected response data. Do not
duplicate API policy as browser policy unless both surfaces independently own that contract.

## Completion report

End configuration work with:

- Files changed and the authority source for every activated semantic rule.
- Commands run and their exact tri-state totals.
- Remaining suggested rules that require product policy.
- Contracts that abstained because evidence, authentication, reset, or platform capability was
  unavailable.
- Important behavior that remains uncovered.

The agent may improve configuration after an abstention, but it must not weaken a predicate, add a
retry that hides instability, or reinterpret missing evidence as success.
