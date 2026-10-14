# Refresh token rotation

## Purpose

Keep access tokens short-lived (≤ 1h) while letting clients stay signed in, using **rotating refresh tokens** with reuse detection (Decision 6, §4A.1). The refresh flow is implemented by the platform's **built-in `token-refresh` pipeline**, which uses the `method` op to call the `capabilities: [sign-token]` model method (e.g. `user.sign-token`) to issue the new access token.

## Natural-language source example

`api/auth/v1/refresh/refresh.md` (route `POST /auth/v1/{slug}/refresh`)

```text
# Refresh token
POST, path /auth/v1/{slug}/refresh.
Public, no auth (the caller only holds a refresh token), tenant is determined by slug.
Use the built-in token-refresh pipeline, signing new access tokens with user.sign-token.
Each IP at most 30 requests per minute.
Request body: refresh_token (string, required).
Return 200 with the new access token and new refresh token.
An invalid, expired, revoked or reused refresh token returns 401 INVALID_CREDENTIALS.
```

## Compiled lock md example

`api/auth/v1/refresh/refresh.lock.md` (sections only; frontmatter per §2A.1, `kind: api`, `scope: feature`; `dependencies` include `token-refresh` (pipeline) and `user/methods/sign-token`; security items `public` and `auth-removed`, high risk)

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/refresh

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: false
- role: public_reader
- external-calls: forbidden

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 30
- key: ip

### p3 token-refresh
- sign-method: user.sign-token

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.refresh_token | string | | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { access_token, refresh_token } | |

## Transform
(none)

## Steps
(none)

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | token-refresh |
| 422 | VALIDATION_FAILED | platform |
````

> ⚠️ The `token-refresh` pipeline is platform-provided and its `## Parameters` schema is **not published in the spec**. `sign-method`, the response field names, `public-writes: false`, and an empty `## Steps` are **assumptions** for illustration. `INVALID_CREDENTIALS` in `## Errors` is a **self-defined error code** chosen for this example (reusing the login code); the spec defines no error codes for `token-refresh`. See Spec gaps.

## Security considerations

Platform-enforced rules (§4A.1, Decision 6):

- **Access token**: JWT, ttl ≤ **1 hour**.
- **Refresh token**: opaque random value of at least **256 bits**; only its **hash** is stored in the DB.
- **Lifetime**: absolute max **30 days** from the family's first issuance; expires after **7 days idle**.
- **Rotation on every use**: a new refresh token and a new access token are issued and the old refresh token is invalidated immediately.
- **Families**: all refresh tokens from one login share a `family_id`. Reuse of an invalidated token **revokes the whole family**, writes an audit record and notifies the user.
- **Retry grace**: reusing the same old token within **10 seconds** after rotation returns the same new token pair and is not treated as reuse.
- **Logout** revokes the current family; "log out of all devices" revokes all of the user's families.
- Privacy deletion revokes all of the subject's refresh-token families (§12.10).
- Pipelines cannot run `query` or `call` directly; they reach models only via `method` (§4).
- Tokens are never logged (§9).
- **Client storage** (general guidance, outside the spec): browsers should keep refresh tokens in an `HttpOnly`, `Secure`, `SameSite` cookie; native apps in the OS keychain/keystore (iOS Keychain, Android Keystore). Never store them in `localStorage`/`sessionStorage`. The spec only shows a body field `refresh_token`; cookie transport is not specified. Signing goes only through the `sign-token` capability, with claim whitelist and ttl cap (§12.6) — see [sign-token.md](sign-token.md).

## Spec gaps

- The `token-refresh` pipeline's **parameters, response shape, and error codes** are not specified.
- How the **first refresh token is issued at login** (and which component creates the family) is not specified; the §12.8 `user.login` example returns only an access token.
- Where refresh tokens are **stored** (platform store vs. a model table) is not specified; pipelines cannot `query`, and no op exists for generating opaque random tokens.
- **Logout / logout-all endpoints**: required behavior is defined, but no pipeline, op or method for revocation is specified.
- Whether the refresh endpoint counts as a public write (it mutates token state) and whether it needs `public-writes: true`, quotas or challenge is **not specified**.
- Whether a refresh endpoint is "login-class" (500ms) or general public (300ms) is not specified; by the §7.2 definition (has a `verify-password` step) it would be 300ms.
- **10-second grace vs. hash-only storage**: §4A.1 requires returning *the same* new token pair when the old token is reused within 10s, but also that only the hash of refresh tokens is stored. Returning the same plaintext refresh token again requires retaining it (or a reversible form) for 10s, which contradicts hash-only storage (spec owner should resolve).
- Cookie-based transport of the refresh token is not specified.
- What a feature lock's `## Steps` contains when a pipeline produces the response is not specified.
