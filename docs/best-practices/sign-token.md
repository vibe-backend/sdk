# `sign-token`: token issuance capability

## Purpose

Centralize JWT issuance in a single model method flagged with `capabilities: [sign-token]`. Only such a method may use the `sign-jwt` op; every other method or pipeline that needs a token calls it via the `method` op (Decisions 5, 30; §12.6).

## Natural-language source example

`models/user/methods/sign-token.md`

```text
# sign-token
Sign an access token for a user.
This method is responsible for signing tokens.
Input: sub (user id, uuid).
Output: token (access token) and refresh_token.
The token is valid for 1 hour, uses ES256, and the key is JWT_SIGNING_KEY.
```

## Compiled lock md example

`models/user/methods/sign-token.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/sign-token
source: models/user/methods/sign-token.md
source-hash: sha256:3b90...d4
source-type: natural
dependencies:
  - { id: user, kind: model, hash: sha256:aa41...0f }
vibe-version: 1.0.0-rc.7
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:22:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: sign-token, risk: high, reason: "Sole token issuance entry point", approved-by: [jack@example, mei@example] }
signature: MEQCIA...
---
## Signature
- input: { sub: { type: uuid, required: true } }
- output: { token: string, refresh_token: string }
- errors: []
- writes: false
- capabilities: [sign-token]

## Calls
(none)

## Steps
### s1 query
- op: query
- table: users
- sql: select id::text as id from users where id = $1::uuid and deleted_at is null
- params: [input.sub]
- out: u
- source-lines: 1-2

### s2 sign-jwt
- op: sign-jwt
- alg: ES256
- key-ref: JWT_SIGNING_KEY
- claims: { sub: u.id }
- platform-claims: [iss, aud, exp, nbf, iat, jti, roles, tenant_id]
- platform-claims-source: { roles: users.roles where id = sub, tenant_id: ctx.tenant_id }
- ttl: 1h
- ttl-cap: 1h
- issue-refresh: platform-decided
- out: token
- source-lines: 1-5

### s3 respond
- op: respond
- value: { token: token.access, refresh_token: token.refresh }
- source-lines: 4-4
````

Callers use it like this (from `user.login`):

````markdown
### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 10-10
````

`issue-refresh` is decided by the platform from the call source: `new-family` when called from login, `rotate` when injected by the `token-refresh` pipeline, otherwise `none`; a method can never choose `rotate` itself (§4A.1).

## Security considerations

- **Explicit capability**: `sign-jwt` is allowed only in a method whose `## Signature` declares `capabilities: [sign-token]`; the name is irrelevant (a method called `sign-token` without the flag is rejected) (§2A.4, §12.6). The capability must be a **model-layer** method (§10.8).
- Write the capability as its own clear sentence ("This method is responsible for signing tokens") — the semantic-consistency check errors if the lock signs tokens and the source never mentions it (§2A.7, §10.1).
- **Claim whitelist**: the method may pass only `sub` and platform-allowed custom claims (e.g. `sid`). `roles` is read by the platform from the DB (looking up `sub` in the current tenant). `tenant_id` has exactly one source: `ctx.tenant_id`, resolved by the platform from the slug (public) or the verified token (auth); never from the client, the method or the `users` table (Decision 62). `iss`, `aud`, `exp`, `nbf`, `iat`, `jti` are added by the platform. Passing any of these is an error (§12.6, §10.8).
- **TTL**: access tokens are capped at 1h (§4A.1, §10.8).
- **Algorithm/key**: JWT only, RS256 or ES256; the private key is a platform secret referenced via `key-ref`, never inline (§12.6, §9B). An ES256 key that cannot sign (trailing data, private scalar out of range) is refused when `vibe run` starts.
- `capabilities: [sign-token]`, the claim whitelist and token ttl are all **high-risk** security items (two non-author approvers) (§2A.5).
- Tokens are never logged (§9).
- ⚠️ **Any caller can sign an arbitrary `sub`**: `user.sign-token` takes `sub` as plain input and does not verify that the caller authenticated that user. Any method or pipeline that can call it can mint a token for any user in the tenant. Keep callers to a minimum (`user.login`, `token-refresh`), review every new `## Calls` edge pointing at it, and watch the "signs token" column of the effective-permission summary (§10.10). The spec has no caller allowlist for capability methods.

## Spec gaps

- The full platform **custom-claim whitelist** is not listed (only `sid` as an example).
- `sign-jwt` parameter `platform-claims-source` uses a pseudo-query (`users.roles where id = sub`) that is not otherwise defined.
- **Caller restriction**: there is no way to restrict which methods/pipelines may call a `sign-token` method.
- Whether `respond` in a method needs a `status` is not specified (method responds omit it in the spec).
