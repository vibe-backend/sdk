# Registration: `user.self-register`, `user.complete-verification` and admin `user.register`

## Purpose

Let visitors sign up without enabling account pre-hijacking or email enumeration. Spec v1.7 defines public registration as a two-step flow (§12.11, Decisions 52–56, 67):

1. **Register** (`POST /auth/v1/{slug}/register` → `user.self-register`): accepts **only email and name, never a password**. It creates an unverified user without a password (or, for an existing unverified email, only rotates the verification token), enqueues one email, and **always returns 202 with the same body**, whether or not the email already exists (Decisions 56, 67).
2. **Verify and set password** (`POST /auth/v1/{slug}/verify-email` → `user.complete-verification`): the email link opens a set-password page; one UPDATE sets `password` **and** `email_verified_at` together and clears the token. Afterwards all refresh-token families of the user are revoked (Decision 67).

Login only accepts verified users (Decision 55, see [user-login.md](user-login.md)). The admin path uses a separate method, `user.register`, which may return 409 `EMAIL_TAKEN` and is followed by an invite (Decision 69, see [auth-jwt.md](auth-jwt.md)). The public method must never reveal whether an email exists (§12.11).

Prerequisites from §12.11: migration `0004-add-email-verification` (nullable `password`, `email_verified_at`, `email_verify_hash`, `email_verify_expires_at`, `email_verify_purpose`, and the `email_outbox` table), the `email-outbox` model and its `enqueue` method (emails are sent by the platform mail worker, not by an external `call`; Decision 54).

## Natural-language source example

`models/user/methods/self-register.md`

```text
# self-register
A visitor registers; only email and name are accepted, no password.
Input: email (valid email, at most 254 characters), name (1 to 50 characters).

Steps:
1. Generate an email verification token valid for 24 hours.
2. Create the user, unverified and without a password. If the email already exists but is unverified, only replace the token and expiry (the old token stops working immediately) and change nothing else; if it is already verified, or has an unexpired admin invite, change nothing.
3. Enqueue one email with email-outbox.enqueue (purpose register): new or unverified users get the verification email (with the token); verified users or users with an unexpired invite get an "you already have an account" notice.
4. Never reveal whether the email already exists; the output is always empty.
```

`models/user/methods/complete-verification.md`

```text
# complete-verification
Confirm the email with the token from the email link and set the password at the same time. Used for both registration and admin invites.
Input: token (string, 43 characters), password (string).
This method confirms verification and also sets the password.

Steps:
1. Always hash the password first (fail WEAK_PASSWORD if it does not meet the platform password floor).
2. If the token is correct, unexpired and not yet used, set the password, record the verification time and clear the token in a single update; otherwise fail INVALID_TOKEN.
```

`api/auth/v1/register/register.md` (route `POST /auth/v1/{slug}/register`)

```text
# Register
POST, path /auth/v1/{slug}/register.
Public, no auth, tenant is determined by slug.
Creates an unverified user; public writes are allowed; no password is written. Captcha is always required.
At most 5 per minute per IP and 60 per minute per tenant; use the platform default daily write quota; audit, recording only a hash of the email.

Request body: email (valid email, at most 254 characters), name (1 to 50 characters).
Register with user.self-register.
Whether or not the email already exists, return 202 with the same message "If the details are correct, you will receive a confirmation email".
```

`api/auth/v1/verify-email/verify-email.md` (route `POST /auth/v1/{slug}/verify-email`)

```text
# Verify email and set password
POST, path /auth/v1/{slug}/verify-email.
Public, no auth, tenant is determined by slug.
The email link opens a set-password page; after the user enters a password, the page sends the token and the password here.
Sets the password and confirms the email; public writes, password writes and verification writes are allowed. No captcha (no INSERT).
At most 10 per minute per IP and 120 per minute per tenant; audit without plaintext token or email.

Request body: token (43-character base64url), password (1 to 256 characters).
Use user.complete-verification. Return 204 on success.
A wrong, expired or used token returns 422 INVALID_TOKEN; a weak password returns 422 WEAK_PASSWORD.
```

## Compiled lock md example

