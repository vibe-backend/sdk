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
   (provider unavailable) is released. When the cache (100,000 entries) is
   full of live tokens, challenge routes answer 503.
3. Calls `https://challenges.cloudflare.com/turnstile/v0/siteverify` with the
   secret, the token and the client IP (5 s timeout, no redirects, public
   addresses only).
4. Requires `success: true`, a `hostname` in `VIBE_CHALLENGE_HOSTNAMES`, an
   `action` equal to the route's endpoint name (the last segment of the feature
   id, at most 32 characters of `[A-Za-z0-9_-]`; set it as the widget's
   `data-action`), and a `challenge_ts` that is present, at most 300 s old and not more
   than 60 s in the future (clock skew).

## Replay store

The single-use state (step 2) lives behind the `ReplayStore` trait
(`src/runtime/replay.rs`):

| Variable | Meaning |
| --- | --- |
| `VIBE_CHALLENGE_REPLAY_STORE` | `postgres` (shared by all instances) or `memory` (per process, development and CI only). No default outside production. |

Which store is used, when a verifier is configured:

| Environment | Unset | `postgres` | `memory` |
| --- | --- | --- | --- |
| Production | `postgres` | `postgres` | refused at startup |
| Development / CI | error at startup (choose explicitly) | `postgres` | in-memory |

- Production always uses the shared PostgreSQL store. `vibe serve` refuses to
  start if `VIBE_CHALLENGE_REPLAY_STORE=memory` is set (with or without a
  verifier), or if the table is missing or not usable by the runtime role.
- `postgres` keeps the state in `vibe_platform.challenge_replay` in the
  `VIBE_DATABASE_URL` database, so a token, the per-IP and per-tenant quotas
  and the capacity limit hold across instances. Reservations use
  `INSERT ... ON CONFLICT` (an expired row is taken over atomically), quota
  checks are serialized per IP and per tenant with transaction-level advisory
  locks, and expired rows are removed in small batches on each reservation.
  Client IPs are stored as SHA-256 hashes; tokens as SHA-256 of the token.
- `vibe migrate` always creates the schema and table (owner role, idempotent)
  and grants the runtime role `SELECT, INSERT, UPDATE, DELETE`.
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
| No verifier configured, replay cache full, replay store unavailable, provider unreachable, timeout, bad response, `type: pow` | 503 `CHALLENGE_UNAVAILABLE` |

In production (`VIBE_ENV`), `vibe serve` refuses to start and `vibe validate --deploy`
fails if any route uses `challenge ... when: always` and no verifier is configured.
Challenges do not replace server-side rate limits and quotas. The route's rate
limits and the public write quota are checked before the challenge, so a
limited request never reaches the replay cache or the provider.
