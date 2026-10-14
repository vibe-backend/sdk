# CRUD: optimistic locking (`ETag` / `If-Match`)

## Purpose

Prevent lost updates when two clients edit the same record concurrently. In v1.7 the injected `update` CRUD method **always** performs an optimistic-lock check using HTTP conditional requests (Decision 48, §12.9):

- `get` and `update` responses carry a strong `ETag` computed from `id` + `updated_at`.
- Every api that calls `update` must require an `If-Match` header; its `## Transform` maps `req.header.if-match` to the method input `if_match` (the only allowed source).
- Missing `If-Match` → **428** `PRECONDITION_REQUIRED`; non-matching → **412** `PRECONDITION_FAILED` (§8, RFC 6585 / RFC 9110).
- `updated_at` stays a system field and is never written from input.

This replaces the v1.5 pattern (client sends `updated_at`, conflict → 409), which v1.7 fixed as inconsistent (F4).

## Natural-language source example

`models/user/user.md` (relevant line)

```text
Provide get, list and update CRUD.
```

`api/users/v2/update-user/update-user.md` (route `PUT /users/v2/{id}`)

```text
# Update user
PUT, path /users/v2/{id}, id is a uuid. Requires JWT; only admin may use it.
The If-Match header is required and must carry the ETag the client last read.
Body: name (1 to 50 characters, optional).
Update the user with user.update and return 200 with id and name, plus the new ETag.
User not found returns 404 NOT_FOUND. Missing If-Match returns 428 PRECONDITION_REQUIRED; a stale ETag returns 412 PRECONDITION_FAILED.
```

## Compiled lock md example

`models/user/user.lock.md` (CRUD section only, as in §12.8)

````markdown
## CRUD
| method | options |
| --- | --- |
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: if-match |
````

`api/users/v2/update-user/update-user.lock.md` (sections only; `kind: api`, `scope: feature`; `dependencies` include `user/crud/update`)

````markdown
## Route
- method: PUT
- path: /users/v2/{id}

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.id | uuid | | yes |
| header.if-match | string | | yes |
| body.name | string | min: 1, max: 50 | no |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { id, name } | ETag |

## Transform
- in: { id: req.path.id, name: req.body.name, if_match: req.header.if-match }
- out: { id: u.id, name: u.name }

## Steps
### s1 method
- op: method
- target: user.update
- args: transform.in
- out: u
- source-lines: 5-5

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 5-5

## Errors
| status | code | from |
| --- | --- | --- |
| 404 | NOT_FOUND | user.update |
| 412 | PRECONDITION_FAILED | user.update |
| 428 | PRECONDITION_REQUIRED | user.update |
| 422 | VALIDATION_FAILED | platform |
````

## Security considerations

- **Same-tenant access**: RLS only isolates tenants. This example restricts the endpoint to `roles: [admin]` (high-risk `roles` item). For self-service edits, declare `owner-key` on the model (for `users`, the table's own `id`): non-admin read/update SQL must then include `{owner-key} = ctx.user.id`; relaxing that (`ownership: tenant-wide` or `ownership: role(admin)`) must be written in source and is high risk (Decision 70, §12.6a).
- **PUT with partial semantics**: injected `update` has PATCH semantics (§12.9), but v1 routes only GET/POST/PUT/DELETE (§0.1), so PUT carries a partial update. Document this for clients.
- `if_match` may only be mapped from `req.header.if-match`; never accept a version from the body or query (Decision 48).
- System fields `id`, `tenant_id`, `created_at`, `updated_at`, `deleted_at`, `erased_at` are never written from input (§12.9).
- Map 412 and 428 in `## Errors`; an unmapped method error becomes 500 `INTERNAL` and raises a warning (§8, §10.9, §12.4).
- Every `UPDATE` needs `WHERE` bounded by primary/unique key or RLS (`scope: pk | rls`) (§2A.4).
- If you override `update` with `models/user/methods/update.md`, you replace the injected optimistic-lock logic and the override is listed in the deploy report (§12.9).

## Spec gaps

- The `## Request` syntax for a header field (`header.if-match`) and the `ETag` entry in `## Response` headers are not shown in any spec lock; they follow the `req.header.if-match` name from Decision 48 but are illustrative.
- Whether 412/428 are raised by the injected `update` method (and so mapped `from: user.update`) or by the platform before the method runs is **not specified**.
- Whether `NOT_FOUND` is a declared error of injected `update` is not stated (it is stated for `get`).
- Whether the `get` api lock must also declare `ETag` in its `## Response` headers is **not specified**.
