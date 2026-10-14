# Auth pipeline: API key

## Purpose

Authenticate machine-to-machine callers with an API key instead of a user JWT. API-key verification is part of the built-in `auth` pipeline surface: the pipeline op whitelist includes `verify-api-key` (§2A.4), and §4 defines the API-key rules.

## Natural-language source example

`api/partners/_definition.md`

```
# partners
Partner integration API. Only uses provider main.
All endpoints require API key authentication.
```

`api/partners/v1/list-orders/list-orders.md` (route `GET /partners/v1/list-orders`)

```
# List partner orders
GET. Requires an API key. At most 60 requests per minute.
Query: page (integer, from 1) and page-size (integer).
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

## Defaults
- timeout: 10s
- body: 1MB
````

> ⚠️ `mode: api-key` is an **assumption**: the spec shows only `mode: jwt` (§11.3). It is the natural counterpart given the `verify-api-key` op (§2A.4), but the exact value is unspecified. The compiler would attach an `assumption` note here and emit a warning (§10.9).

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

### p2 rate-limit
- per-minute: 60

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |

## Response
| status | schema | headers |
|---|---|---|
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
|---|---|---|
| 422 | VALIDATION_FAILED | platform |
````

## Security considerations

- API keys are **stored only as hashes**, must have a **scope**, can be **revoked**, and are **never written to logs** (§4, §9).
- Same ordering and default-deny rules as JWT: `auth` runs first (`must-run: first`) and removing it requires `public: true` (§3, §4).
- After success `auth` injects `ctx.tenant_id` and `ctx.user`; logic cannot change them (§4).
- Privacy deletion revokes all API keys of the erased subject (§12.10 step 5).
- Failure → 401 (with `WWW-Authenticate`), insufficient scope/permission → 403 (§8).

## Spec gaps

- The `auth` parameter value selecting API-key mode, and parameters of the `verify-api-key` op, are **not specified**.
- How API-key **scopes** are declared in source/lock and how they map to endpoints is **not specified**.
- Where API keys are issued/managed (UI/CLI/model) is **not specified**.
- **Hash algorithm**: "only store a hash" (§4) does not name the algorithm; a keyed fast hash such as HMAC-SHA256 (high-entropy keys don't need argon2id) is a reasonable choice but **not specified**.
- **Transport**: the header name, and whether keys are accepted **only** in a header (never the query string, which leaks into logs), are not specified.
- **Tenant source**: how `ctx.tenant_id` is derived for an API key (bound at issuance?) is not specified.
- **Per-key rate limits**: only `ip` and `tenant` rate-limit keys are shown; a per-API-key key is not specified.
- **401 header**: §8 requires `WWW-Authenticate` per RFC 6750 (Bearer), which may be the wrong scheme for API-key auth.
- What `ctx.user` contains for an API-key caller is **not specified**.
