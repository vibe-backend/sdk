# Public writes

Decision 188: “Anyone can …” authorizes public access without frontmatter flags.
Public endpoints remove auth and derive public-writes from their called methods;
explicit frontmatter pins take precedence. For GET and POST on the same URL,
use two endpoint files explicitly naming the same route path.

Prose limits that the lock cannot express produce a build warning quoting the
source sentence. “At most 50 per day” is not enforced by a per-minute rate limit.
For implicit memory, query SQL must use decision 183's supported unique-key
counter forms; unsupported SQL fails the build.
A source naming a single thing, such as “the counter”, gets a constant key.

## Purpose

Allow unauthenticated visitors to create or update data (e.g. submitting a review) under tightly controlled conditions. Public endpoints may call `writes: true` methods (Decision 4), but every rule in §7.2 is mandatory (Decisions 21–26, 38, 39, 41, 47, 49, 53).

## Natural-language source example

`api/catalog/v1/submit-review.md` (route `POST /catalog/v1/{slug}/reviews`)

```text
# Submit review
POST, path /catalog/v1/{slug}/reviews, public, no auth, tenant is determined by slug.
Visitors may leave reviews, so this writes data and requires captcha.
At most 5 per minute per IP and at most 100 per minute per tenant.
body: item_id (uuid), rating (1 to 5), comment (1 to 500 characters).
Create the review with review.submit; on success return 201 with id. If the item does not exist return 404 NOT_FOUND.
```

## Compiled lock md example

`api/catalog/v1/submit-review.lock.md` (sections only; frontmatter per §2A.1 with `kind: api`, `scope: feature`; security items on **this** lock: `public`, `auth-removed`, `public-writes` (high risk) and `output-new-field` for the new response field (medium risk)). The `write-sql` item (medium) for the new INSERT belongs to the `review.submit` **method** lock, not this api lock. The cap of 5 security items applies to **all locks in the same PR together** (Decision 34), so this api lock plus `review.submit` total 5 items. The `review` model's public-writable / `untrusted` fields are a further security item and must be merged and deployed in an **earlier PR**.

````markdown
## Route
- method: POST
- path: /catalog/v1/{slug}/reviews

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { reviews: [item_id, rating, comment] }
- challenge: { type: captcha, when: always }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant

### p4 audit-log
- event: public-write

## Limits
- provider: main
- timeout: 10s
- body: 16KB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.item_id | uuid | | yes |
| body.rating | integer | min: 1, max: 5 | yes |
| body.comment | string | min: 1, max: 500, untrusted | yes |

## Response
| status | schema | headers |
|---|---|---|
| 201 | { id } | Location |

## Transform
- in: { item_id: req.body.item_id, rating: req.body.rating, comment: req.body.comment }
- out: { id: r.id }

## Steps
### s1 method
- op: method
- target: review.submit
- args: transform.in
- out: r
- source-lines: 6-6

### s2 respond
- op: respond
- status: 201
- value: transform.out
- location: /catalog/v1/{req.path.slug}/reviews/{r.id}
- source-lines: 6-6

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | NOT_FOUND |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

`review.submit` checks the item via `item.get` (cross-object reads/writes go through that object's own method) and runs one parameterized `INSERT INTO reviews (item_id, rating, comment) VALUES ($1, $2, $3)`; `comment` is marked `untrusted: true, max: 500` in the `review` model (§11.7).

## Security considerations

- **Two switches**: the tenant must enable `public_access` **and** `public_write_access`. `public_write_access` can only be enabled by a tenant admin in the platform UI — no API, CLI or source (Decisions 26, 41). A tenant that has not enabled `public_access`, and a tenant that has `public_access` but not `public_write_access`, both get the **identical 404 body**, padded to the same fixed minimum (300ms), so callers cannot tell which switch is off (§7.2).
- **Per-endpoint opt-in**: `public-writes: true` in `## Access` (high risk) (§7.2).
- **Role and grants**: separate `public_writer` role; grants generated from the lock, limited to the exact tables, **columns** and operations used (§7.2, §10.7).
- **Operations**: only `INSERT` and `UPDATE`, only on `writable-columns`. `UPDATE` must have `WHERE` with primary key or RLS (§7.2).
- **Blacklist**: never writable publicly: `role`, `roles`, `permissions`, `is_admin`, `tenant_id`, `owner`, `owner_id`, `created_by`, `approved`, `verified`, `password` (only the registration rule below allows it), `deleted_at`, all system fields; objects may add more (§7.2).
- **Password writes** (Decision 47): a public path may write a password column only if all three hold: the method has `capabilities: [set-password]` (high risk), the written value is the output of `hash-password` in the same method, and the api lock has `password-write: allow` with the password column listed in `writable-columns`. Public registration itself never writes a password in v1.7; it is set at the verification step (Decision 67, see [user-register.md](user-register.md)).
- **Email verification writes** (Decision 53): `email_verified_at` (a "verified"-class blacklisted column) may only be written by a method with `capabilities: [confirm-verification]`, in a single UPDATE whose WHERE matches the `hash-token` output against the unique token-hash column, checks expiry and unverified state, and clears the hash; the api needs `verification-write: allow` (high risk).
- **Tenant injection**: `tenant_id` comes from the slug; a body containing `tenant_id`, `id` or `created_by` is rejected with 422, and declaring them in `## Request` is an error (Decision 23, §10.7).
- **Quota**: default 1,000 public writes per tenant per day and 50 per IP per day; tenant admins may lower, raising up to 10,000 needs a platform request. When exhausted only public writes are blocked (429 + `Retry-After`) (Decision 39, Decision 21).
- **Rate limits**: IP + tenant both mandatory; `cost: high` steps get half the endpoint limit (§7.2).
- **Bot protection** (Decision 49): `challenge: { type: none | captcha | pow, when: always | after-failures, after-failures: <n> }`. Public endpoints with INSERT must use `type: captcha` or `pow` with `when: always`; login-class endpoints must use `{ type: captcha, when: after-failures, after-failures: 3 }`; other public writes without INSERT may use `type: none`. A password write without INSERT (e.g. verify-email) does not need captcha but still needs IP + tenant limits (§7.2).
- **Audit**: `audit-log` cannot be removed from public-write endpoints (§10.3).
- **Untrusted content**: public-written fields are `untrusted: true` in the model; all outputs (including authenticated reads) are output-encoded per Content-Type; every field needs a length cap and arrays a count cap (Decision 24).
- **External calls**: forbidden on public paths unless `external-calls: allow` (high risk) (§7.2).
- **Fixed min response**: 300ms for general public endpoints, applied to the 404 responses above as well, so timing does not reveal tenant or switch state (§0.1, §7.2, Decision 36).
- Deploy report lists all public-write endpoints and writable columns, with a warning (§7.2, §10.9).
- Residual risk (captcha bypass, spam) is to be handled by a future content-moderation hook (§14).

## Spec gaps

- `daily-write-quota: platform-default` is the only quota syntax shown; how a lowered tenant quota appears (if at all) in the lock is not specified (quota is set in the platform UI).
- `untrusted` appears as a `## Request` rule here and as `untrusted: true` in the model; the model lock syntax for it is not shown.
- How objects declare additional blacklist fields is not specified.
- `challenge: { type: pow }` parameters (difficulty) are not specified.
- The model-level `public_writer` grant is the union over all public endpoints while each api gets only its own columns (§12.8 comment); how the platform materializes per-endpoint grants for one role is not described.
