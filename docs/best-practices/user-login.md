# `user.login`

## Purpose

Authenticate a **verified** user with email + password and return an access token and a refresh token. `user.login` is a model method; it calls `user.sign-token` (the only token-issuing method) via the `method` op. It is exposed through a **public** api feature (users are not yet logged in), which therefore writes `last_login_at` as a public write (§12.8).

In v1.7, login only accepts users whose email is verified (`email_verified_at is not null`); an unverified user gets exactly the same 401 `INVALID_CREDENTIALS` as a wrong password (Decision 55). Because public registration and admin invites set `password` and `email_verified_at` in the same UPDATE (Decisions 67, 69), an account either has both or neither. See [user-register.md](user-register.md).

## Natural-language source example

`models/user/methods/login.md`

```text
# login
Log in with email and password.
Input: email (valid email, required), password (1 to 256 characters, required).
Output: user (id, email, name, roles), token and refresh_token.

Steps:
1. Find the user by email; only verified, non-deleted users count.
2. Compare the password; if the user is not found or the password is wrong, fail INVALID_CREDENTIALS.
3. On success record the last login time.
4. Sign tokens with user.sign-token and return user, token and refresh_token.
```

`api/auth/v1/login/login.md` (route `POST /auth/v1/{slug}/login`)

```text
# Login
POST, path /auth/v1/{slug}/login.
Public, no auth, tenant is determined by slug.
This endpoint writes data (records the last login time); public writes are allowed.
Protect with login-guard: 10 per IP per minute, 5 per account per minute; also at most 300 per tenant per minute.

Request body: email (valid email, required), password (1 to 256 characters, required).
Log in with user.login; on success return 200 with token, refresh_token and user (id, email, name, roles).
Wrong credentials return 401 INVALID_CREDENTIALS; validation failure returns 422.
```

## Compiled lock md example

`models/user/methods/login.lock.md`

> **Note:** `INVALID_CREDENTIALS` is a **self-defined error code**, declared by the `user.login` method in its `## Signature` `errors` and mapped to 401 in the api lock. It is not a platform-reserved code, though the spec reuses the same name for lockout responses (§4A.3).

````markdown
---
lock-version: 1
kind: method
id: user/methods/login
source: models/user/methods/login.md
source-hash: sha256:e81f...2a
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
  - { id: user/methods/sign-token, kind: method, hash: sha256:3b90...d4 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01, llm-provider: openai, endpoint-host: api.openai.com, endpoint-private: false }
compiler-hash: sha256:c0de...77
security-items:
  - { item: write-sql, risk: medium, reason: "Records last login time" }
