# Refresh token rotation

## Purpose

Keep access tokens short-lived (≤ 1h) while letting clients stay signed in, using **rotating refresh tokens** with reuse detection (Decision 6, §4A.1). Refresh is handled by the built-in `token-refresh` pipeline (Decision 45); logout by the built-in `token-revoke` pipeline (Decision 65). The first refresh token is issued at login: `sign-jwt` with `issue-refresh: new-family` (§4A.1, F10; see [user-login.md](user-login.md)).

Spec decision 149 (PM-approved 2026-10-17, §4A.1c): the `sign-method` must be read-only (`writes: false`), including methods it calls through `op: method`. `vibe build` and `vibe run` will reject it otherwise (`REFRESH_SIGN_METHOD_WRITES`). **Not yet implemented:** this blocking and the platform-recorded `last_refreshed_at` arrive with the D149 code PR (#270); until then, follow the rule by hand. The platform records the last refresh time per family itself (`_platform_refresh_families.last_refreshed_at`); after upgrading, start `vibe run --db postgres` with `VIBE_MIGRATE_DATABASE_URL` set; put any other write in a separate write method that the client calls after the refresh succeeds.

## Natural-language source example

`api/auth/v1/refresh.md` (route `POST /auth/v1/{slug}/refresh`)

```text
# Refresh token
POST, path /auth/v1/{slug}/refresh.
This is the refresh token endpoint. Public (the caller only holds a refresh token), no auth, tenant is determined by slug.
Sign new access tokens with user.sign-token. The refresh token is sent in the body.
Request body: refresh_token (string, required).
Return 200 with the new token and refresh_token.
```

`api/auth/v1/logout.md` (route `POST /auth/v1/logout`)

```text
# Logout
POST. Requires JWT. Revokes the refresh token family of the given refresh token.
Request body: refresh_token (string, required).
Return 204.
```

## Compiled lock md example

`api/auth/v1/refresh.lock.md` (sections only; frontmatter per §2A.1, `kind: api`, `scope: feature`; security items `public` and `auth-removed`, high risk)

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/refresh

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: false
- external-calls: forbidden
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 30
- key: ip

### p3 rate-limit
- per-minute: 10
- key: family

### p4 token-refresh
- sign-method: user.sign-token
- refresh-ttl: 30d
- idle-ttl: 7d
- reuse-grace: 10s
- transport: body

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | | yes |
| body.refresh_token | string | | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { token, refresh_token } | |

## Transform
(none)

## Steps
(none)

## Errors
| status | code | from |
| --- | --- | --- |
| 401 | INVALID_CREDENTIALS | INVALID_CREDENTIALS |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

`api/auth/v1/logout.lock.md` (`## Pipelines` only)

````markdown
## Pipelines
### p1 auth
- mode: jwt

### p2 token-revoke
- scope: family
- token-source: body
````

> ⚠️ `token-refresh` and `token-revoke` parameters, and the refresh classification (public, 30/min per IP, 10/min per family, 300ms), are from the spec (§4A.1, Decisions 45, 65, 72). The response field names, the 401 error code, the empty `## Transform`/`## Steps`, and `role` being omitted are **assumptions**: `INVALID_CREDENTIALS` here is a **self-defined error code** chosen for this example; the spec defines no error code for `token-refresh`.

## Security considerations

Platform-enforced rules (§4A.1, Decisions 6, 45, 60, 61, 72):

- **Access token**: JWT, ttl ≤ 1 hour.
- **Refresh token**: opaque random value (≥ 256 bits); stored only as `HMAC-SHA-256(pepper, token)` in the platform table `_platform_refresh_tokens` (one per tenant schema, RLS forced); user locks cannot query it; only the platform `refresh_store` role reads/writes it.
- **Lifetime**: `refresh-ttl` 30d absolute from the family's first issuance (cap 30d); `idle-ttl` 7d (cap 7d).
- **Rotation on every use**: new refresh + access token; the old refresh token is invalidated immediately.
- **Families**: reuse of an invalidated token revokes the whole family, writes an audit record and notifies the user. Revocation is an UPDATE of `revoked_at`/`revoke_reason`, never DELETE.
- **Grace window** (Decision 61, `reuse-grace` ≤ 10s): within the window the platform does **not** return the earlier plaintext (it only has the hash). It revokes the previously issued child (`revoke_reason: grace-reissue`), issues a new child in the same family, and sets `grace_reissued_at`, reserving the parent so only one concurrent grace request proceeds. A concurrent presentation while issuance is in progress returns retryable 503 `TX_CONFLICT`; a presentation after that grace reissue completes counts as reuse. Each old token can be reissued at most once; a second use, use after the window, or use after the child was rotated again counts as reuse and revokes the family. `used_at` is not changed, so the window never extends.
- **Cookie transport** (`transport: cookie`): `__Host-vibe_rt_{slug}` cookie, always `HttpOnly; Secure; SameSite=<cookie-mode>; Path=/; Max-Age=<seconds>`, no `Domain` (spec v1.8 Decisions 101, 143; Decision 60's slug `Path` applies only to `transport: body`).
- **Classification** (Decision 72): `public: true` (no access token) but **not** a public write: the user lock has no write step and only the platform writes its own table, so no daily public-write quota and no captcha; fixed limits of 30/min per IP and 10/min per family; minimum response 300ms.
- **Logout** (`token-revoke`, Decision 65): requires `auth`; `scope: family` revokes the current family (logout), `scope: all` revokes all of the user's families (logout everywhere); only the caller's own (`sub`) families. Exception (spec v1.8 Decision 145): with `token-source: cookie` and `scope: family`, logout needs no access token but must pass the refresh route's `Origin` allowlist and `X-Vibe-CSRF: 1` (otherwise 403 `CSRF_REJECTED`); it then reads the token only from the `__Host-` cookie and always returns 204, revoking only that cookie's family. Logout never returns 401 `INVALID_REFRESH_TOKEN`; it returns 204 even if the token is unknown or already revoked; with cookie transport it clears the cookie (`Max-Age=0`).
- **Other revocations**: email verification revokes all families (`email-verified`, Decision 67); privacy deletion revokes all families (`erasure`, §12.10).
- **Client storage** (general guidance, outside the spec): prefer `transport: cookie` in browsers; native apps should keep the token in the OS keychain/keystore. Never store refresh tokens in `localStorage`/`sessionStorage`.
- Tokens are never logged (§9). Signing goes only through the `sign-token` capability (§12.6) — see [sign-token.md](sign-token.md).

## Spec gaps

- The error code and response field names of `token-refresh` are **not specified**.
- What a feature lock's `## Transform`/`## Steps` contain when a pipeline produces the response is **not specified**.
- Decision 72 says the fixed refresh limits are "written into the lock using the §4.3 schema", but whether they appear as explicit `rate-limit` steps (as shown here) or are implied by `token-refresh` is not shown.
- The optional client-fingerprint check during the grace window (§4A.1 step 5) has no parameter.

Rotation records a reservation in a short transaction, then runs issuance without holding a rotation connection or lock. A live reservation returns retryable 503 `TX_CONFLICT`, without `Set-Cookie`. Reuse is checked first: a used token outside its grace window revokes the family even while pending. After finalization, retrying the old token within grace follows decision 61 (one grace reissue); after grace it is reuse.

Issuance errors clear the reservation and leave the presented token usable. Cancellation, issuance timeout, finalization failure and expired reservations also clear the reservation and leave the presented token current, including after process exit. Only a committed child consumes the parent. The PostgreSQL reservation lifetime is the issuance bound (statement plus pool timeout) plus statement timeout; finalization requires the original, unexpired reservation and current parent. Sign-method writes commit independently. Refresh and logout responses, including 405, include `Cache-Control: no-store` and `Pragma: no-cache`. Database/pool timeouts return 504 `TX_TIMEOUT`; conflicts return 503 `TX_CONFLICT`.

On upgrade, start `vibe run --db postgres` with `VIBE_MIGRATE_DATABASE_URL` set. Migration adds the reservation columns per tenant; serving verifies the installed table and grants.

Startup migrations (decision 179) use `VIBE_MIGRATE_DATABASE_URL` as owner;
serving uses the non-owner `VIBE_DATABASE_URL`. Owner credentials may be omitted
when nothing is pending. See [automatic migrations](../runtime-config.md#automatic-migrations-decision-179)
for tenant selection, append-only records, advisory locks and the deliberate
`--allow-destructive-migrations` backup/start-once/restart procedure.
