# vibe-backend SDK: technical details

This page holds the detailed, technical material that used to live in the top-level
[README](../README.md). The README now gives a short, plain-language introduction;
everything below is kept unchanged for developers, operators and reviewers.

## Overview

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The `providers/` directory is optional: without provider sources, the built-in in-memory provider `main` stores data until restart. Release builds and runtime startup warn; production still refuses memory. Add `providers/main.md` to use PostgreSQL. The HTTP runtime loads that manifest. It does not interpret natural language.

The specification is [docs/spec-v1.7.md](spec-v1.7.md). The draft [docs/spec-v1.8.md](spec-v1.8.md) is under review.

## Plain-language authoring (decision 188)

Write “Anyone can read the counter” to authorize a public endpoint; “公開” and
“任何人” also work. Public endpoints remove auth, and public-writes follows the
methods they call. Explicit `public`, `auth` and `public-writes` frontmatter wins.
Without clear public wording, endpoints stay private and require auth.

The flat filename and project route rule determine the path. With the built-in
defaults (`prefix: /api`, `default-slug: dev`), `api/web/v1/counter.md` resolves
to `/api/web/v1/counter`. Set `default-slug: (none)` in `api/_project.md` when
public paths should include `/{slug}`, and `prefix: (none)` when no prefix is
wanted. Locks exclude the project prefix.

GET and POST on one URL use two files with an explicit same-path sentence:

- `counter.md`: “Anyone can read the counter. Use GET at /web/v1/counter.”
- `increment-counter.md`: “Anyone can add one to the counter. Use POST at /web/v1/counter.”

The router and manifest distinguish method plus path, so these coexist.

Model dependencies provide their actual normalized table names. SQL must name
existing model tables and columns and respect CRUD: SELECT needs `get` or `list`,
INSERT needs `create`, UPDATE needs `update`, DELETE needs `delete`.
`tenant_id` is platform-owned and cannot appear in query SQL.

Without providers, the target is memory: query SQL must use decision 183's
unique-key UPDATE increments, single-row INSERT upserts/DO NOTHING, unique-key
SELECT, or COALESCE(MAX) forms. Unsupported SQL is a build error sent to repair.
Build and lock validation also reject statically known method return-shape mismatches and reads of fields the method does not return.
Explicit PostgreSQL providers retain an advisory warning for memory development.

A prose requirement such as “at most 50 per day” produces a warning quoting the
source sentence when the lock cannot enforce it. Built-in rate limits use
per-minute windows; a warning never means that a daily cap was installed.
Review warnings before relying on a limit.

## SDK and deployment runtime

**vibe-sdk** installs as `vibe` for development and CI builds: interactive `vibe`,
`vibe build` and `vibe run`.
**vibe-runtime** is for deployment and only runs build artifacts. It contains no
LLM client, build or interactive code. Use `vibe-runtime run --artifact <dir>`;
bare `vibe-runtime` means run, with artifact `.` by default. Both forms accept
`--addr`, `--db` / `VIBE_DB`, and `--allow-test-build` (development only).
`vibe-runtime verify --artifact dist` checks an artifact with the same pins without serving (exit 0 on success, 1 on failure).

