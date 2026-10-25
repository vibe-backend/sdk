# Rate limiting

Decision 188: “Anyone can …” authorizes public access without frontmatter flags.
Public endpoints remove auth and derive public-writes from their called methods;
explicit frontmatter pins take precedence. For GET and POST on the same URL,
use two endpoint files explicitly naming the same route path.

Prose limits that the lock cannot express produce a build warning quoting the
source sentence. “At most 50 per day” is not enforced by a per-minute rate limit.
For implicit memory, query SQL must use decision 183's supported unique-key
counter forms; unsupported SQL fails the build.

## Purpose

Bound request rates per IP, per tenant and per account using the built-in `rate-limit` pipeline (§4) or a custom pipeline composed from the `rate-limit-check` op (§2A.4, e.g. `login-guard` in §11.2). Limits are inherited `_definition` → `_version` → feature (§3).

## Natural-language source example

`api/users/v1/_version.md`

```text
# users v1
At most 60 requests per minute. This version is deprecated and will be retired on 2027-06-30.
```

`pipelines/login-guard.md` (custom pipeline)

```text
# login-guard
Login protection: at most 10 per minute per IP, at most 5 per minute per email.
Parameters: ip-per-minute (integer, default 10), account-per-minute (integer, default 5).
Must run before all methods, and record an audit entry (without the password).
```

Feature using both IP and tenant keys (from `submit-review.md`):

```text
At most 5 per minute per IP and at most 100 per minute per tenant.
```

## Compiled lock md example

`api/users/v1/_version.lock.md` (sections only; `scope: version`)

````markdown
## Pipelines
### p1 rate-limit
- per-minute: 60
- key: ip

## Defaults
(none)

## Lifecycle
- deprecated: true
- sunset: 2027-06-30
````

`pipelines/login-guard.lock.md`

````markdown
---
lock-version: 1
kind: pipeline
id: login-guard
source: pipelines/login-guard.md
source-hash: sha256:51bd...07
source-type: natural
dependencies: []
vibe-version: 1.0.0-rc.8
status: approved
approval-mode: manual
approved-by: [jack@example]
approved-at: 2026-10-14T08:05:00+08:00
approval-ref: pr#121
approved-commit: a1b2c121
security-items: [{ item: rate-limit-new, risk: medium, reason: "Login brute-force protection", approved-by: [jack@example] }]
signature: MEYCIQ...
---
## Parameters
| name | type | default | min | max |
|---|---|---|---|---|
| ip-per-minute | integer | 10 | 1 | 600 |
| account-per-minute | integer | 5 | 1 | 60 |

## Order
- must-run: before-steps

## Steps
### s1 rate-limit-check
- op: rate-limit-check
- key: ip
- per-minute: params.ip-per-minute
- source-lines: 2-2

### s2 rate-limit-check
- op: rate-limit-check
- key: input.email
- per-minute: params.account-per-minute
- source-lines: 2-2

### s3 audit
- op: audit
- event: login-attempt
- fields: [req.ip]
- hash: [req.body.email]
- redact: [req.body.password]
- source-lines: 4-4
````

The account key `input.email` is normalized (trim, lowercase) and then HMAC'd; the plaintext is never stored (§4.3). The audit step records only an HMAC of the email, never the plaintext (Decision 68).

Feature-level IP + tenant limits (excerpt of `## Pipelines`):

````markdown
### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant
````

## Parameter schema (§4.3, Decision 65)

| Parameter | Type | Required | Notes |
| --- | --- | --- | --- |
| `per-minute` / `per-hour` / `per-day` | positive integer | exactly one | one window per step |
| `key` | `ip` \| `tenant` \| `user` \| `api-key` \| `family` \| `input.{field}` | yes | `user` needs `auth`; `family` only with `token-refresh`; `input.{field}` is normalized then HMAC'd |
| `burst` | positive integer | no | defaults to the window limit, cannot exceed it |
| `fail` | error code | no | default `RATE_LIMITED` (429 + `Retry-After`) |

Counting uses a sliding window stored per `tenant_id`. `key: ip` uses the IP resolved from the platform's trusted-proxy settings, never client-supplied headers. An endpoint may have several `rate-limit` steps; exceeding any of them rejects the request.

`## Order` in a pipeline lock takes exactly one `must-run:` value: `first` (only `auth` or a platform pipeline replacing it), `before-steps`, `after-steps`, `last` or `any` (§4.4).

## Security considerations

- Exceeding a limit returns **429** with `Retry-After` (added by the platform) (§8).
- **Public endpoints** are always rate-limited per IP; per-tenant limits can be added. **Public writes** require both IP **and** tenant limits plus a daily quota (§7.2, §10.7).
- **Login** must be limited per IP and per account (4A.3: 10 per IP per minute) (§12.6).
- `cost: high` steps (`verify-password`, `hash-password`, large queries) on public paths automatically use half the endpoint limit (§7.2, Decision 39).
- Pipeline parameters are validated against `## Parameters`; unknown parameters or wrong types are errors (§4).
- **Relaxing** a rate limit, timeout, body size or quota is a medium-risk security item (§2A.5). Inner layers override single settings but cannot exceed platform caps (§3).
- Never log secrets; redact passwords in audit steps (§9, §11.2).

## Spec gaps

- The spec's own `_version` example (§11.3) writes `rate-limit` with only `per-minute` and no `key`, although §4.3 makes `key` required; this page adds `key: ip`.
- Platform maximums for the window limits are **not specified** (only `login-guard`'s own `## Parameters` min/max are shown).
- Whether `input.{field}` in a pipeline refers to the request body or to the called method's input is not spelled out (§11.2 uses `input.email` with a `req.body.email` request).
