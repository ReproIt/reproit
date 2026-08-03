# The dashboard

One list of bugs, ranked. The ranking is the product: it decides what you work on next.

## Ranking

A bug's place in the list is your priority when you set one, and the impact score otherwise. So the
default order is evidence-driven, and overriding it is an explicit act rather than the only way to
get a useful list. Pin a bug to hold it at the top regardless.

## What a bug is

Equivalent occurrences group into one bug by structural identity, not by fuzzy message matching.
The same defect reached by different paths, on different builds, in different languages, is one
row. Each row carries its occurrences, the builds they came from, stored evidence, and the
reproduction history.

## Lifecycle

Triage status is yours to set: `untriaged`, `investigating`, `wontfix`.

**`fixed` is not.** It is machine-owned, and a human cannot post it. A bug becomes fixed only from
a verdict produced by re-executing it, so "fixed" on this dashboard always means something was run
and did not reproduce, never that somebody believed it was done.

Resolution state, shown alongside, is the production view of the same question:

| state | meaning |
| --- | --- |
| `active` | occurring |
| `resolving` | a fix is in, production has not confirmed it yet |
| `resolved` | production agrees the fix holds |
| `regressed` | the bug recurred on a build at or after the fix |

A regression is judged against build order, so occurrences from builds older than the fix do not
count against it. That distinction is what keeps a stale rollout from looking like a failed fix.

## Evidence

Each bug carries what was captured: structural signatures, the action sequence, the typed finding
identity, and any confirmed reproduction results uploaded from a local or CI run. What is never
there is your users' input values or their pixels; see [security](security.md).

## Issue trackers

GitHub, Jira, Linear, and Shortcut. A bug can open a ticket carrying its evidence and its
occurrence id, so the person who picks it up starts from a command that reproduces it rather than
from a description.