Release artifacts need `--expect-identity` / `VIBE_EXPECT_IDENTITY` and
`--expect-issuer` / `VIBE_EXPECT_ISSUER`; optional `--expect-repository-id` /
`VIBE_EXPECT_REPOSITORY_ID` pins the numeric repo id. See
[runtime configuration](runtime-config.md#release-manifest-pins-decision-182).

Install verified pre-release assets for either or both with the manager:

```sh
./scripts/vibe-install.sh install --version v1.0.0-rc.7
./scripts/vibe-install.sh install --version v1.0.0-rc.7 --runtime
VIBE_INSTALL_BINARY=runtime ./scripts/vibe-install.sh install --version v1.0.0-rc.7
```

The default installs `vibe`; `--runtime` installs `vibe-runtime`. They coexist.
Release archives are `vibe-sdk-<target>.tar.gz` / `.zip` and
`vibe-runtime-<target>.tar.gz` / `.zip`; SHA256SUMS and build provenance
attestations cover both. See [installation details](install.md).

Build from source with Rust 1.89:

```sh
cargo build --release --bin vibe
cargo build --release --no-default-features --bin vibe-runtime
```

## Development

`vibe dev` (no precompile) is planned for vNext (spec decision 166); use the Hello world flow below.

## Runtime features

**Method ops (#26, part 1).** Method steps `query`, `verify-password`, `hash-password`, `random-token`, `hash-token` and `sign-jwt` now run. `query` runs the lock's parameterised SQL in the tenant schema under row level security, as `public_reader` (or `public_writer` for a step marked `writes: true`, via `SET LOCAL ROLE`; both database roles must exist and be granted to the runtime role), in a READ ONLY transaction unless the step writes. Lock SQL is checked with a PostgreSQL parser by `vibe build` and again at runtime: exactly one SELECT, INSERT, UPDATE or DELETE; unqualified plain table names (no `pg_*`, `information_schema` or platform tables); no table functions; no `SELECT … INTO`; no casts to object-identifier types (`regclass`, `regproc`, …); only allowlisted functions (`src/sqlguard.rs`), so no `SET`, settings or dynamic SQL. A data-modifying `WITH` is rejected, and a unique violation (SQLSTATE 23505) returns 409 `CONFLICT`. `query` is PostgreSQL only: with `--db memory`, `vibe run` warns at startup and the step fails with a PostgreSQL-required error (500). `verify-password` uses argon2id and only accepts hashes made with the platform's own parameters (m=65536, t=3, p=1) and passwords of at most 256 characters (at most four hashes at a time per instance; a request that waits more than 5 s gets 503). When the user does not exist (`missing-user: dummy-hash`) it compares against a dummy hash, built at startup, so timing and the result look the same as a wrong password. Endpoints that reach a `verify-password` step are padded to at least 500 ms. `validate` conditions are `X is empty`, `X is not empty`, `X == true|false|null` or `X != true|false|null` (code-span backticks are allowed); anything else is a validate error and, at runtime, fails closed (the step's `fail` fires), never a silent pass. Step expressions can read `ctx.user.sub`, `ctx.user.roles` and `ctx.user.tenant`. `random-token` returns `{value, hash}` for 32 random bytes; `hash` (and `hash-token`) is HMAC-SHA-256 under a key derived from `VIBE_API_KEY_PEPPER`, as a `bytea` literal (`\x…`) for a `bytea` column (`normalize: email` trims and lowercases first). `sign-jwt` runs only in methods with `capabilities: [sign-token]`. It reads the private PEM from the environment variable named by `key-ref` (`<key-ref>_KID` sets the kid, default `VIBE_JWT_KID`), and `vibe run` refuses to start if that variable is missing. The runtime sets `iss`, `aud`, `iat`/`nbf`/`exp` (ttl at most 1 h), `jti`, `tenant_id` (always the request's tenant) and the roles claim, which it reads from `users.roles` for the token's `sub`; a lock cannot supply roles. `sub` must be a field of a row read by an earlier `query` step (for example `u.id`), checked by `vibe build` and at runtime. Other claims must be listed in `custom-claims` and can never be a reserved claim or any configured roles claim. The step returns `{access_token, token_type, expires_in, expires_at}`. `issue-refresh: platform-decided` (or `new-family`) adds a `refresh_token`; it needs a route with a `token-refresh` pipeline whose `sign-method` is that method, otherwise the step errors.

**Refresh tokens (#26, part 2).** A route with a `### token-refresh` pipeline (`sign-method`, `refresh-ttl` ≤ 30d, `idle-ttl` ≤ 7d, `reuse-grace` default and max 10s (1s–10s; larger values or 0 are rejected by the build and at `vibe run` startup), `transport: body`) must be public with auth removed. It takes `{refresh_token}` and returns `{access_token, token_type, expires_in, expires_at, refresh_token, refresh_expires_in}`. Refresh token expiry: each token expires `idle-ttl` (default and max 7d) after it was issued unless it is used, and a family ends `refresh-ttl` (default and max 30d) after login. `refresh_expires_in` is the seconds until the returned token expires if unused: the smaller of `idle-ttl` and what is left of the family lifetime. A `sign-jwt` step with `issue-refresh` returns `refresh_expires_in` on login too. The compiler-added `source-lines` bullet is ignored; any other unknown setting is an error. `vibe build` and the manifest builder accept `issue-refresh: none | platform-decided | new-family`, and reject a `sign-method` that does not name a method with a `sign-jwt` step. Refresh routes run before request-table validation, so a missing or malformed token is the same 401 as an unknown one. Tokens are 32 random bytes; only an HMAC (under a key derived from the pepper) is stored, in each tenant's `_platform_refresh_tokens` (created by `vibe run --db postgres` when such a route exists). Every use rotates the token. Presenting a spent token again revokes the whole family (also after it expired), except for one reissue inside `reuse-grace` while the earlier child is still unused; that reissue kills the lost child. On PostgreSQL every rotation and revocation of a family holds a transaction-scoped family lock. Until the dedicated refresh-store role lands (#51), `vibe run --db postgres` revokes all privileges on the table from `public_reader`, `public_writer`, `app_rw` and `PUBLIC`. This is a table-level `REVOKE` on `_platform_refresh_tokens`, not a schema-level one. It runs after the tenant's migrations, so it also removes the table grant that a `GRANT … ON ALL TABLES IN SCHEMA` in a migration creates (that statement grants on each existing table when it runs). It does not change schema privileges or default privileges (`ALTER DEFAULT PRIVILEGES`). Existing tenants must restart `vibe run --db postgres` with `VIBE_MIGRATE_DATABASE_URL` set to get the revoke. The runtime role gets SELECT, INSERT and column-level UPDATE on rotation and revocation columns only (no DELETE); a trigger keeps identity columns immutable and revocation final, and `vibe run` refuses to start if those grants or the trigger differ. Each `vibe run --db postgres` also deletes families whose lifetime ended more than a day ago, in bounded batches (see [docs/runtime-config.md](runtime-config.md#refresh-tokens)). Logout-all takes the same per-family lock as rotation for every family it revokes, and a per-user lock shared with login, so a login that runs at the same time is either revoked too or starts after logout-all finishes. Every failure (unknown, expired, revoked, replayed) is the same 401 `INVALID_REFRESH_TOKEN`, padded to 300 ms. Replays are logged. Limits: 30/min per IP (platform, 429), and 10/min per family (`rate-limit` `key: family` can only lower it). Only live (unrevoked) tokens count toward the family limit, and hitting it returns the same 401, so the response never reveals whether a token exists. Clients should not retry a 401 from the refresh route in a loop: back off (for example, wait at least a minute) or send the user back to login. A rate-limited token is not consumed, so it still works after the back-off. If the token store cannot be read (for example the connection pool is exhausted), the refresh fails closed with a retryable 503 `SERVICE_UNAVAILABLE` (`Retry-After: 1`) instead of skipping the family limit; the token is not consumed. Logout revokes refresh tokens only; access tokens already issued stay valid until they expire (`ttl`, at most 1 h), so keep access token TTLs short. The per-IP limit uses the client IP: `X-Forwarded-For` is honoured only from `VIBE_TRUSTED_PROXIES`, so set it when `vibe run` runs behind a proxy, or all clients behind it share one bucket. Pepper rotation is not supported: refresh tokens and API keys are stored as HMACs under keys derived from `VIBE_API_KEY_PEPPER`, so changing it invalidates every refresh token (users must log in again) and every API key. A `### token-revoke` pipeline (`scope: family` takes the caller's own `refresh_token`; `scope: all` revokes every family of the caller) needs auth, runs before the route's steps, and answers 204 whether or not anything was revoked.

**Database roles.** `public_reader`, `public_writer` and `app_rw` must never be members of the runtime role (directly or through other roles): `vibe run --db postgres` refuses to start if they are, and startup migration fails if any of them can still reach `_platform_refresh_tokens` after its revoke. Startup migration revokes access on the tables it creates; schema-level or default-privilege grants added by hand afterwards (for example `GRANT ALL ON ALL TABLES IN SCHEMA …` or `ALTER DEFAULT PRIVILEGES`) can re-open that access, so restart `vibe run --db postgres` with `VIBE_MIGRATE_DATABASE_URL` set after changing grants.

## Install

Pre-releases are available on [GitHub Releases](https://github.com/vibe-backend/sdk/releases); the CLI is not on crates.io or a container image. You can also build from source with Rust 1.89:

```bash
cargo build --release --locked --bin vibe
./target/release/vibe --help
```

Use an existing release tag and verify downloaded assets as described in [the installation guide](install.md).

## Hello world (command details)

```bash
vibe            # first run asks for LLM settings, then: 幫我開一個 hello world
vibe build
vibe run
curl http://127.0.0.1:8080/hello/v1/dev/hello
```

First run writes global `llm.env` and a separate key file (0600) only when no `LLM_*` setting exists in the process environment, project `.env` or global config. With partial project or process LLM settings, it prompts only for missing values and appends them to project `.env` (0600), adding `.env` to `.gitignore`. The key is entered masked and never printed; see [global LLM configuration](runtime-config.md#global-llm-configuration-decision-185). The CLI shows the files and asks before writing, never overwrites, and non-interactive `vibe` prints help. In development, `run` always serves slug `dev`, even when other slugs are configured, and reloads edits to `.vibe/slugs.json` without restart. `vibe build` refreshes locks, runs deploy validation and writes the dev manifest. `vibe run` validates project locks and builds the dev manifest in memory; it never calls the LLM. Missing, stale or wrong-version locks ask for `vibe build`.

## Run a project

Another sample is [vibe-backend/sample-envelope](https://github.com/vibe-backend/sample-envelope), a Chinese envelope generator that is frontend-only for now.

The shop demo is moving to [vibe-backend/sample-shop](https://github.com/vibe-backend/sample-shop). That repository currently contains only the frontend; the vibe backend project will be added there later. Until then, the small shop project used by this repo's tests, [tests/fixtures/shop](../tests/fixtures/shop), is the runnable example. From a checkout of this repo with Rust 1.89 (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` pins the same channel), replace `tests/fixtures/shop` with your own project path as needed:

```bash
vibe build --project tests/fixtures/shop -o manifest.json
vibe run --artifact manifest.json --project tests/fixtures/shop --db memory
```

**Breaking (#31):** `vibe build` (any mode) without `-o` writes `./manifest.json` relative to the current working directory (same as `-o` / `run --artifact`), not `<project>/manifest.json`.

Runtime configuration (PostgreSQL roles and TLS, connection pool, LLM variables, `VIBE_ENV`, JWT/JWKS and API keys, rate limits and trusted proxies) is in [docs/runtime-config.md](runtime-config.md).

Locks are build output: they are not committed (`*.lock.md` is gitignored) and carry no approval or signature (spec v1.8). Deployments use only the manifest built and attested in the protected `release` environment (`vibe build --release`); see [docs/ci/README.md](ci/README.md). Review happens in pull requests; see [docs/pr-review-guide.md](pr-review-guide.md). `security-items` in a lock are informational. The manifest is still signed.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The CLI flow is interactive `vibe` → `vibe build` → `vibe run` → curl. Build refreshes stale locks,
validates, and writes a dev manifest. Fresh locks need no LLM call or LLM key,
including in CI. Use `vibe build --check` for an in-memory policy check that
writes nothing, or `vibe build --release` for an unsigned release manifest with
all release checks, ready for Sigstore signing. Options include `--project`,
`-o`/`--output`, `--release-version`, and `--commit`.

Run the project with `vibe run`, then curl the API. Use `--artifact` to run a build artifact.
Locks record `vibe-version: 1.0.0-rc.7`: stable versions use major.minor and
pre-releases use the full version, with an exact match required. Run
`vibe build` to regenerate locks from another version or the old lock format.
Locks built by an rc.6 binary are stamped `1.0.0-rc.6` and must be rebuilt with the matching binary using `vibe build`.

## Status

See [docs/status.md](status.md) and [CHANGELOG.md](../CHANGELOG.md).

## Development build keys, slugs and migrations

Build also creates `.vibe/signing.key` and `.vibe/signing.pub` for development
(release builds need no key), validates and lists slugs and disabled endpoints,
and always runs the secret scan, failing on findings. Edit `.vibe/slugs.json`
and `.vibe/disabled.txt` directly; restart to apply disabled-endpoint changes.
PostgreSQL startup applies pending migrations with owner credentials and advisory
locks; see [runtime configuration](runtime-config.md#automatic-migrations-decision-179)
for roles, tenant selection, append-only tracking and destructive-migration recovery.

## CI/CD

Required CI checks are **Markdown lint & link check** and **Rust fmt / clippy / test**. There is no Docker image job.

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust 1.89 `cargo check --locked`, `fmt`, `clippy`, and `test` once a `Cargo.toml` exists.
- **Release workflow** (`.github/workflows/release.yml`, on tags `v*`): is configured to build archives for Linux x86_64 and aarch64, macOS x86_64 and aarch64, and Windows x86_64 (`tar.gz` on Unix, `zip` on Windows) plus `SHA256SUMS` for pre-releases. Nothing is pushed to GHCR or crates.io.
- **CD** (`.github/workflows/cd.yml`): GHCR image publishing is disabled.

## Binary installation

The install/upgrade/remove manager in [docs/install.md](install.md) installs verified pre-release assets. It checks SHA256SUMS and GitHub build provenance attestations before installation; see the guide for verification requirements and source-build alternatives.

## Global LLM configuration (decision 185)

Development `vibe`, `vibe build` and `vibe build --check` can reuse personal LLM
settings across projects. Settings live in `$XDG_CONFIG_HOME/vibe/llm.env` when
`XDG_CONFIG_HOME` is nonempty and absolute, otherwise `$HOME/.config/vibe/llm.env`,
with the API key in a separate private file. Process environment settings take
precedence over project `.env`, which takes precedence over global settings.
Global settings are ignored by runtime, release builds, CI and non-development
environments. See [the detailed configuration reference](runtime-config.md#global-llm-configuration-decision-185)
for allowed keys, permissions, first-run behavior, precedence and diagnostics.

## Migrating to flat endpoint files (decision 186)

Starting with rc.5, endpoints live at `api/<definition>/<version>/<endpoint>.md`.
The file name determines the route; endpoint ids and routes stay the same.
Optional schemas and generated locks are siblings: `<endpoint>.schema.md` and
`<endpoint>.lock.md`. Run this POSIX sh snippet from the project root:

```sh
for folder in api/*/*/*; do
    [ -d "$folder" ] || continue
    name=${folder##*/}
    version=${folder%/*}
    if [ -f "$folder/$name.md" ]; then
        git mv "$folder/$name.md" "$version/$name.md" || exit 1
    fi
    if [ -f "$folder/schema.md" ]; then
        git mv "$folder/schema.md" "$version/$name.schema.md" || exit 1
    fi
    rm -f "$folder/$name.lock.md"
    rmdir "$folder" || exit 1
done
```

`vibe build` lists every file still to move, with its exact new path.
Rebuild to generate the new sibling locks.

## Project route settings (decision 187)

`api/_project.md` is optional and is parsed deterministically, never compiled by the LLM. When the file is omitted, empty, `# project`, or `## Routes` followed by `(none)` with no key overrides, routes use `prefix: /api` and `default-slug: dev`. A public `api/web/v1/counter.md` is then `/api/web/v1/counter`, with `tenant: default-slug`:

```markdown
# project

## Routes

- prefix: /api
- default-slug: dev
```

Set `- prefix: (none)` for no prefix. Set `- default-slug: (none)` for multi-tenant routes (`/{definition}/{version}/{slug}/<endpoint>` and `tenant: path.slug`). Omitting a key keeps that default. An empty `prefix` value clears the prefix; an empty `default-slug` value keeps `dev`. A bare `(none)` under `## Routes` does not opt out. Unknown sections or keys are errors. Underscore files are reserved; only `_project.md` is accepted directly under `api/`.

The prefix must start with `/`, have no trailing slash or empty segments, and total at most 64 bytes. Each segment matches `^[a-z0-9][a-z0-9-]*$`; the first cannot be `health` or `_internal`. It is applied only to manifest routes, never sent to the LLM or stored in locks. Changing only the prefix updates the manifest without recompiling features.

`default-slug` uses the same validation as `slugs.json` and is single-tenant only. Remove other slugs from both `.vibe/slugs.json` and `slugs.example.json`, remove `{slug}` from routes, and use `tenant: default-slug` for public routes; conflicts fail the build. Set `default-slug: (none)` to keep multi-tenant `{slug}` routes. Changing slug mode invalidates compiled locks and caches. The compiler and interactive authoring prompts receive the project route rule as input data (rc.5). The signed manifest records `route_prefix` and `default_slug`. An empty prefix and an absent default slug (the `(none)` opt-out) are omitted. Runtime and verification reject routes inconsistent with these fields.

See [runtime configuration](runtime-config.md) for tenant permissions and development defaults.

## LLM replay and recording (decision 189)

Build reviewed natural-language sources without credentials or network access:

```sh
LLM_PROVIDER=replay vibe build
LLM_PROVIDER=replay vibe build --check
```

Fixtures live in project-relative `fixtures/llm/`; `VIBE_LLM_FIXTURES` overrides
this with a project-relative or absolute directory. Each `<key>.md` is the raw
response; `index.json` is a sorted, pretty JSON array of `{key, source, purpose}`.
Source is project-relative; purpose is `compile`, `repair-<n>` or `translate`.
Review and commit these responses, not generated `*.lock.md` files.

The key is lowercase hex SHA-256 of the concatenation of VIBE_VERSION, decimal
LOCK_VERSION, `LlmRequest.system`, and the full `LlmRequest.input`. Each UTF-8
string is prefixed with its byte length as an eight-byte big-endian integer.
Host, model, credentials, URL, seed, timeouts and timestamps are excluded.
Prompt or SDK version changes therefore require new recordings.

Record locally with your configured real provider (or `fake` for supported tests):

```sh
VIBE_LLM_RECORD=fixtures/llm LLM_PROVIDER=openai vibe build --check
```

Use a clean checkout without generated locks to record every call, or use
`--check`. Recording stores only response text and index fields, never request
headers or connection metadata. It scans both payloads before writing for the
configured API key, base URL and endpoint host (host case-insensitively), and
refuses a match without printing the value. Build and check also scan existing
fixtures using credentials from environment, project `.env` and enabled global
settings. Empty values are ignored. Replay misses identify source and purpose
and show a recording command; unused index entries and response files warn.
Recording is refused in CI; recording and replay are refused with `--release`.
Replay manifests carry `test_build`, just like fake builds.
