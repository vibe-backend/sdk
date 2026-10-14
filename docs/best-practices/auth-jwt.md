# Auth pipeline: JWT

## Purpose

Require a valid JWT on every endpoint of an API definition. `auth` is a built-in pipeline (§4) whose lock is provided and signed by the platform; you only enable it and pass parameters (e.g. `mode`, `roles`). Declare it once in `_definition.md` so it is inherited by every version and feature (§3), and restrict roles per feature where needed.

## Natural-language source example

`api/users/_definition.md`

```
# users
User management API. Allowed providers: main and enterprise; default is main.
All endpoints require JWT authentication.
```

`api/users/v2/create-user/create-user.md` (route `POST /users/v2/create-user`)

```
# Create user
POST. Only admin may use it, at most 30 requests per minute.
The request body must contain email (valid email) and name (1 to 50 characters).
Create the user with user.register; on success return 201 with the new user's id, email and name, plus Location.
If the email is already taken return 409 EMAIL_TAKEN; validation failure returns 422.
```

## Compiled lock md example

`api/users/_definition.lock.md` (sections only; frontmatter per §2A.1 with `kind: api`, `scope: definition`)

````markdown
## Providers
- allowlist: [main, enterprise]
- default: main

## Pipelines
### p1 auth
- mode: jwt

## Defaults
- timeout: 10s
- body: 1MB
````

`api/users/v2/create-user/create-user.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: users/v2/create-user
source: api/users/v2/create-user/create-user.md
source-hash: sha256:7f02...aa
source-type: natural
dependencies:
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:10:00+08:00
approval-ref: pr#122
approved-commit: a1b2c122
security-items: [{ item: roles, risk: high, reason: "Only admin may create users", approved-by: [jack@example, mei@example] }]
signature: MEUCIG...
---
## Route
- method: POST
- path: /users/v2/create-user

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

### p2 rate-limit
- per-minute: 30

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| body.email | string | format: email | yes |
| body.name | string | min: 1, max: 50 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 201 | { id, email, name } | Location |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { id: u.id, email: u.email, name: u.name }

## Steps
### s1 method
- op: method
- target: user.register
- args: transform.in
- out: u
- source-lines: 4-4

### s2 respond
- op: respond
- status: 201
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 4-4

## Errors
| status | code | from |
|---|---|---|
| 409 | EMAIL_TAKEN | user.register |
| 422 | VALIDATION_FAILED | platform |
````

## Security considerations

- **Default deny**: every endpoint goes through `auth` unless explicitly removed; removing it (`-auth`) must be paired with `public: true` (§3, §4, §10.3).
- **Ordering**: `auth` must run before every `method` step and all other logic (`## Order` declares `must-run: first`) (§4).
- **Algorithms**: only RS256 and ES256 are allowed; `none` is forbidden and mixing with HS is not allowed. `iss`, `aud`, `exp` and `nbf` must be verified. JWKS rotation and cache TTL are supported (§4). Any other algorithm or a missing claim check is a deploy error (§10.3).
- **Roles**: `roles-claim` defaults to `roles`; `roles: []` is an error (§4). Adding, changing, relaxing or removing role checks is a **high-risk** security item requiring two non-author approvers (§2A.5).
- **Context injection**: after success, `auth` injects `ctx.tenant_id` and `ctx.user`; logic cannot modify them (§4, §12.5). Methods do not inherit api pipelines; they only receive `ctx` (§12.5).
- **Errors**: unauthenticated / invalid token → 401 with `WWW-Authenticate` (RFC 6750); authenticated but not permitted → 403 (§0.1, §8). Authentication failures never distinguish "user not found" from "bad credentials" (§8).
- **Logging**: tokens are never logged (§9).

## Spec gaps

- The spec shows only `mode: jwt` and `roles` as `auth` parameters. The parameter names for issuer, audience, JWKS URL and JWKS cache TTL in the `auth` pipeline's `## Parameters` schema are **not specified** (the built-in lock is platform-provided). Do not invent them in source; ask the platform for the built-in `auth` parameter schema.
- How `roles-claim` is overridden (parameter name in lock) is not shown.
