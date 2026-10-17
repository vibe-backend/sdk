# vibe-backend SDK

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The HTTP runtime loads that manifest. It does not interpret natural language.

The specification is [docs/spec-v1.7.md](docs/spec-v1.7.md). The draft [docs/spec-v1.8.md](docs/spec-v1.8.md) is under review.

## Development

`vibe dev` (no precompile) is planned for vNext (spec decision 166); use the Hello world flow below.

## Runtime features

**Method ops (#26, part 1).** Method steps `query`, `verify-password`, `hash-password`, `random-token`, `hash-token` and `sign-jwt` now run. `query` runs the lock's parameterised SQL in the tenant schema under row level security, as `public_reader` (or `public_writer` for a step marked `writes: true`, via `SET LOCAL ROLE`; both database roles must exist and be granted to the runtime role), in a READ ONLY transaction unless the step writes. Lock SQL is checked with a PostgreSQL parser by `vibe validate` and again at runtime: exactly one SELECT, INSERT, UPDATE or DELETE; unqualified plain table names (no `pg_*`, `information_schema` or platform tables); no table functions; no `SELECT … INTO`; no casts to object-identifier types (`regclass`, `regproc`, …); only allowlisted functions (`src/sqlguard.rs`), so no `SET`, settings or dynamic SQL. A data-modifying `WITH` is rejected, and a unique violation (SQLSTATE 23505) returns 409 `CONFLICT`. `query` is PostgreSQL only: with `--memory`, `vibe serve` warns at startup and the step fails with "query steps need PostgreSQL; the --memory executor does not run them" (500). `verify-password` uses argon2id and only accepts hashes made with the platform's own parameters (m=65536, t=3, p=1) and passwords of at most 256 characters (at most four hashes at a time per instance; a request that waits more than 5 s gets 503). When the user does not exist (`missing-user: dummy-hash`) it compares against a dummy hash, built at startup, so timing and the result look the same as a wrong password. Endpoints that reach a `verify-password` step are padded to at least 500 ms. `validate` conditions are `X is empty`, `X is not empty`, `X == true|false|null` or `X != true|false|null` (code-span backticks are allowed); anything else is a validate error and, at runtime, fails closed (the step's `fail` fires), never a silent pass. Step expressions can read `ctx.user.sub`, `ctx.user.roles` and `ctx.user.tenant`. `random-token` returns `{value, hash}` for 32 random bytes; `hash` (and `hash-token`) is HMAC-SHA-256 under a key derived from `VIBE_API_KEY_PEPPER`, as a `bytea` literal (`\x…`) for a `bytea` column (`normalize: email` trims and lowercases first). `sign-jwt` runs only in methods with `capabilities: [sign-token]`. It reads the private PEM from the environment variable named by `key-ref` (`<key-ref>_KID` sets the kid, default `VIBE_JWT_KID`), and `vibe serve` refuses to start if that variable is missing. The runtime sets `iss`, `aud`, `iat`/`nbf`/`exp` (ttl at most 1 h), `jti`, `tenant_id` (always the request's tenant) and the roles claim, which it reads from `users.roles` for the token's `sub`; a lock cannot supply roles. `sub` must be a field of a row read by an earlier `query` step (for example `u.id`), checked by `vibe validate` and at runtime. Other claims must be listed in `custom-claims` and can never be a reserved claim or any configured roles claim. The step returns `{access_token, token_type, expires_in, expires_at}`. `issue-refresh: platform-decided` (or `new-family`) adds a `refresh_token`; it needs a route with a `token-refresh` pipeline whose `sign-method` is that method, otherwise the step errors.

**Refresh tokens (#26, part 2).** A route with a `### token-refresh` pipeline (`sign-method`, `refresh-ttl` ≤ 30d, `idle-ttl` ≤ 7d, `reuse-grace` default and max 10s (1s–10s; larger values or 0 are rejected by the build and at `vibe serve` startup), `transport: body`) must be public with auth removed. It takes `{refresh_token}` and returns `{access_token, token_type, expires_in, expires_at, refresh_token, refresh_expires_in}`. Refresh token expiry: each token expires `idle-ttl` (default and max 7d) after it was issued unless it is used, and a family ends `refresh-ttl` (default and max 30d) after login. `refresh_expires_in` is the seconds until the returned token expires if unused: the smaller of `idle-ttl` and what is left of the family lifetime. A `sign-jwt` step with `issue-refresh` returns `refresh_expires_in` on login too. The compiler-added `source-lines` bullet is ignored; any other unknown setting is an error. `vibe validate` (also with `--deploy`) and the manifest builder accept `issue-refresh: none | platform-decided | new-family`, and reject a `sign-method` that does not name a method with a `sign-jwt` step. Refresh routes run before request-table validation, so a missing or malformed token is the same 401 as an unknown one. Tokens are 32 random bytes; only an HMAC (under a key derived from the pepper) is stored, in each tenant's `_platform_refresh_tokens` (created by `vibe migrate` when such a route exists). Every use rotates the token. Presenting a spent token again revokes the whole family (also after it expired), except for one reissue inside `reuse-grace` while the earlier child is still unused; that reissue kills the lost child. On PostgreSQL every rotation and revocation of a family holds a transaction-scoped family lock. Until the dedicated refresh-store role lands (#51), `vibe migrate` revokes all privileges on the table from `public_reader`, `public_writer`, `app_rw` and `PUBLIC`. This is a table-level `REVOKE` on `_platform_refresh_tokens`, not a schema-level one. It runs after the tenant's migrations, so it also removes the table grant that a `GRANT … ON ALL TABLES IN SCHEMA` in a migration creates (that statement grants on each existing table when it runs). It does not change schema privileges or default privileges (`ALTER DEFAULT PRIVILEGES`). Existing tenants must re-run `vibe migrate` to get the revoke. The runtime role gets SELECT, INSERT and column-level UPDATE on rotation and revocation columns only (no DELETE); a trigger keeps identity columns immutable and revocation final, and `vibe serve` refuses to start if those grants or the trigger differ. Each `vibe migrate` also deletes families whose lifetime ended more than a day ago, in bounded batches (see [docs/runtime-config.md](docs/runtime-config.md#refresh-tokens)). Logout-all takes the same per-family lock as rotation for every family it revokes, and a per-user lock shared with login, so a login that runs at the same time is either revoked too or starts after logout-all finishes. Every failure (unknown, expired, revoked, replayed) is the same 401 `INVALID_REFRESH_TOKEN`, padded to 300 ms. Replays are logged. Limits: 30/min per IP (platform, 429), and 10/min per family (`rate-limit` `key: family` can only lower it). Only live (unrevoked) tokens count toward the family limit, and hitting it returns the same 401, so the response never reveals whether a token exists. Clients should not retry a 401 from the refresh route in a loop: back off (for example, wait at least a minute) or send the user back to login. A rate-limited token is not consumed, so it still works after the back-off. If the token store cannot be read (for example the connection pool is exhausted), the refresh fails closed with a retryable 503 `SERVICE_UNAVAILABLE` (`Retry-After: 1`) instead of skipping the family limit; the token is not consumed. Logout revokes refresh tokens only; access tokens already issued stay valid until they expire (`ttl`, at most 1 h), so keep access token TTLs short. The per-IP limit uses the client IP: `X-Forwarded-For` is honoured only from `VIBE_TRUSTED_PROXIES`, so set it when `vibe serve` runs behind a proxy, or all clients behind it share one bucket. Pepper rotation is not supported: refresh tokens and API keys are stored as HMACs under keys derived from `VIBE_API_KEY_PEPPER`, so changing it invalidates every refresh token (users must log in again) and every API key. A `### token-revoke` pipeline (`scope: family` takes the caller's own `refresh_token`; `scope: all` revokes every family of the caller) needs auth, runs before the route's steps, and answers 204 whether or not anything was revoked.

**Database roles.** `public_reader`, `public_writer` and `app_rw` must never be members of the runtime role (directly or through other roles): `vibe serve` and `vibe migrate` refuse to start if they are, and migrate fails if any of them can still reach `_platform_refresh_tokens` after its revoke. Migrate revokes access on the tables it creates; schema-level or default-privilege grants added by hand afterwards (for example `GRANT ALL ON ALL TABLES IN SCHEMA …` or `ALTER DEFAULT PRIVILEGES`) can re-open that access, so re-run `vibe migrate` after changing grants.

## Install

There are currently no published tags, GitHub Releases, or downloadable release assets for this repository. The `vibe` CLI is not published to crates.io or as a container image. For now, build it from a source checkout with Rust 1.89:

```bash
cargo build --release --locked
./target/release/vibe --help
```

Do not use guessed release tags or `releases/download` URLs; they are not a supported installation path until an actual release has been published and verified.

## Hello world

```bash
vibe            # first run asks for LLM settings, then: 幫我開一個 hello world
vibe compile
vibe manifest --dev
vibe serve --memory
curl http://127.0.0.1:8080/hello/v1/dev/hello
```

First run writes LLM settings to `.env` (gitignored; the key is entered masked and never printed). The CLI shows the files and asks before writing, never overwrites, and non-interactive `vibe` prints help. In development, `serve --memory` always serves slug `dev`, even when other slugs are configured, and picks up `vibe slug add` without restart. `vibe manifest --dev` runs the same deploy checks as `vibe validate --deploy --dev`.

## Run a project

Another sample is [vibe-backend/sample-envelope](https://github.com/vibe-backend/sample-envelope), a Chinese envelope generator that is frontend-only for now.

The shop demo is moving to [vibe-backend/sample-shop](https://github.com/vibe-backend/sample-shop). That repository currently contains only the frontend; the vibe backend project will be added there later. Until then, the small shop project used by this repo's tests, [tests/fixtures/shop](tests/fixtures/shop), is the runnable example. From a checkout of this repo with Rust 1.89 (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` pins the same channel), replace `tests/fixtures/shop` with your own project path as needed:

```bash
vibe compile --project tests/fixtures/shop
vibe validate --project tests/fixtures/shop
vibe validate --deploy --dev --project tests/fixtures/shop
vibe manifest --dev --project tests/fixtures/shop -o manifest.json
vibe serve --manifest manifest.json --project tests/fixtures/shop --memory
```

**Breaking (#31):** `vibe manifest` without `-o` writes `./manifest.json` relative to the current working directory (same as `-o` / `serve --manifest`), not `<project>/manifest.json`.

Runtime configuration (PostgreSQL roles and TLS, connection pool, LLM variables, `VIBE_ENV`, JWT/JWKS and API keys, rate limits and trusted proxies) is in [docs/runtime-config.md](docs/runtime-config.md).

Locks are build output: they are not committed (`*.lock.md` is gitignored) and carry no approval or signature (spec v1.8). Deployments use only the manifest built and attested in the protected `release` environment (`vibe manifest --release`); see [docs/ci/README.md](docs/ci/README.md). Review happens in pull requests; see [docs/pr-review-guide.md](docs/pr-review-guide.md). `security-items` in a lock are informational. The manifest is still signed.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

See [docs/status.md](docs/status.md) and [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## CI/CD

Required CI checks are **Markdown lint & link check** and **Rust fmt / clippy / test**. There is no Docker image job.

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust 1.89 `cargo check --locked`, `fmt`, `clippy`, and `test` once a `Cargo.toml` exists.
- **Release workflow** (`.github/workflows/release.yml`, on tags `v*`): is configured to build archives for Linux x86_64 and aarch64, macOS x86_64 and aarch64, and Windows x86_64 (`tar.gz` on Unix, `zip` on Windows) plus `SHA256SUMS`, but no tag or GitHub Release has been published. Nothing is pushed to GHCR or crates.io.
- **CD** (`.github/workflows/cd.yml`): GHCR image publishing is disabled.

## Binary installation

The install/upgrade/remove manager in [docs/install.md](docs/install.md) is an unreleased review candidate only. It is not a public installation method, because there are no published tags, Releases, or assets to install. Continue using a source checkout until the release and provenance flow has passed review and end-to-end verification.
