# Auth pipeline: JWT

## Purpose

Require a valid JWT on every endpoint of an API definition. `auth` is a built-in pipeline (§4) whose lock is provided and signed by the platform; you enable it and pass parameters (`mode`, `roles`, optionally `iss`, `aud`, `jwks-uri`) (§4.1, Decision 44). Declare it once in `_definition.md` so it is inherited by every version and feature (§3), and restrict roles per feature where needed.

The example feature is the admin **create-user** endpoint from §11.4. Under v1.7 it no longer sets a password: it creates an unverified user without a password and sends an **invite** (Decision 69).

## Natural-language source example

`api/users/_definition.md`

```text
# users
User management API. Allowed providers: main and enterprise; default is main.
All endpoints require JWT authentication.
```

`api/users/v2/create-user/create-user.md` (route `POST /users/v2/create-user`)

```text
# Create user
POST. Only admin may use it, at most 30 requests per minute.
The request body must contain email (valid email) and name (1 to 50 characters).
Create the user with user.register (no password, unverified), then send an invitation with user.invite; on success return 201 with the new user's id, email and name, plus Location.
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

With no `iss`/`aud`/`jwks-uri` given, the platform defaults apply: `iss` = `https://auth.{platform-domain}/{project}`, `aud` = `{project}`, `jwks-uri` = `{iss}/.well-known/jwks.json` (§4.1).

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
  - { id: user/methods/invite, kind: method, hash: sha256:<...> }
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01, llm-provider: openai, endpoint-host: api.openai.com, endpoint-private: false }
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
- key: user

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| body.email | string | format: email | yes |
| body.name | string | min: 1, max: 50 | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
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

### s2 method
- op: method
- target: user.invite
- args: { id: u.id }
- out: (none)
- source-lines: 4-4

### s3 respond
- op: respond
- status: 201
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 4-4

## Errors
| status | code | from |
| --- | --- | --- |
| 409 | EMAIL_TAKEN | user.register |
| 422 | VALIDATION_FAILED | platform |
````

> ⚠️ The spec's own §11.4 lock shows only the `user.register` step even though its source says "then send an invitation with user.invite". A lock that omits the invite step would contain **less** than the source says; this page adds `s2` so the lock matches the source and Decision 69. The `user.invite` input (`{ id }`) and the `key: user` rate-limit key are illustrative; see Spec gaps.

## Invite flow (Decision 69, §12.11.9)

1. Admin calls create-user → `user.register` creates the user with **no password** and `email_verified_at = null` (the `password` column is nullable since migration `0004`, §12.11.1).
2. `user.invite` generates a token with `random-token`, writes its hash to `users.email_verify_hash`, sets `email_verify_expires_at` to **72 hours** later and `email_verify_purpose = invite`, then enqueues an email through `email-outbox.enqueue` with `purpose: invite` (fixed in the lock) and the `invite` template.
3. The user opens the invite link, sets a password on the same set-password page as self-registration, which posts to `POST /auth/v1/{slug}/verify-email` → `user.complete-verification`. One UPDATE sets `password` **and** `email_verified_at` together; all refresh-token families are then revoked (Decision 67). See [user-register.md](user-register.md).
4. Until then the user has no password and is unverified, so login is impossible (Decision 55).
5. Re-inviting overwrites the token hash and expiry, so the old invite stops working immediately. An expired, unaccepted invite is anonymized and soft-deleted by the same cleanup job as unverified self-registrations (§12.11.4).
6. A public self-registration for an email with an unexpired invite leaves the invite untouched; the registrant gets only the neutral "already have an account" notice (§12.11.4).

## Security considerations

- **Default deny**: every endpoint goes through `auth` unless explicitly removed; removing it (`-auth`) must be paired with `public: true` (§3, §4, §10.3).
- **Ordering**: `auth` runs first (`must-run: first`, §4.4).
- **JWT validation** (§4.1, Decision 44): `iss` must match exactly; `aud` must contain the configured value; `jwks-uri` must be https; JWKS cached 10 minutes, unknown `kid` triggers a re-fetch (at most once per 60 s per issuer), stale cache usable for at most 1 hour, then all tokens get 401; clock skew 60 s on `exp`/`nbf`/`iat`; alg only ES256 or RS256 (RSA ≥ 2048 bit), `none`/HS* forbidden, `kid` required; tokens whose `exp` is more than 1 hour after `iat` are rejected.
- Changing `iss`, `aud` or `jwks-uri` (for example to use an external IdP) is a **high-risk** security item (§2A.5).
- **Roles**: `roles-claim` defaults to `roles`; `roles: []` is an error (§4). Adding, changing or relaxing role checks is high risk (§2A.5).
- **Context**: `auth` injects `ctx.tenant_id` and `ctx.user`; logic cannot modify them (§4, §12.5).
- **No admin-set passwords**: admins never set or see a user's password; the user sets it via the invite link (Decision 69). The admin path may return 409 `EMAIL_TAKEN` because it is authenticated and admin-only; the public path never may (§12.11).
- **Errors**: 401 with `WWW-Authenticate` (RFC 6750) for missing/invalid tokens, 403 for insufficient role (§8).
- Tokens are never logged (§9).

## Spec gaps

- §11.4's lock omits the `user.invite` step that its own source describes (lock contains less than source). This page adds it.
- No lock is given for `user.register` (admin) or `user.invite`; their `## Signature` (e.g. the invite input) is **not specified**.
- §11.4 `p2 rate-limit` has no `key`, but §4.3 makes `key` required. `key: user` here is an assumption.
- It is not specified whether create-user should still return 201 if the invite enqueue fails, or whether the two steps are atomic (v1 has no transaction block, §14).
