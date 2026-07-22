# Security policy

## Supported versions

The latest 1.x release receives security fixes. Older minor and patch releases
may be asked to upgrade before a fix is provided.

## Report a vulnerability

Do not open a public issue. Use GitHub's private vulnerability reporting for
the `ReproIt/reproit` repository. Include the affected version, platform,
reproduction steps, impact, and any suggested mitigation. Do not include real
customer credentials, source, captures, or evidence.

We will acknowledge a complete report within three business days, provide a
status update within seven business days, and coordinate disclosure after a fix
or mitigation is available. These are response targets, not a bounty promise.

## Dependency policy

CI audits Rust and production JavaScript dependency graphs. A suppression must
name one advisory and explain why the dependency is unreachable in every
supported artifact. Reachable advisories remain release blockers.
