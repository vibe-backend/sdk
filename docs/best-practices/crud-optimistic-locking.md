# CRUD: optimistic locking

## Purpose

Prevent lost updates when two clients edit the same record concurrently. The injected `update` CRUD method performs partial updates (PATCH semantics) **with an optimistic-lock check on `updated_at`**; on conflict it fails with `CONFLICT` (§12.9).

## Natural-language source example

`models/user/user.md` (relevant line)

```text
Provide get, list and update CRUD. update is a partial update.
```

`api/users/v2/update-user/update-user.md` (route `PUT /users/v2/{id}`)

```text
# Update user
PUT, path /users/v2/{id}, id is a uuid. Requires JWT; only admin may use it.
Body: name (1 to 50 characters, optional) and updated_at (timestamp, required — the version the client last read).
Update the user with user.update and return 200 with id, name and updated_at.
User not found returns 404 NOT_FOUND. If someone else changed the user in the meantime return 409 CONFLICT.
```

## Compiled lock md example

`models/user/user.lock.md` (CRUD section only)

````markdown
## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: updated_at |
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
|---|---|---|---|
| path.id | uuid | | yes |
| body.name | string | min: 1, max: 50 | no |
| body.updated_at | timestamp | | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { id, name, updated_at } | |

## Transform
- in: { id: req.path.id, name: req.body.name, updated_at: req.body.updated_at }
- out: { id: u.id, name: u.name, updated_at: u.updated_at }

## Steps
### s1 method
- op: method
- target: user.update
- args: transform.in
- out: u
- source-lines: 3-3

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 3-3

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | user.update |
| 409 | CONFLICT | user.update |
| 422 | VALIDATION_FAILED | platform |
````

## Security considerations

- **Same-tenant IDOR**: RLS only isolates tenants. Without a role or ownership check, any authenticated user of the tenant could update any user by id. This example restricts the endpoint to `roles: [admin]` (a high-risk `roles` security item). The spec defines **no row-ownership mechanism** (e.g. "only the owner may update"), see Spec gaps.
- **PUT with partial semantics**: the injected `update` has PATCH semantics (§12.9), but v1 only supports GET/POST/PUT/DELETE (§0.1), so PUT is used for a partial update. Document this for clients.
- Map `CONFLICT` to **409** in `## Errors`; an unmapped method error becomes 500 `INTERNAL` and raises a warning (§8, §10.9, §12.4).
- System fields `id`, `tenant_id`, `created_at`, `updated_at`, `deleted_at` cannot be written from input (§12.9). The client-supplied `updated_at` is only the expected version for comparison, not a value written.
- `password` fields are hashed automatically on update (§12.9).
- Every `UPDATE` must have a `WHERE` bounded by primary/unique key or RLS (`scope: pk | rls`) (§2A.4).
- Changing writable columns or adding write SQL is a **medium-risk** security item (§2A.5).
- If you override `update` by writing `models/user/methods/update.md`, the override replaces the injected version and is listed in the deploy report; you then own the optimistic-lock logic (§12.9).

## Spec gaps

- The **input field name** carrying the expected version for the optimistic-lock check is **not specified** (§12.9 only says the check uses `updated_at`). Passing `updated_at` is the obvious reading but is an assumption.
- Whether `NOT_FOUND` is a declared error of `update` is not stated (it is stated for `get`). Only `CONFLICT` is specified for `update`.
- The spec's `## CRUD` option `optimistic-lock: updated_at` appears in the §12.8 lock, but §12.9 lists only `partial` as an update option; whether optimistic locking can be disabled is **not specified**.
- **Row ownership** ("a user may only edit their own record", e.g. comparing `ctx.user` to the row) has no specified CRUD option or lock syntax; injected CRUD has no owner check. Use roles, or override `update` with a custom method and get it reviewed.
- The `timestamp` request-field type is listed for model fields (§12.2); its use in `## Request` is not shown in spec examples.
