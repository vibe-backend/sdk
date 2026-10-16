# Refresh token rotation

## Purpose

Keep access tokens short-lived (≤ 1h) while letting clients stay signed in, using **rotating refresh tokens** with reuse detection (Decision 6, §4A.1). Refresh is handled by the built-in `token-refresh` pipeline (Decision 45); logout by the built-in `token-revoke` pipeline (Decision 65). The first refresh token is issued at login: `sign-jwt` with `issue-refresh: new-family` (§4A.1, F10; see [user-login.md](user-login.md)).

## Natural-language source example

`api/auth/v1/refresh/refresh.md` (route `POST /auth/v1/{slug}/refresh`)

```text
# Refresh token
POST, path /auth/v1/{slug}/refresh.
This is the refresh token endpoint. Public (the caller only holds a refresh token), no auth, tenant is determined by slug.
Sign new access tokens with user.sign-token. The refresh token is sent in the body.
Request body: refresh_token (string, required).
Return 200 with the new token and refresh_token.
```

`api/auth/v1/logout/logout.md` (route `POST /auth/v1/logout`)

```text
# Logout
POST. Requires JWT. Revokes the refresh token family of the given refresh token.
Request body: refresh_token (string, required).
Return 204.
```

## Compiled lock md example

`api/auth/v1/refresh/refresh.lock.md` (sections only; frontmatter per §2A.1, `kind: api`, `scope: feature`; security items `public` and `auth-removed`, high risk)

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
| 401 | INVALID_CREDENTIALS | token-refresh |
| 422 | VALIDATION_FAILED | platform |
````

`api/auth/v1/logout/logout.lock.md` (`## Pipelines` only)

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
- **Grace window** (Decision 61, `reuse-grace` ≤ 10s): within the window the platform does **not** return the earlier plaintext (it only has the hash). It revokes the previously issued child (`revoke_reason: grace-reissue`), issues a new child in the same family, and sets `grace_reissued_at`, using a conditional `update ... where grace_reissued_at is null` so only one of two concurrent requests wins. Each old token can be reissued at most once; a second use, use after the window, or use after the child was rotated again counts as reuse and revokes the family. `used_at` is not changed, so the window never extends.
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