`models/user/methods/self-register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/self-register
source: models/user/methods/self-register.md
source-hash: sha256:d3c9...10
source-type: natural
dependencies:
  - { id: email-outbox/methods/enqueue, kind: method, hash: sha256:81aa...c2 }
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
vibe-version: 1.0.0-rc.3
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T15:40:00+08:00
approval-ref: pr#150
approved-commit: a1b2c150
security-items:
  - { item: write-sql, risk: medium, reason: "Create unverified user or rotate verification token", approved-by: [jack@example] }
signature: MEYCIQ...
---
## Signature
- input: { email: { type: string, format: email, max: 254, required: true }, name: { type: string, min: 1, max: 50, required: true } }
- output: {}
- errors: []
- writes: true

## Calls
- methods: [email-outbox.enqueue]
- external: (none)

## Steps
### s1 random-token
- op: random-token
- bytes: 32
- out: vt
- source-lines: 5-5

### s2 query
- op: query
- provider: main
- table: users
- sql: `insert into users (email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose) values ($1, $2, $3, now() + interval '24 hours', 'register') on conflict (tenant_id, email) do update set email_verify_hash = excluded.email_verify_hash, email_verify_expires_at = excluded.email_verify_expires_at, email_verify_purpose = 'register' where users.email_verified_at is null and users.deleted_at is null and not (users.email_verify_purpose = 'invite' and users.email_verify_expires_at > now()) returning id`
- params: [input.email, input.name, vt.hash]
- writes: true
- scope: pk
- columns: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }
- out: ins
- source-lines: 6-6

### s3 method
- op: method
- target: email-outbox.enqueue
- args: { user_id: ins.id, email: input.email, token: vt.value, purpose: register }
- out: (none)
- source-lines: 7-7

### s4 respond
- op: respond
- value: {}
- source-lines: 8-8
````

`models/user/methods/complete-verification.lock.md` (sections only; frontmatter per §2A.1; security items `set-password` and `confirm-verification`, both high risk)

````markdown
## Signature
- input: { token: { type: string, length: 43, pattern: base64url, sensitive: true, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: {}
- errors: [WEAK_PASSWORD, INVALID_TOKEN]
- writes: true
- capabilities: [set-password, confirm-verification]

## Calls
(none)

## Steps
### s1 hash-password
- op: hash-password
- cost: high
- plain: input.password
- policy: platform-floor
- out: pw
- source-lines: 6-6

### s2 hash-token
- op: hash-token
- value: input.token
- out: th
- source-lines: 7-7

### s3 query
- op: query
- provider: main
- table: users
- sql: `update users set password = $1, email_verified_at = now(), email_verify_hash = null, email_verify_expires_at = null, email_verify_purpose = null where email_verify_hash = $2 and email_verify_expires_at > now() and email_verified_at is null and deleted_at is null returning id`
- params: [pw, th]
- writes: true
- scope: pk
- columns: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose]
- password-source: s1
- out: upd
- source-lines: 7-7

### s4 validate
- op: validate
- when: `upd.id == null`
- fail: INVALID_TOKEN
- source-lines: 7-7

### s5 respond
- op: respond
- value: {}
- source-lines: 7-7
````

`api/auth/v1/register/register.lock.md` (sections only; security items `public`, `auth-removed`, `public-writes`, all high risk)

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/register

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { users: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }, email_outbox: { insert: [user_id, email, recipient_hash, template, token] } }
- effective-grant: public_writer [users: INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose); email_outbox: INSERT(user_id, email, recipient_hash, template, token)]
- challenge: { type: captcha, when: always }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 60
- key: tenant

### p4 audit-log
- event: public-register
- hash: [req.body.email]

## Limits
- provider: main
- timeout: 10s
- body: 4KB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | | yes |
| body.email | string | format: email, max: 254, untrusted | yes |
| body.name | string | min: 1, max: 50, untrusted | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 202 | { message: "If the details are correct, you will receive a confirmation email" } | |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { message: "If the details are correct, you will receive a confirmation email" }

## Steps
### s1 method
- op: method
- target: user.self-register
- args: transform.in
- out: (none)
- source-lines: 7-7

### s2 respond
- op: respond
- status: 202
- value: transform.out
- source-lines: 8-8

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

`api/auth/v1/verify-email/verify-email.lock.md` (`## Access` and `## Errors` only; security items `public`, `auth-removed`, `public-writes`, `password-write`, `verification-write`, all high risk, which is exactly the 5-item PR cap)

````markdown
## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- password-write: allow
- verification-write: allow
- role: public_writer
- writable-columns: { users: { update: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose] } }
- effective-grant: public_writer [users: UPDATE(password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose)]
- challenge: { type: none }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required
- min-response: 300ms

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | WEAK_PASSWORD | WEAK_PASSWORD |
| 422 | INVALID_TOKEN | INVALID_TOKEN |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

