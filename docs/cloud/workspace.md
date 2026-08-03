# Team, retention, and deletion

## Members and roles

Invite by email from the dashboard. An invitation is a single-use link; there are no passwords to
provision. Roles are `owner`, `admin`, and member: owners and admins can manage members and
project settings, and only an owner can transfer or remove another owner.

Administrative actions are recorded in audit history.

## Organizations

A workspace can hold several projects. Each workspace maps to its own database and its own artifact
namespace, so isolation is structural rather than a filter in a query.

## Retention

Evidence is retained for your plan's retention window and then deleted automatically. Retention is
a property of the workspace, not something each SDK has to be configured for.

## Export

`GET /v1/apps/{app}/export` returns a portability export: bugs, occurrences, evidence metadata, and
immutable file keys. It is available before deletion and does not require a support request.

## Deletion

Deleting a project removes its object keys. Deleting a workspace deletes its database, its stored
evidence, its keys, and its members. Deletion is not a soft flag.

You can also ask for export or erasure by email at any time; see
[the privacy policy](https://reproit.com/privacy) for the statutory rights process.
