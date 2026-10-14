# `user.register`

## Purpose

Create a new user account in one model method that enforces email uniqueness and the platform password baseline. The spec describes `user.register` as: "If the email is already taken fail EMAIL_TAKEN, otherwise create the user and return id, email, name" — compiled into one SELECT check and one parameterized INSERT (`writes: true`) (§11.7). It is called from api features such as `create-user` (§11.4, see [auth-jwt.md](auth-jwt.md)).

## Natural-language source example

`models/user/methods/register.md`

```
# register
Register a new user.
Input: email (valid email, required), name (1 to 50 characters, required), password (required).
Output: id, email, name.

Steps:
1. Look up the user by email; if the email is already taken fail EMAIL_TAKEN.
2. Hash the password.
3. Create the user and return id, email and name.
```

## Compiled lock md example

`models/user/methods/register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/register
source: models/user/methods/register.md
source-hash: sha256:c3e1...90
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example]
approved-at: 2026-10-14T08:15:00+08:00
approval-ref: pr#124
approved-commit: a1b2c124
security-items:
  - { item: write-sql, risk: medium, reason: "Inserts new user rows", approved-by: [jack@example] }
signature: MEUCIQ...
---
## Signature
- input: { email: { type: string, format: email, required: true }, name: { type: string, min: 1, max: 50, required: true }, password: { type: string, min: 12, max: 256, required: true } }
- output: { id, email, name }
- errors: [EMAIL_TAKEN, WEAK_PASSWORD]
- writes: true

## Calls
- methods: (none)
- external: (none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `select id from users where email = $1`
- params: [input.email]
- writes: false
- out: existing
- source-lines: 6-6

### s2 validate
- op: validate
- when: `existing != null`
- fail: EMAIL_TAKEN
- source-lines: 6-6

### s3 hash-password
- op: hash-password
- cost: high
- plain: input.password
- out: pw
- source-lines: 7-7

### s4 query
- op: query
- provider: main
- table: users
- sql: `insert into users (email, name, password) values ($1, $2, $3) returning id, email, name`
- params: [input.email, input.name, pw]
- writes: true
- columns: [email, name, password]
- out: u
- source-lines: 8-8

### s5 respond
- op: respond
- value: { id: u.id, email: u.email, name: u.name }
- source-lines: 8-8
````

## Security considerations

- **Password baseline**: min 12, max 256 characters, no forced complexity, mandatory breached-password check (k-anonymity, only a hash prefix leaves the platform). `hash-password` enforces it before hashing and fails `WEAK_PASSWORD` (422) (§4A.2). Objects may only **tighten** (e.g. `min: 16`); a lock below the baseline is an error.
- `type: password` fields are hashed with argon2id (platform-managed parameters) and never appear in SELECT results, method output, responses or logs (§12.2). Returning `password` in output is an error (§10.8).
- System fields (`id`, `tenant_id`, `created_at`, ...) are never written from input; `tenant_id` comes from the RLS context (§7.1, §12.9).
- Map method errors in the api lock: `EMAIL_TAKEN` → 409, `WEAK_PASSWORD` → 422; unmapped errors become 500 (§12.4).
- **Public self-signup** (a public endpoint calling `user.register`) is a public write with INSERT, so it requires `public-writes: true`, `challenge: captcha | pow`, IP + tenant rate limits, daily quota, `audit-log` and `writable-columns` (§7.2). `roles` is blacklisted for public writes, so a public signup must never set roles. See [public-writes.md](public-writes.md).
- Email-existence leak: `EMAIL_TAKEN` reveals that an address is registered. The spec mandates anti-enumeration for login (§8, §12.6) but not for registration — decide consciously for public signup.
- Single responsibility: one write SQL to one table — compliant (§12.3a).

## Spec gaps

- No lock for `user.register` is shown in the spec; only a one-line description (§11.7). The lock above is an illustration that follows §2A rules.
- **`hash-password` op parameters** (`plain`, `out`, `cost`) are not specified; only its behavior is (§4A.2). Whether its result is passed as a param or the platform hashes the `password` column on write is ambiguous (§12.2 says writes "always use argon2id").
- Whether `input.password` should carry `min: 12` in `## Signature` or the check lives only in `hash-password` is not specified.
- §7.2 blacklists `password` for public writes "except login-class handled by the platform `hash-password`"; whether public **registration** counts as "login-class" is **ambiguous** (§7.2 defines login-class as endpoints with a `verify-password` step).
- **Duplicate check vs. soft delete**: s1 intentionally does **not** filter `deleted_at is null`, because the `unique` constraint on `email` still covers soft-deleted rows and the INSERT would fail anyway. Whether a soft-deleted (or erased, §12.10, which replaces email with a random unique value) user's email can be re-registered is **not specified**.
- **argon2id parameters and rehash**: §12.2 says the platform manages argon2id parameters, but the values and whether hashes are transparently re-hashed on login when parameters change are **not specified** (spec owner).
