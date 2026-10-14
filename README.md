# vibe-backend SDK

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The HTTP runtime loads that manifest. It does not interpret natural language.

The specification is [docs/spec-v1.7.md](docs/spec-v1.7.md). The draft [docs/spec-v1.8.md](docs/spec-v1.8.md) is under review.

## v1.8 設計（實作中）

The commands below are the v1.8 design and are not implemented yet. Until then, use the steps in the next sections.

```bash
vibe dev
curl http://localhost:8080/catalog/v1/acme/items
```

`vibe dev` will use an in-memory database for development and testing only; all data is lost when the server stops.

**Method ops (#26, part 1).** Method steps `query`, `verify-password`, `hash-password`, `random-token`, `hash-token` and `sign-jwt` now run. `query` runs the lock's parameterised SQL in the tenant schema under row level security, as `public_reader` (or `public_writer` for a step marked `writes: true`, via `SET LOCAL ROLE`; both database roles must exist and be granted to the runtime role), in a READ ONLY transaction unless the step writes. Lock SQL is checked with a PostgreSQL parser by `vibe validate` and again at runtime: exactly one SELECT, INSERT, UPDATE or DELETE; unqualified plain table names (no `pg_*`, `information_schema` or platform tables); no table functions; no `SELECT … INTO`; no casts to object-identifier types (`regclass`, `regproc`, …); only allowlisted functions (`src/sqlguard.rs`), so no `SET`, settings or dynamic SQL. A data-modifying `WITH` is rejected, and a unique violation (SQLSTATE 23505) returns 409 `CONFLICT`. `query` is PostgreSQL only: with `--memory`, `vibe serve` warns at startup and the step fails with "query steps need PostgreSQL; the --memory executor does not run them" (500). `verify-password` uses argon2id and only accepts hashes made with the platform's own parameters (m=65536, t=3, p=1) and passwords of at most 256 characters (at most four hashes at a time per instance; a request that waits more than 5 s gets 503). When the user does not exist (`missing-user: dummy-hash`) it compares against a dummy hash, built at startup, so timing and the result look the same as a wrong password. Endpoints that reach a `verify-password` step are padded to at least 500 ms. `validate` conditions are `X is empty`, `X is not empty`, `X == true|false|null` or `X != true|false|null` (code-span backticks are allowed); anything else is a validate error and, at runtime, fails closed (the step's `fail` fires), never a silent pass. Step expressions can read `ctx.user.sub`, `ctx.user.roles` and `ctx.user.tenant`. `random-token` returns `{value, hash}` for 32 random bytes; `hash` (and `hash-token`) is HMAC-SHA-256 under a key derived from `VIBE_API_KEY_PEPPER`, as a `bytea` literal (`\x…`) for a `bytea` column (`normalize: email` trims and lowercases first). `sign-jwt` runs only in methods with `capabilities: [sign-token]`. It reads the private PEM from the environment variable named by `key-ref` (`<key-ref>_KID` sets the kid, default `VIBE_JWT_KID`), and `vibe serve` refuses to start if that variable is missing. The runtime sets `iss`, `aud`, `iat`/`nbf`/`exp` (ttl at most 1 h), `jti`, `tenant_id` (always the request's tenant) and the roles claim, which it reads from `users.roles` for the token's `sub`; a lock cannot supply roles. `sub` must be a field of a row read by an earlier `query` step (for example `u.id`), checked by `vibe validate` and at runtime. Other claims must be listed in `custom-claims` and can never be a reserved claim or any configured roles claim. The step returns `{access_token, token_type, expires_in, expires_at}`. `issue-refresh: platform-decided` (or `new-family`) adds a `refresh_token`; it needs a route with a `token-refresh` pipeline whose `sign-method` is that method, otherwise the step errors.

**Refresh tokens (#26, part 2).** A route with a `### token-refresh` pipeline (`sign-method`, `refresh-ttl` ≤ 30d, `idle-ttl` ≤ 7d, `reuse-grace` default 10s, `transport: body`) must be public with auth removed. It takes `{refresh_token}` and returns `{access_token, token_type, expires_in, expires_at, refresh_token, refresh_expires_in}`. Refresh token expiry: each token expires `idle-ttl` (default and max 7d) after it was issued unless it is used, and a family ends `refresh-ttl` (default and max 30d) after login. `refresh_expires_in` is the seconds until the returned token expires if unused: the smaller of `idle-ttl` and what is left of the family lifetime. A `sign-jwt` step with `issue-refresh` returns `refresh_expires_in` on login too. The compiler-added `source-lines` bullet is ignored; any other unknown setting is an error. `vibe validate` (also with `--deploy`) and the manifest builder accept `issue-refresh: none | platform-decided | new-family`, and reject a `sign-method` that does not name a method with a `sign-jwt` step. Refresh routes run before request-table validation, so a missing or malformed token is the same 401 as an unknown one. Tokens are 32 random bytes; only an HMAC (under a key derived from the pepper) is stored, in each tenant's `_platform_refresh_tokens` (created by `vibe migrate` when such a route exists). Every use rotates the token. Presenting a spent token again revokes the whole family (also after it expired), except for one reissue inside `reuse-grace` while the earlier child is still unused; that reissue kills the lost child. On PostgreSQL every rotation and revocation of a family holds a transaction-scoped family lock. Until the dedicated refresh-store role lands (#51), `vibe migrate` revokes all privileges on the table from `public_reader`, `public_writer`, `app_rw` and `PUBLIC`. This is a table-level `REVOKE` on `_platform_refresh_tokens`, not a schema-level one. It runs after the tenant's migrations, so it also removes the table grant that a `GRANT … ON ALL TABLES IN SCHEMA` in a migration creates (that statement grants on each existing table when it runs). It does not change schema privileges or default privileges (`ALTER DEFAULT PRIVILEGES`). Existing tenants must re-run `vibe migrate` to get the revoke. Logout-all takes the same per-family lock as rotation for every family it revokes, and a per-user lock shared with login, so a login that runs at the same time is either revoked too or starts after logout-all finishes. Every failure (unknown, expired, revoked, replayed) is the same 401 `INVALID_REFRESH_TOKEN`, padded to 300 ms. Replays are logged. Limits: 30/min per IP (platform, 429), and 10/min per family (`rate-limit` `key: family` can only lower it). Only live (unrevoked) tokens count toward the family limit, and hitting it returns the same 401, so the response never reveals whether a token exists. Clients should not retry a 401 from the refresh route in a loop: back off (for example, wait at least a minute) or send the user back to login. A rate-limited token is not consumed, so it still works after the back-off. Logout revokes refresh tokens only; access tokens already issued stay valid until they expire (`ttl`, at most 1 h), so keep access token TTLs short. The per-IP limit uses the client IP: `X-Forwarded-For` is honoured only from `VIBE_TRUSTED_PROXIES`, so set it when `vibe serve` runs behind a proxy, or all clients behind it share one bucket. Pepper rotation is not supported: refresh tokens and API keys are stored as HMACs under keys derived from `VIBE_API_KEY_PEPPER`, so changing it invalidates every refresh token (users must log in again) and every API key. A `### token-revoke` pipeline (`scope: family` takes the caller's own `refresh_token`; `scope: all` revokes every family of the caller) needs auth, runs before the route's steps, and answers 204 whether or not anything was revoked.

**Database roles.** `public_reader`, `public_writer` and `app_rw` must never be members of the runtime role (directly or through other roles): `vibe serve` and `vibe migrate` refuse to start if they are, and migrate fails if any of them can still reach `_platform_refresh_tokens` after its revoke. Migrate revokes access on the tables it creates; schema-level or default-privilege grants added by hand afterwards (for example `GRANT ALL ON ALL TABLES IN SCHEMA …` or `ALTER DEFAULT PRIVILEGES`) can re-open that access, so re-run `vibe migrate` after changing grants.

**Registration and verification (#26, part 3).** The fixture `tests/fixtures/auth-app` implements spec §12.11 end to end:

- `POST /auth/v1/{slug}/register` takes only email and name (decision 67). It always runs one upsert plus one outbox insert and answers the same 202 body whether the email is new, unverified, verified or invited (decision 56).
- Re-registering an unverified email only rotates its token, so the old link stops working; a verified user or one with a pending invite gets an "already registered" notice instead.
- `POST /auth/v1/{slug}/verify-email` runs `user.complete-verification`. It always hashes the password first, then sets `password` and `email_verified_at` in one UPDATE.
- After a successful verification the platform revokes every refresh family of that user (`email-verified`). Admin invites (decision 69) use the same route and method.

Runtime rules:

- A `query` that writes `password` needs `capabilities: [set-password]`, and one that writes `email_verified_at` needs `confirm-verification` (checked by validate and at runtime).
- Public response floors pad a response that runs past the floor up to the next 100 ms (decision 66).
- Expressions accept quoted string literals (`purpose: 'invite'`).

## Install

Download `vibe` from [GitHub Releases](https://github.com/vibe-backend/sdk/releases). The CLI is not published to crates.io and it is not published as a container image.

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `vibe-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `vibe-aarch64-unknown-linux-gnu.tar.gz` |
| macOS x86_64 | `vibe-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 | `vibe-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `vibe-x86_64-pc-windows-msvc.zip` |

Each release also has `SHA256SUMS`. On Linux or macOS:

```bash
tag=v0.1.0
asset=vibe-x86_64-unknown-linux-gnu.tar.gz
curl -fsSL -O "https://github.com/vibe-backend/sdk/releases/download/${tag}/${asset}"
curl -fsSL -O "https://github.com/vibe-backend/sdk/releases/download/${tag}/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "$asset"
install -m 755 vibe "$HOME/.local/bin/vibe"
```

On Windows, download `vibe-x86_64-pc-windows-msvc.zip`, check it against `SHA256SUMS`, and unpack `vibe.exe`.

A checkout with Rust 1.89 can also build the binary with `cargo build --release --locked`.

## Run a project

The shop demo is moving to [vibe-backend/sample-shop](https://github.com/vibe-backend/sample-shop). That repository currently contains only the frontend; the vibe backend project will be added there later. Until then, the small shop project used by this repo's tests, [tests/fixtures/shop](tests/fixtures/shop), is the runnable example. From a checkout of this repo with Rust 1.89 (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` pins the same channel), replace `tests/fixtures/shop` with your own project path as needed:

```bash
vibe compile --project tests/fixtures/shop
vibe approve --project tests/fixtures/shop \
  --reviewer ada@example.com --reviewer grace@example.com \
  --reason "sample shop locks"
vibe validate --project tests/fixtures/shop --deploy
vibe manifest --project tests/fixtures/shop -o manifest.json
vibe serve --manifest manifest.json --project tests/fixtures/shop --memory
```

**Breaking (#31):** `vibe manifest` without `-o` writes `./manifest.json` relative to the current working directory (same as `-o` / `serve --manifest`), not `<project>/manifest.json`.

`--memory` is a dev executor. PostgreSQL needs two roles. `vibe migrate` connects with the owner role (`VIBE_MIGRATE_DATABASE_URL`) and grants SELECT, INSERT and UPDATE to the runtime role named in `VIBE_RUNTIME_DB_ROLE`. `vibe serve` connects as that runtime role (`VIBE_DATABASE_URL`, or `DATABASE_URL`) and refuses to start if the role is, or can `SET ROLE` to, a superuser or BYPASSRLS role, or can own any tenant schema, relation, function or type, because row-level security would not apply. `VIBE_RUNTIME_DB_ROLE` is required:

```bash
VIBE_MIGRATE_DATABASE_URL=postgres://owner@db/app VIBE_RUNTIME_DB_ROLE=vibe_app \
  vibe migrate --manifest manifest.json --project tests/fixtures/shop --tenant tenant0001
VIBE_DATABASE_URL=postgres://vibe_app@db/app \
  vibe serve --manifest manifest.json --project tests/fixtures/shop
```

Structured markdown compiles with no model call. Natural-language sources need the variables in [.env.example](.env.example). `LLM_*` wins over `OPENAI_*`. Sampling parameters are sent only when the model is in the adapter capability table. Copy `.env.example` to `.env` (gitignored) and do not put a real key in the repo.

The runtime refuses to start when `LLM_*` or `OPENAI_*` is set. Public catalog reads resolve the tenant from `.vibe/slugs.json`. Only `--memory` falls back to `slugs.example.json` when that file is missing; otherwise public slug routes return 404. `VIBE_ENV` (case-insensitive) is development only for `dev`, `development`, `local` or `test`; `ci` is CI, and any other value is production. Unset counts as development with `--memory` and as production with PostgreSQL. Outside development, `--memory` is refused and `VIBE_API_KEY_PEPPER` must be set to at least 32 bytes (not characters); generate one with `openssl rand -base64 48`. The test fixture's [slugs.example.json](tests/fixtures/shop/slugs.example.json) maps slug `acme` to tenant `acme0001`. Add another with `vibe slug add <slug> --tenant <id>`. Staff writes need a JWT (`VIBE_JWT_PUBLIC_KEY_PEM`, `VIBE_JWT_ISS`, `VIBE_JWT_AUD`, `VIBE_JWT_KID`). The PEM's algorithm is detected (ES256 or RS256); override with `VIBE_JWT_ALG` if needed. To use an identity provider's keys instead, set `VIBE_JWT_JWKS_URI` (https only — an `http://` issuer or JWKS URI refuses to start). Do not set PEM and JWKS together. If no PEM is set, the default is `{VIBE_JWT_ISS}/.well-known/jwks.json`. Keys are cached for 10 minutes, an unknown `kid` triggers a refetch at most once a minute (single-flight; other requests keep using the cache), and a failed fetch falls back to the old keys for at most an hour. For local JWKS experiments, point `VIBE_JWT_JWKS_URI` at an https endpoint that serves a static JWKS (a tunnel or a local reverse proxy with TLS); plain `http://localhost` is refused. `VIBE_JWT_ROLES_CLAIM` (default `roles`) names the roles claim (dotted paths like `realm_access.roles` are supported); a route's auth `roles-claim` overrides it. Reserved claims (`aud`, `scope`, …) are rejected. A JWT `scope` (space-separated string or array) or `scp` claim, and API-key scopes, are checked against the route's `scopes`; the caller must hold all of them. Empty route `scopes` means scopes are opt-in (not required). A malformed lock `roles:` or `scopes:` list (for example `roles: staff` without brackets) is a build error, never an empty allow-all. API keys are loaded from `VIBE_API_KEYS_FILE`: a JSON array of `{key_id, secret_hash, env, tenant_id, scopes, expires_unix, revoked}` records, where `secret_hash` is the HMAC of the secret with `VIBE_API_KEY_PEPPER` (see `hash_api_secret` / mint with a 12-char base32 `key_id` and `mbk_{live|test}_{key_id}_{secret}`). The file must be `chmod 600` and is re-read when its mtime changes (at most once a second); a bad reload keeps the previous keys. Keys are `mbk_live_…` when `VIBE_ENV` is production, and `mbk_test_…` otherwise. Auth failures return a generic 401; enable debug logging to see the operator-facing reason.

Per-IP rate limits and public-write quotas use the TCP peer address. Set `VIBE_TRUSTED_PROXIES` (comma-separated IPs or CIDR blocks, e.g. `10.0.0.0/8,2001:db8::/32`) behind a load balancer. `X-Forwarded-For` is honoured only from those proxies, and the rightmost hop that is not a trusted proxy counts. Hops may carry a port (`1.2.3.4:5678`, `[2001:db8::1]:443`), and IPv4-mapped IPv6 addresses count as IPv4. IPv6 clients are limited per /64. Rate and quota state is kept per route and key type with a fixed capacity; when a shard is full, its least recently used bucket is evicted, so flooding never blocks other clients.

`vibe approve` needs `--reason` and one `--reviewer` per reviewer (repeat the flag, e.g. `--reviewer a@x --reviewer b@x`). High-risk locks (public endpoints, providers, and migrations) need two reviewers. A failed approve writes nothing.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

Implemented in 0.1.0: discovery and source hashing, OpenAI-compatible / Anthropic / Gemini adapters, a fake adapter for tests, deterministic compile of structured markdown, lock rendering, the validator, local ed25519 approval, a signed manifest, JWT and API-key auth, built-in CRUD, offset pagination (platform `max-page-size` 500), soft delete, per-row `owner-key` scoping, and a single-connection Postgres executor with schema-per-tenant RLS settings.

Deferred: platform KMS and GitHub review binding, refresh tokens, account lockout, real captcha, persisted quotas, cross-instance rate limits, crypto-shredding, the email outbox, webhooks, queues, schedules, Postgres TLS, and a connection pool per tenant. Custom method steps other than `method`, `respond`, and `validate` are not executed. Challenge proofs are accepted when the header is non-empty.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## CI/CD

Required CI checks are **Markdown lint & link check** and **Rust fmt / clippy / test**. There is no Docker image job.

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust 1.89 `cargo check --locked`, `fmt`, `clippy`, and `test` once a `Cargo.toml` exists.
- **Release** (`.github/workflows/release.yml`, on tags `v*`): builds `vibe` for Linux x86_64 and aarch64, macOS x86_64 and aarch64, and Windows x86_64. Archives are `tar.gz` (Unix) or `zip` (Windows), plus `SHA256SUMS`, on a GitHub Release. Nothing is pushed to GHCR or crates.io.
- **CD** (`.github/workflows/cd.yml`): GHCR image publishing is disabled.
