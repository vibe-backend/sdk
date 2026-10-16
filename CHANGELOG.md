# Changelog

All notable changes to this project are documented in this file. Entries are
added only when the corresponding PR is merged to `main`.

## Unreleased

### Breaking

- **Challenge verification (#118):** `x-challenge-proof` is no longer accepted just because it is non-empty. Routes with `challenge: { type: captcha, when: always }` verify the token with Cloudflare Turnstile siteverify (hostname and action checked, each token single-use) and fail closed (403 `CHALLENGE_FAILED`, 503 `CHALLENGE_UNAVAILABLE` when no verifier is configured, the provider is unreachable, or the route uses `pow`). In production, `vibe serve` and `vibe validate --deploy` refuse such routes until `VIBE_TURNSTILE_SECRET` and `VIBE_CHALLENGE_HOSTNAMES` are set.
- **Ownership (#53):** The implicit admin ownership bypass is removed. Models that need admin-wide access must declare `ownership: role(admin)`. `owner-key: id` is only allowed on user-like models.
- **Environment / slugs / pepper (#52):** Unknown `VIBE_ENV` values are treated as production. With PostgreSQL, unset `VIBE_ENV` is production. `--memory` is refused outside development. Outside development, `VIBE_API_KEY_PEPPER` is required (≥ 32 bytes). Missing, empty, or malformed `.vibe/slugs.json` fails closed (only `--memory` falls back to `slugs.example.json`).
- **Manifest path (#62):** `vibe manifest` without `-o` writes `./manifest.json` in the current working directory, not `<project>/manifest.json`.
- **Rate limits (#54):** Rate-limit keys follow the spec: a limit keyed by `account` or `input.<field>` must be paired with an `ip` limit on the same route, `family` is rejected until token refresh lands (#26), and only `per-minute` limits are accepted (1–10000). Behind a proxy, the client IP comes from forwarded headers only when the peer is a trusted proxy. `key: account` now means per-email (`input.email`), not per-tenant.
- **Request validation (#57), possibly breaking:** `vibe validate` rejects `## Request` fields whose type is not `string`, `integer`, `number`, `boolean` or `uuid`. At runtime, requests accepted before can now get 422: non-canonical uuids, query booleans other than `true`/`false`, empty required headers, non-object JSON bodies, and badly typed path params.

### Changed

- **LLM errors (#62):** Failed LLM calls report the real HTTP status (only 401/403 blame the API key; 400, 404, 429 and 5xx are worded by status). HTML responses and 404s add a hint to check `LLM_BASE_URL` (OpenAI-compatible endpoints usually end in `/v1`). The message includes the request URL and the first 200 characters of the response; the API key and `key=`/`api_key=` query values are masked as `***` before truncation.
- **Rate limits (#54):** `VIBE_TRUSTED_PROXIES` accepts proxy IPs, CIDR blocks (v4/v6), hops with ports and IPv4-mapped IPv6; `X-Forwarded-For` is used only when the peer is trusted, taking the rightmost untrusted hop. IPv6 clients are bucketed per /64. `Retry-After` is the real time until the oldest hit leaves the window (quota: seconds until UTC midnight). Buckets are held in bounded, per-route/key-type LRU shards (10k each); at-limit buckets are never evicted, and a flood of one key type cannot block other clients.
- **Request handling (#57):** Authentication, role/scope checks and rate limiting now run before request validation, so unauthenticated callers get 401/403 instead of 422 details. Expression failures return a generic `422 Invalid request` (details go to the server log). Required headers are enforced case-insensitively; body, query and path values are type-checked. Early 400, 413 (payload too large) and 415 (unsupported media type) responses respect the route's `min_response_ms`.

<!--
Pending — add each entry only when its PR merges:
- #51: migrate requires VIBE_MIGRATE_DATABASE_URL and VIBE_RUNTIME_DB_ROLE; no longer reads VIBE_DATABASE_URL.
- #61: stricter lock validation — unknown step keys and unparseable expressions are errors;
  API validate steps need `when` and `fail` and now actually execute; write SQL needs
  `writes: true` and `writes: true` needs a write; SELECT INTO counts as a write and as DDL
  outside migrations; unparseable or inverted min/max rules are errors.
-->
