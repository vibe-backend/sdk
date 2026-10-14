# Rate limiting

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
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
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
- after: [auth]

## Steps
### s1 rate-limit-check
- op: rate-limit-check
- key: req.ip
- per-minute: params.ip-per-minute
- source-lines: 2-2

### s2 rate-limit-check
- op: rate-limit-check
- key: req.body.email
- per-minute: params.account-per-minute
- source-lines: 2-2

### s3 audit
- op: audit
- event: login-attempt
- fields: [req.ip, req.body.email]
- redact: [req.body.password]
- source-lines: 4-4
````

> ⚠️ **Privacy:** this spec example (§11.2) audits `req.body.email` in plain form. §12.10 says personal data must never be logged and only pseudonymized ids may be kept. Audit records are not explicitly the same as logs, but treat this as a conflict: prefer not auditing raw email until the spec clarifies (see Spec gaps).
>
> ⚠️ **Key normalization:** an account key like `req.body.email` should be normalized (e.g. trimmed, lower-cased) before counting, or attackers bypass the per-account limit with case variants. The spec does not define normalization in `rate-limit-check`.

Feature-level IP + tenant limits (excerpt of `## Pipelines`):

````markdown
### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant
````

## Security considerations

- Exceeding a limit returns **429** with `Retry-After` (added by the platform) (§8).
- **Public endpoints** are always rate-limited per IP; per-tenant limits can be added. **Public writes** require both IP **and** tenant limits plus a daily quota (§7.2, §10.7).
- **Login** must be limited per IP and per account (4A.3: 10 per IP per minute) (§12.6).
- `cost: high` steps (`verify-password`, `hash-password`, large queries) on public paths automatically use half the endpoint limit (§7.2, Decision 39).
- Pipeline parameters are validated against `## Parameters`; unknown parameters or wrong types are errors (§4).
- **Relaxing** a rate limit, timeout, body size or quota is a medium-risk security item (§2A.5). Inner layers override single settings but cannot exceed platform caps (§3).
- Never log secrets; redact passwords in audit steps (§9, §11.2).

## Spec gaps

- The built-in `rate-limit` pipeline's full parameter schema is not published; only `per-minute` and `key` (`ip`, `tenant`) appear in examples. Per-second/per-hour windows and burst settings are **not specified**.
- The default `key` when omitted (e.g. `_version` above has only `per-minute`) is **not specified**.
- `login-guard` declares `must-run: before-steps` while `auth` uses `must-run: first`; the full set of `## Order` values is not enumerated.
- Platform maximums for rate limits are not specified.
- **Privacy conflict**: §11.2 `login-guard` audits `req.body.email` (personal data, `personal: standard` per §12.10) while §12.10 forbids personal data in logs and requires pseudonymized ids in erasure audit. Whether `audit` records count as logs, and whether `audit` supports pseudonymizing a field, is **not specified**; the spec owner should resolve.
- Normalization of `rate-limit-check` keys (case, whitespace, Unicode) is **not specified**.
