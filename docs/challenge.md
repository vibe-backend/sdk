# Challenge verification (#118)

A public route declared with `challenge: { type: captcha, when: always }` needs a
valid `x-challenge-proof` header. The header being present is **not** enough: the
server verifies the token with the configured provider on every request.

## Provider

v1 supports Cloudflare Turnstile (`ChallengeVerifier` / `TurnstileVerifier` in
`src/runtime/challenge.rs`). hCaptcha can be added later behind the same trait;
`VIBE_CHALLENGE_PROVIDER=hcaptcha` is rejected until then.

| Variable | Meaning |
| --- | --- |
| `VIBE_CHALLENGE_PROVIDER` | `turnstile` (default). |
| `VIBE_TURNSTILE_SECRET` | Turnstile secret key. Unset means "not configured". |
| `VIBE_CHALLENGE_HOSTNAMES` | Comma-separated hostnames the widget is served from. Required with the secret. |

## Checks

For each request the server:

1. Rejects an empty or oversized token (more than 2048 characters).
2. Reserves the token in a replay cache (SHA-256 of the token) before asking
   the provider, so a token is accepted at most once, also under concurrent
   reuse. Each client IP may hold 16 and each tenant 1000 unverified
   reservations at a time; beyond that the request is rejected without calling
   the provider. A verified token is kept for the 300 s token lifetime plus a
   margin; a rejected token is kept for 10 s; a reservation without an answer
   (provider unavailable) is released; a pending reservation whose request was
   interrupted is held for 30 s. When the cache (100,000 entries) is
   full of live tokens, the spec (decision 134) answers 429 with `Retry-After`;
   the current code still answers 503 (not yet implemented, follow-up issue #204).
3. Calls `https://challenges.cloudflare.com/turnstile/v0/siteverify` with the
   secret, the token and the client IP (5 s timeout, no redirects, public
   addresses only).
4. Requires `success: true`, a `hostname` in `VIBE_CHALLENGE_HOSTNAMES`, an
   `action` equal to the route's widget action (set it as the widget's
   `data-action`), and a `challenge_ts` that is present, at most 300 s old and not more
   than 60 s in the future (clock skew).

   The widget action is derived from the whole feature id (#159), so
   endpoints that share a last segment get different actions: `/` becomes
   `_` (`shop/v1/register` → `shop_v1_register`). If the result is longer
   than 32 characters or contains a character outside `[A-Za-z0-9_-]`
   (replaced by `-`), the action is the first 23 sanitized characters, `-`,
   and the first 8 hex digits of SHA-256 of the feature id. `vibe serve`
   refuses to start, and `vibe validate --deploy` fails, if two challenge
   routes would still share an action.

## Replay store

The single-use state (step 2) lives behind the `ReplayStore` trait
(`src/runtime/replay.rs`):

| Variable | Meaning |
| --- | --- |
| `VIBE_CHALLENGE_REPLAY_STORE` | `postgres` (per tenant, shared by all instances) or `memory` (per process, development and CI only). No default outside production. |

Which store is used, when a verifier is configured:

| Environment | Unset | `postgres` | `memory` |
| --- | --- | --- | --- |
| Production | `postgres` | `postgres` | refused at startup |
| Development / CI | error at startup (choose explicitly) | `postgres` | in-memory |

- Production always uses the PostgreSQL store. `vibe serve` refuses to
  start if `VIBE_CHALLENGE_REPLAY_STORE=memory` is set (with or without a
  verifier), or if a configured tenant's table is missing, not usable by the
  runtime role, or grants it DELETE, UPDATE or TRUNCATE.
- `postgres` keeps the state per tenant in `t_<tenant>.vibe_challenge_replay`
  (decision 97: no table shared between tenants). The table has `tenant_id`
  under ENABLE + FORCE row level security with the standard
  `tenant_isolation` policy, is owned by the owner role and is created by that
  tenant's `vibe migrate`. Every instance shares it, so a token, the per-IP
  and per-tenant quotas and the per-tenant capacity limit hold across
  instances; one tenant cannot use up another tenant's capacity.
- The runtime role has only `SELECT, INSERT` on it (decision 27). The table is
  an append-only log: a reservation inserts a `pending` row with a random
  nonce; settling it inserts a `verified`, `rejected` or `released` row for
  that nonce. A `released` row only cancels its own `pending` row, so a
  verified token stays used for its whole TTL. Reads ignore expired rows;
  expired rows are deleted only by the owner role when `vibe migrate` runs.
  Checks are serialized per tenant with a transaction-level advisory lock.
- Client IPs are stored as HMAC-SHA256 under a per-tenant key derived from
  `VIBE_API_KEY_PEPPER`; tokens as SHA-256 of the token.
- `memory` is an explicit opt-in for a single development or CI process. It
  is the only choice with `--memory`.
- Any store error (database unreachable, timeout, missing table) answers
  503 `CHALLENGE_UNAVAILABLE`. If a verified token cannot be recorded, it is
  not accepted.
- `vibe validate` applies the same rules to the variables (the table is
  checked by `vibe serve`).

Turnstile's own single-use check (`timeout-or-duplicate`) applies in addition.

## Failing closed

| Situation | Response |
| --- | --- |
| No proof | 400 `CHALLENGE_REQUIRED` |
| Proof rejected (forged, replayed, expired, missing or future `challenge_ts`, wrong hostname or action, per-IP or per-tenant quota reached) | 403 `CHALLENGE_FAILED` |
| Provider or network failure only: provider unreachable, timeout, bad response, replay store unreachable (spec decision 134) | 503 `CHALLENGE_UNAVAILABLE`, `Retry-After: 5` (decision 135) |
| Per-network-prefix cap reached (IPv4 /24 or IPv6 /48, default 16 per tenant replay store; decision 134) | 429 `CHALLENGE_RATE_LIMITED` with `Retry-After` = remaining validity of the tenant's oldest record in that prefix, capped at 360 s, or a fixed 60 if it cannot be computed (same rule as the full store; PM-approved 2026-10-16); identical to the full-store response apart from `Retry-After` |
| Replay store full (capacity limit from traffic, not a provider failure; decision 134, PM-confirmed 2026-10-16) | 429 `CHALLENGE_RATE_LIMITED`, `Retry-After` = remaining validity of the tenant's own oldest record, capped at 360 s, or 60 s if it cannot be computed |

A full replay store still fails closed: a request is never admitted without recording its replay entry. Each occurrence emits the metric `vibe_challenge_rejected_total{reason="replay_store_full"}` and raises the alert `challenge_replay_store_full`. Names confirmed by the PM on 2026-10-16.

The full-store and per-prefix-cap cases use the same error code, `CHALLENGE_RATE_LIMITED`. The reason (`replay_store_full`, `replay_prefix_cap`, and so on) is recorded only in the metric and internal logs and never appears in the HTTP response (body, headers or error detail). All these 429 responses are identical apart from `Retry-After`.

Implementation status: not yet aligned with spec decisions 131, 134 and 135 (follow-up issues #202, #203, #204):

- The 503 has no `Retry-After` header yet.
- The current code also returns 503 when no verifier is configured and for a `type: pow` route; under the spec these are rejected at build or startup (decisions 131, 134).
- A full replay cache or store returns 503 today; the spec requires 429 with the computed `Retry-After`, the `replay_store_full` metric and the alert (decision 134). Not yet implemented, follow-up issue #204.
- Per-network-prefix cap (decision 134): in each tenant's replay store, each IPv4 /24 or IPv6 /48 may hold at most 16 entries by default (matching decision 137: 16 unverified reservations per IP); over the cap the request gets 429 `CHALLENGE_RATE_LIMITED` (internal reason `replay_prefix_cap`). The alert `challenge_replay_store_high_usage` fires when replay store utilization exceeds 80%; the alert `challenge_replay_store_full` fires when the store is full (metric reason `replay_store_full`). Not yet implemented, follow-up issue #204.

In production (`VIBE_ENV`), `vibe serve` refuses to start and `vibe validate --deploy`
fails if any route uses `challenge ... when: always` and no verifier is configured.

Only public routes with `when: always` are verified. A challenge declared with
`when: after-failures` (for example on login routes) or on a non-public
(authenticated) route is not verified yet. Such challenges are temporarily
unsupported in production: `vibe serve` and `vibe validate --deploy` refuse
these routes even when a verifier is configured (#199), until verified
challenges on these routes are implemented (#201). Development and CI still accept them. Runtime verification for those
modes is tracked in #201.

Challenges do not replace server-side rate limits and quotas. The route's rate
limits and the public write quota are checked before the challenge, so a
limited request never reaches the replay cache or the provider.

## Request binding (#156, decision 140)

Spec v1.8 §7.2b binds each token to a digest of the request it was solved for.
The digest goes in the widget's `cdata` (`data-cdata`). Binding applies to every
captcha route, including login routes with `when: after-failures`. Implementation
status: not yet implemented.

### Request nonce

Every bound request must carry a fresh nonce in the `X-Vibe-Request-Nonce` header:

- 16 random bytes from a CSPRNG (`crypto.getRandomValues`), never `Math.random`.
- Encoded as base64url with no padding: exactly 22 characters.
- Single use: never reuse a nonce, including on a retry. A retry needs a new
  nonce and a new challenge.
- Send the nonce only in the `X-Vibe-Request-Nonce` header to the vibe server.
  The protocol puts only the digest in `cdata`, never the nonce, so `cdata` alone
  can't be used to recover low-entropy bodies (such as login forms).
- Nonces are unique per tenant (key `(tenant_id, nonce)`). Each request uses one
  nonce record, which counts toward the replay store caps (decisions 134 and
  137).

The server checks the nonce in a fixed order and stops at the first failure:

1. Format and canonical form: 403 `CHALLENGE_FAILED`.
2. Reuse of the same nonce: 403 `CHALLENGE_FAILED`.
3. Decision 137 unverified-reservation cap: 403 `CHALLENGE_FAILED`.
4. Decision 134 capacity caps (store full, per-prefix cap): 429
   `CHALLENGE_RATE_LIMITED` with `Retry-After`. This response is identical to
   every other decision 134 429, apart from the `Retry-After` value.
   `Retry-After` is the remaining lifetime of the tenant's own oldest record
   (store full) or of the tenant's oldest record in that prefix (per-prefix
   cap), at most 360 seconds, and 60 seconds if it can't be computed.

Every verification failure, including a reused nonce, is 403, never 429. On 429,
wait `Retry-After` seconds, then retry with a new nonce and a new challenge.

### Frontend: run the challenge just before submit

The digest covers the nonce, method, path, query, `Content-Type`, `If-Match` and
the exact body bytes, so compute it only after the form is complete:

1. Let the user fill in the whole form. Do not run the challenge on page load.
2. On submit, serialize the body once (for example `JSON.stringify`) and keep
   those exact bytes.
3. Build the URL with the client helper, which encodes the query for you. A space
   becomes `%20` and a literal `+` becomes `%2B`. A raw `+` in the query is
   rejected, so do not build query strings by hand or with
   `URLSearchParams.toString()` (it writes spaces as `+`).
4. Generate the nonce. Then compute the digest (`rd1.` + base64url SHA-256, see
   `scripts/request-digest-vectors.mjs`) and render or execute the widget with
   that `cdata`.
5. Send the request with exactly the same method (upper case), URL, headers and
   body bytes, plus `x-challenge-proof` and `X-Vibe-Request-Nonce`.
6. If the user edits the form after the challenge, call `turnstile.reset` and run
   the challenge again with a new nonce and digest.

Any invalid input is rejected with the same 403 `CHALLENGE_FAILED`, and the
response never says which part was wrong. That includes a raw `+`, a lower-case
method, a duplicate header, a non-ASCII header value, `Content-Encoding`, dot
segments and bad percent-encoding.

A proxy or CDN in front of the runtime must not rewrite the path, query or body,
or add `Content-Encoding`. Otherwise every bound request is rejected (fail
closed). `bind: none` is allowed in v1 with a warning. From the next minor
release, new public INSERT routes may not use it (#245).
