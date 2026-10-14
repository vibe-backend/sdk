# Auth pipeline: API key

## Purpose

Authenticate machine-to-machine callers with an API key instead of a user JWT. API-key mode is part of the built-in `auth` pipeline: `auth: { mode: api-key, scopes: [...] }` (§4.2, Decision 43), verified by the `verify-api-key` pipeline op (§2A.4).

## Natural-language source example

`api/partners/_definition.md`

```text
# partners
Partner integration API. Only uses provider main.
All endpoints require an API key with partners:read scope.
```

`api/partners/v1/list-orders/list-orders.md` (route `GET /partners/v1/list-orders`)

```text
# List partner orders
GET. Requires an API key with partners:read scope. At most 60 requests per minute per API key.
Query: page (integer, from 1) and page-size (integer, 1 to 100).
List orders with order.list and return 200 with items, page, page_size and has_more.
```

## Compiled lock md example

`api/partners/_definition.lock.md` (sections only; `kind: api`, `scope: definition`)

````markdown
## Providers
- allowlist: [main]
- default: main

## Pipelines
### p1 auth
- mode: api-key
- scopes: [partners:read]

## Defaults
- timeout: 10s
- body: 1MB
````

`api/partners/v1/list-orders/list-orders.lock.md` (sections only; `kind: api`, `scope: feature`)

````markdown
## Route
- method: GET
- path: /partners/v1/list-orders

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: api-key
- scopes: [partners:read]

### p2 rate-limit
- per-minute: 60
- key: api-key

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { items, page, page_size, has_more } | |

## Transform
- in: { page: req.query.page, page-size: req.query.page-size }
- out: { items: r.items, page: r.page, page_size: r.page_size, has_more: r.has_more }

## Steps
### s1 method
- op: method
- target: order.list
- args: transform.in
- out: r
- source-lines: 4-4

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 4-4

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | platform |
````

## Security considerations

Platform rules (§4.2, Decision 43):

- **Transport**: header `X-API-Key: <key>` only. A key in the query string or body is rejected with 400 and never logged.
- **Format**: `mbk_{env}_{key_id}_{secret}`; `env` is `live` or `test`; `key_id` is 12 lowercase base32 characters; `secret` is 32 random bytes in base62. `test` keys cannot be used in the live environment.
- **Scopes**: `{definition}:{read|write|admin}` or `{definition}:*`; `admin` implies `write`, `write` implies `read`; a global `*` is not allowed. Changing API-key scopes is a **high-risk** security item (§2A.5).
- **Storage**: only `HMAC-SHA-256(pepper, secret)` is stored (pepper in platform KMS); lookup by `key_id`, then constant-time comparison.
- **Lifetime and rotation**: default 90 days, maximum 365; at most 2 active keys per client; the old key overlaps the new one for 7 days by default (max 30), then expires.
- **Revoke**: platform UI/CLI `api-key revoke {key_id}`; effective on all instances within 30 seconds; recorded by UPDATE of `revoked_at`, never DELETE.
- `last_used_at` is recorded; keys and secrets are never logged.
- **Context**: `ctx.tenant_id` comes from the tenant the key is bound to; `ctx.user` is `{ type: api-key, key_id, scopes }` (§4.2).
- **Rate limiting** per key uses `key: api-key` (§4.3).
- Privacy deletion revokes all API keys of the erased subject (§12.10 step 5).
- Missing/invalid key → 401; insufficient scope → 403 (§8).

## Spec gaps

- §8 requires `WWW-Authenticate` per RFC 6750 (a Bearer scheme) on every 401, which does not obviously fit API-key auth; the challenge value for API-key mode is **not specified**.
- How API keys are issued (UI/CLI command names other than `revoke`) is **not specified**.
- Whether `scopes` declared in `_definition` and in a feature are merged or overridden by inheritance (§3) is **not specified**.
