# CRUD: soft delete

## Purpose

Delete records without destroying data. In this platform **every delete is a soft delete**: the injected `delete` CRUD method sets `deleted_at`, and soft-deleted rows are automatically excluded from `get` and `list` (§12.9, Decision 13). There is no hard delete, no purge, and no retention-based expiry.

## Natural-language source example

`models/note/note.md`

```
# note
User notes, stored in the notes table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- title: 1 to 200 characters.
- body: at most 10000 characters.

Provide all CRUD. list defaults to 20 per page, at most 100, can sort by created_at.
```

`api/notes/v1/delete-note/delete-note.md` (route `DELETE /notes/v1/{id}`)

```
# Delete note
DELETE, path /notes/v1/{id}, id is a uuid. Requires JWT; only admin may use it.
Delete the note with note.delete and return 204.
Note not found returns 404 NOT_FOUND.
```

## Compiled lock md example

`models/note/note.lock.md` (sections only; `kind: model`)

````markdown
## Table
- provider: main
- table: notes

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| title | string | min: 1, max: 200 | no |
| body | string | max: 10000 | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| create | |
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at], filter: [] |
| update | partial: true, optimistic-lock: updated_at |
| delete | soft |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT]
````

Note the grants: no runtime role ever has `DELETE` (§7.1).

`api/notes/v1/delete-note/delete-note.lock.md` (sections only; `kind: api`, `scope: feature`; `dependencies` include `note/crud/delete`)

````markdown
## Route
- method: DELETE
- path: /notes/v1/{id}

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

## Response
| status | schema | headers |
|---|---|---|
| 204 | (none) | |

## Transform
- in: { id: req.path.id }
- out: (none)

## Steps
### s1 method
- op: method
- target: note.delete
- args: transform.in
- out: (none)
- source-lines: 3-3

### s2 respond
- op: respond
- status: 204
- source-lines: 3-3

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | note.delete |
````

If you write a custom deletion method, it must be an `UPDATE`, for example:

````markdown
### s1 query
- op: query
- provider: main
- table: notes
- sql: `update notes set deleted_at = now() where id = $1 and deleted_at is null`
- params: [input.id]
- writes: true
- scope: pk
- columns: [deleted_at]
- out: (none)
- source-lines: 3-3
````

## Security considerations

- **Same-tenant IDOR**: RLS isolates tenants only. Without a role/ownership check any authenticated tenant user could delete any note by id. This example uses `roles: [admin]` (high-risk `roles` item). The spec defines **no row-ownership mechanism** for notes owned by a user, see Spec gaps.

- `DELETE` SQL must never appear in any lock; DB roles have no `DELETE`/`TRUNCATE` privilege (§2A.4, §7.1, §10.8). There is no `soft: false` option (§12.9).
- `deleted_at` is a system field: it cannot be written from request input (§12.9) and is on the public-write blacklist (§7.2).
- Soft-deleted rows are excluded automatically by injected `get`/`list`. **Custom methods must exclude them themselves** (e.g. `deleted_at is null`, as `user.login` does in §12.8).
- Soft-deleted data is retained forever. For legal erasure requests use [privacy deletion](privacy-deletion.md) (crypto-shredding + anonymization), not DELETE (§12.9, §12.10).
- Every `UPDATE` needs `WHERE` with `scope: pk | rls` (§2A.4).

## Spec gaps

- The `## CRUD` option syntax for `create` and `delete` is **not shown** in any spec lock example (only get/list/update in §12.8). `delete | soft` and an empty `create` row above are illustrative.
- The output and error set of the injected `delete` (e.g. whether it fails `NOT_FOUND` for an already-deleted id) is **not specified**.
- **Row ownership** ("users may delete only their own notes") has no specified CRUD option or lock syntax; injected `delete` has no owner check. `owner`/`owner_id`/`created_by` appear on the public-write blacklist (§7.2) but no general owner semantics exist.
- Whether there is an "undelete/restore" path is **not specified**.
- Whether `respond` with 204 takes `value` is not shown; omitted here.