---
## Signature
- input: { email: { type: string, format: email, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: { user: { id, email, name, roles }, token: string, refresh_token: string }
- errors: [INVALID_CREDENTIALS]
- writes: true

## Calls
- methods: [user.sign-token]
- external: (none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `select id, email, name, roles, password from users where email = $1 and deleted_at is null and email_verified_at is not null`
- params: [input.email]
- writes: false
- out: u
- source-lines: 7-7

### s2 verify-password
- op: verify-password
- cost: high
- hash: u.password
- plain: input.password
- missing-user: dummy-hash
- out: ok
- source-lines: 8-8

### s3 validate
- op: validate
- when: `ok != true`
- fail: INVALID_CREDENTIALS
- source-lines: 8-8

### s4 query
- op: query
- provider: main
- table: users
- sql: `update users set last_login_at = now() where id = $1`
- params: [u.id]
- writes: true
- scope: pk
- columns: [last_login_at]
- out: (none)
- source-lines: 9-9

### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 10-10

### s6 respond
- op: respond
- value: { user: { id: u.id, email: u.email, name: u.name, roles: u.roles }, token: t.token, refresh_token: t.refresh_token }
- source-lines: 10-10
````

`api/auth/v1/login/login.lock.md` (sections only; frontmatter per §2A.1 with `kind: api`, `scope: feature`, and security items `public`, `auth-removed`, `public-writes`, all high risk)

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/login

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_reader + public_writer
- writable-columns: { users: [last_login_at] }
- challenge: { type: captcha, when: after-failures, after-failures: 3 }   # 4A.3
- min-response: 500ms
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required

## Pipelines
### p1 -auth
- removed: true

### p2 login-guard
- ip-per-minute: 10
- account-per-minute: 5

### p3 rate-limit
- per-minute: 300
- key: tenant

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.email | string | format: email | yes |
| body.password | string | min: 1, max: 256 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { token, refresh_token, user: { id, email, name, roles } } | |

## Transform
- in: { email: req.body.email, password: req.body.password }
- out: { token: result.token, refresh_token: result.refresh_token, user: result.user }

## Steps
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
- source-lines: 7-7

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 7-7

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | user.login |
| 422 | VALIDATION_FAILED | platform |
````

`INVALID_CREDENTIALS` above is the self-defined code from `user.login` (see the note before the method lock).

The model lock marks `last_login_at` `public-writable` and includes `UPDATE(last_login_at)` in the `public_writer` grant (§12.8); that is itself a high-risk `public-writable-column` item. Calling `user.sign-token` from login makes `sign-jwt` issue a refresh token in a **new family** (`issue-refresh: new-family`, decided by the platform from the call source, §4A.1).

## Security considerations

- **Verified email required** (Decision 55): the s1 query filters `email_verified_at is not null`; an unverified (or never-invited-in) user falls into the same `missing-user: dummy-hash` path, so timing and response match a wrong password.
- **Anti-enumeration**: `verify-password` compares in constant time; when the user does not exist the platform compares against a dummy hash (`missing-user: dummy-hash`). Both cases return the same `INVALID_CREDENTIALS` (§12.6, §8).
- **Fixed response time**: login-class endpoints (any endpoint with a `verify-password` step) are padded to **500ms** (`min-response: 500ms`); exceeding it is only a warning (§0.1, §7.2, Decision 36).
- **Lockout**: 5 failures in 15 minutes locks the account for 15 minutes; after 3 failures per account or per IP within 15 minutes, captcha is required (`challenge: { type: captcha, when: after-failures, after-failures: 3 }`, Decision 49); during lockout the response is identical to a wrong password (401 `INVALID_CREDENTIALS`, padded to 500ms); email reset link unlocks immediately; lock/unlock are audited and the user is notified (§4A.3).
- **Rate limits**: login must be rate-limited per IP **and** per account (e.g. `login-guard`, 10/IP/min) (§4A.3, §12.6). `verify-password` is `cost: high`, so it automatically gets half the endpoint limit on public paths (§7.2).
- **Public write**: only `last_login_at` is writable; `UPDATE` uses `scope: pk` (§7.2).
- The `password` column may be selected only to feed `verify-password` (§10.8); `login-guard` redacts `req.body.password` and records only an HMAC of the email (Decision 68, §11.2).
- Exclude soft-deleted users (`deleted_at is null`).
- **Password hashing** (Decision 46): argon2id m=64 MiB, t=3, p=1; on successful login the platform rehashes automatically if stored PHC parameters differ, using the `credential_rehash` role (§4A.2). Locks do not declare this write.
- **Refresh token**: returned alongside the access token; stored only as an HMAC hash in `_platform_refresh_tokens` (Decision 45). See [refresh-token-rotation.md](refresh-token-rotation.md).
- Review highlights: `public: true`, `-auth`, `public-writes: true` (high) plus the write SQL (medium) (§12.8).

## Spec gaps

- How lockout state and failure counters are stored, and how `challenge: { when: after-failures }` is evaluated relative to the steps, is platform-internal and not expressed in lock terms.
- `role: public_reader + public_writer` is used in §12.8 but the `role` value grammar is not defined elsewhere.
- The `login-guard` account key is `input.email` (§11.2) while the login method's input is also `email`; how a pipeline resolves `input.*` before the method runs (versus `req.body.email`) is not spelled out.