Its pipelines are `-auth`, `rate-limit` 10/min by `ip`, `rate-limit` 120/min by `tenant`, and `audit-log` with `redact: [req.body.token, req.body.password]`; body limit 2KB; success is 204 (§12.11.7).

## Security considerations

- **No password at registration** (Decision 67): `user.self-register` has no `set-password` capability and no `hash-password` step. A body containing `password` (or `tenant_id`, `id`, `created_by`, or any undeclared field such as `roles`) is rejected with 422 (§12.11.6).
- **Anti pre-hijacking**: an attacker who registers a victim's email gets an account with no credential; when the victim registers, the token is rotated and the attacker's token stops working. A password can only be set through the emailed link (§12.11.4).
- **Upsert limits**: `ON CONFLICT DO UPDATE` may only touch `email_verify_hash`, `email_verify_expires_at`, `email_verify_purpose`, only when the row is unverified, not deleted and has no unexpired invite. The validator checks that `columns.update` ⊆ `columns.insert` and contains no password, `name`, `email` or blacklisted column. `scope: pk` holds because `(tenant_id, email)` is a unique key (§12.11.4).
- **No enumeration** (Decision 56): every path (new, unverified, verified, invited) does one upsert plus one outbox insert, returns the same 202 body, and is padded to 300ms. The mail worker chooses the template (`verify-email`, `already-registered`, `invite`) from the derived rule in `email-outbox.enqueue` (§12.11.3).
- **Set password and verify atomically** (Decisions 53, 67): only a method with both `set-password` and `confirm-verification` capabilities may write `password`/`email_verified_at` on a public path; the password value must come from `hash-password` in the same method (`password-source: s1`); the WHERE uses the `hash-token` output, checks expiry and unverified state, and clears the token hash; the api must declare `password-write: allow` and `verification-write: allow` (Decision 47, §7.2). There is never a "verified without password" or "password but unverified" state, which keeps Decision 55 sound.
- **Constant work**: `hash-password` runs on every verify-email call, even with a wrong token, using a fixed-size argon2 worker pool; responses are padded to 300ms, and past 300ms to the next multiple of 100ms (Decision 66).
- **Password policy** (§4A.2, Decision 46): 12–256 characters, breached-password check (k-anonymity), argon2id with memory 64 MiB, iterations 3, parallelism 1, 16-byte salt, 32-byte output in PHC format; automatic rehash on login when parameters change.
- **Tokens** (Decision 52): `random-token` produces 32 bytes, exposed only as a 43-character base64url string; the DB stores `HMAC-SHA-256(pepper, token)` only. `vt.value` may only go to a `sensitive` method input (here `email-outbox.enqueue`) and never to `respond`, logs or plaintext SQL columns. The outbox token is crypto-shredded right after sending (Decision 54).
- **Link format**: `https://{frontend}/verify#token={token}`; the token sits in the fragment so it never reaches server logs or `Referer` (§12.11.7).
- **Audit**: never plaintext email (`hash: [req.body.email]`), token or password (Decision 68).
- **Session revocation**: after verification succeeds the platform revokes all of the user's refresh families (`revoke_reason: email-verified`) (Decision 67).
- **Cleanup**: unverified accounts past `email_verify_expires_at` are anonymized (email → `erased+{id}@invalid`, token hash cleared) and soft-deleted by a platform job with the `erasure` role; never DELETE (Decisions 13, 67).
- **Rate limits**: register 5/min per IP and 60/min per tenant (no `cost: high` step, so not halved); verify-email 10/120 per minute, effectively halved to 5/60 because `hash-password` is `cost: high` (Decisions 25, 39).
- **PR split**: the example needs 4 PRs (migration; model + methods; register api; verify-email api) to stay within 5 security items per PR (§12.11.8).
- **Admin path**: `user.register` (admin, authenticated) may return 409 `EMAIL_TAKEN`; it sets no password and is followed by `user.invite` (Decision 69).

## Spec gaps

- The admin `user.register` lock and the `user.invite` lock are **not shown** in the spec (§11.4, §12.11.9 describe them only in prose).
- The rule that verify-email's effective rate limit is halved by `cost: high` (§12.11.7) while register's is not depends on a step inside a called method; how the validator propagates `cost: high` from methods to api limits is not spelled out.
- What the client sees if `complete-verification` is called for a soft-deleted or erased user is covered only by the generic 422 `INVALID_TOKEN`; no separate guidance exists.
