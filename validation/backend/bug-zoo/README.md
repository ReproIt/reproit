# Backend BugZoo

This opt-in gate tests Reproit's current backend evidence against real historical
bugs, not synthetic mutations. Each entry must make a real request at pinned
buggy and fixed revisions, retain the authoritative schema plus response or
rejection outcome, pass that evidence through Reproit's importer/evaluator,
and prove:

- the buggy revision produces exactly the expected finding;
- the fixed revision produces zero findings;
- the upstream issue/fix describes the same observable contract violation.

## FastAPI PR #889 / BugsInPy FastAPI bug 5

FastAPI [PR #889](https://github.com/fastapi/fastapi/pull/889), related to
[issue #800](https://github.com/fastapi/fastapi/issues/800) and
[issue #847](https://github.com/fastapi/fastapi/issues/847), fixed nested
`response_model` filtering. The upstream regression test proves that a nested
`ModelC(username, password)` returned through a declared `ModelB(username)`
response leaked `password` before the fix and removed it afterwards.

The same revision pair is curated as
[BugsInPy FastAPI bug 5](https://github.com/soarsmu/BugsInPy/tree/master/projects/fastapi/bugs/5):

- buggy: `7cea84b74ca3106a7f861b774e9d215e5228728f`
- fixed: `75a07f24bf01a31225ee687f3e2b3fc1981b67ab`

The gate executes `/model` through FastAPI's real `TestClient` on Python 3.8.
The buggy response contains `model_b.password`; the fixed response does not.
Reproit's declared closed response contract reports exactly one
`response-shape` finding for the buggy revision and none for the fix.

The generated OpenAPI schema alone cannot detect this bug: OpenAPI/JSON Schema
objects permit additional properties unless explicitly closed, and FastAPI did
not emit `additionalProperties: false`. The gate asserts that schema-only mode
stays silent instead of manufacturing authority. This distinction is part of
the release contract, not a skipped test.

## FastAPI issue #2719 / PR #2725

FastAPI [issue #2719](https://github.com/fastapi/fastapi/issues/2719)
reported that a route declaring a required object `response_model` could return
JSON `null` with status 200. The maintainers confirmed the bug and merged
[PR #2725](https://github.com/fastapi/fastapi/pull/2725), released in FastAPI
0.80.0.

The gate uses the fix commit's immediate parent as the buggy revision and the
one-parent fix commit as the fixed revision:

- buggy: `12f60cac7a2262231374404c538f0b227f9b9496`
- fixed: `634cf22584fc4fd9ee53cfdf0ad6d48a2830ac34`

Both revisions emit the same OpenAPI response schema. The buggy revision's
successful `null` produces exactly one `response-shape` finding directly from
the imported OpenAPI contract. The fixed framework rejects the invalid return
before it can become a successful HTTP response.

## Run

Requirements: Docker with `linux/amd64` execution support, Git, Rust/Cargo, and
network access for the pinned source and Python packages. ARM Linux hosts need
amd64/binfmt emulation; Docker Desktop supplies it by default.

```sh
./validation/backend/bug-zoo/run.sh
```

The Python image digest, direct Python dependency versions, Cargo lockfile, and
source revisions are pinned. Transitive Python dependencies remain constrained
by those direct versions rather than independently hash pinned.
Repositories and response evidence live only in a temporary directory and are
removed on exit. Add entries only when current structural evidence can detect
the exact upstream observable; ordinary unit-test failures are insufficient.
