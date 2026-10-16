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
2. Marks the token as used in a replay cache (SHA-256 of the token, kept for
   the 300 s token lifetime plus a margin). A token is accepted at most once, also
   under concurrent reuse. A failed token stays used.
3. Calls `https://challenges.cloudflare.com/turnstile/v0/siteverify` with the
   secret, the token and the client IP (5 s timeout, no redirects, public
   addresses only).
4. Requires `success: true`, a `hostname` in `VIBE_CHALLENGE_HOSTNAMES`, an
   `action` equal to the route's endpoint name (the last segment of the feature
   id, at most 32 characters of `[A-Za-z0-9_-]`; set it as the widget's
   `data-action`), and a `challenge_ts` that is at most 300 s old.

The replay cache is per process. With several instances, Turnstile's own
single-use check (`timeout-or-duplicate`) still applies.

## Failing closed

| Situation | Response |
| --- | --- |
| No proof | 400 `CHALLENGE_REQUIRED` |
| Proof rejected (forged, replayed, expired, wrong hostname or action) | 403 `CHALLENGE_FAILED` |
| No verifier configured, provider unreachable, timeout, bad response, `type: pow` | 503 `CHALLENGE_UNAVAILABLE` |

In production (`VIBE_ENV`), `vibe serve` refuses to start and `vibe validate --deploy`
fails if any route uses `challenge ... when: always` and no verifier is configured.
Challenges do not replace server-side rate limits and quotas.
