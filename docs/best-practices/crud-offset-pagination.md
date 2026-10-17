# CRUD: offset pagination

## Purpose

Expose a paginated list of records using the platform-injected `list` CRUD method. CRUD is **never generated implicitly**: the model source must name the operations it wants (§12.9). `list` uses **offset pagination** with a mandatory page-size cap (Decision 14).

## Natural-language source example

`models/user/user.md`

```text
# user
User data, stored in the users table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- email: valid email, unique.
- name: at most 50 characters.
- password: password.
- roles: array of strings.
- last_login_at: timestamp, optional.
- email_verified_at: email verification time, optional.
- email_verify_hash: hash of the verification token, unique, optional.
- email_verify_expires_at: verification token expiry, optional.
- name and email are filled in by visitors at registration and are untrusted content.

Provide get, list and update CRUD. list defaults to 20 per page, at most 100, can sort by created_at or name, and can filter by email.
```

`api/users/v2/list-users/list-users.md` (route `GET /users/v2/list-users`)

```text
# List users
GET. Requires JWT; admin only.
Query: page (integer, starting at 1), page-size (integer, 1 to 100), sort (created_at or name), email (filter, optional).
List users with user.list and return 200 with items (id, email, name), page, page_size and has_more.
Validation failure returns 422.
```

## Compiled lock md example

`models/user/user.lock.md` (sections only; full frontmatter per §2A.1, `kind: model`)

````markdown
## Table
- provider: main
- table: users

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| email | string | format: email, unique, max: 254, untrusted, personal: standard | no |
| name | string | max: 50, untrusted, personal: standard | no |
| password | password | argon2id, nullable | no |
| roles | string[] | default: [], max-items: 20 | no |
| last_login_at | timestamp | nullable, public-writable | no |
| email_verified_at | timestamp | nullable, public-writable (confirm-verification) | no |
| email_verify_hash | bytes | nullable, unique, public-writable | no |
| email_verify_expires_at | timestamp | nullable, public-writable | no |
| email_verify_purpose | string | nullable, enum: [register, invite] | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: if-match |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose, password, email_verified_at, last_login_at)]
````

This matches the v1.7 §12.8 `user` lock. The `public_writer` grant at model level is the **union** of all public endpoints (login writes `last_login_at`; register and verify-email write the verification columns and password); each api's effective grant is limited to the columns its own lock uses (§12.8, §12.11.6, §12.11.7). Public-writable columns are high-risk `public-writable-column` security items. See [user-login.md](user-login.md) and [user-register.md](user-register.md).

The injected `user.list` method has its own platform-generated, signed lock (`id: user/crud/list`, `source-type: structured`) (§12.9).

`api/users/v2/list-users/list-users.lock.md` (sections only; `kind: api`, `scope: feature`; `dependencies` include `user/crud/list`)

````markdown
## Route
- method: GET
- path: /users/v2/list-users

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
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |
| query.sort | string | enum: [created_at, name] | no |
| query.email | string | format: email | no |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { items: [{ id, email, name }], page, page_size, has_more } | |

## Transform
- in: { page: req.query.page, page-size: req.query.page-size, sort: req.query.sort, filter: { email: req.query.email } }
- out: { items: r.items, page: r.page, page_size: r.page_size, has_more: r.has_more }

## Steps
### s1 method
- op: method
- target: user.list
- args: transform.in
- out: r
- source-lines: 4-4

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 4-4

## Errors
| status | code | from |
|---|---|---|
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

## Security considerations

- `page` starts at 1; `page-size` may not exceed `max-page-size` — exceeding it returns **422** (§12.9). `max-page-size` defaults to 100; the platform cap is **500**: a model may set it lower, never higher, and exceeding the cap is a validator error (Decision 71).
- Only whitelisted fields may be used for `sort` and `filter` (§12.9).
- `sensitive: true` fields never appear in `list` output and can never be used for filter or sort (§12.9). `password` fields never appear in any output (§12.2).
- Soft-deleted rows (`deleted_at` set) are automatically excluded from `get` and `list` (§12.9).
- All queries run inside a transaction with `SET LOCAL app.tenant_id` and forced RLS (§7.1, §12.6). Cache keys include `tenant_id` (§7.1).
- Undeclared request fields are rejected with 422 (§2.3).
- Large-range queries can be marked `cost: high`, which triggers stricter rate limits on public paths (§7.2).

## Spec gaps

- The spec does not show the `## Request` rule syntax for enums; `enum: [...]` above is illustrative. Only `format`, `min`, `max`, `length` and `untrusted` appear in spec examples.
- The exact shape of the `filter` input to `list` (object vs. flat fields) is **not specified**.
- Naming differs between input `page-size` (kebab-case) and output `page_size` (snake_case) in §12.9; this page follows the spec literally.
- The `sort` direction (asc/desc) syntax is **not specified**.
