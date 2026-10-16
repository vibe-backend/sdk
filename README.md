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
vibe keygen --project tests/fixtures/shop
vibe compile --project tests/fixtures/shop
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

Locks are build output: they are not committed (`*.lock.md` is gitignored) and carry no approval or signature (spec v1.8). Deployments use only the manifest built and attested in the protected `release` environment (`vibe build`, which also writes the compiled locks to a local build directory, `--locks-dir`, default `<project>/.vibe/build/locks`, never to the repo; add `.vibe/` to your `.gitignore`). High-risk release builds fail closed until release-environment approval verification (#80) is implemented; see [docs/ci/README.md](docs/ci/README.md). Review happens in pull requests; see [docs/pr-review-guide.md](docs/pr-review-guide.md). `security-items` in a lock are informational. The manifest is still signed.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

Implemented in 0.1.0: discovery and source hashing, OpenAI-compatible / Anthropic / Gemini adapters, a fake adapter for tests, deterministic compile of structured markdown, lock rendering, the validator, an ed25519-signed manifest, JWT and API-key auth, built-in CRUD, offset pagination (platform `max-page-size` 500), soft delete, per-row `owner-key` scoping, and a single-connection Postgres executor with schema-per-tenant RLS settings.

Deferred: platform KMS and GitHub review binding, refresh tokens, account lockout, real captcha, persisted quotas, cross-instance rate limits, crypto-shredding, the email outbox, webhooks, queues, schedules, Postgres TLS, and a connection pool per tenant. Custom method steps other than `method`, `respond`, and `validate` are not executed. Challenge proofs are accepted when the header is non-empty.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## CI/CD

Required CI checks are **Markdown lint & link check** and **Rust fmt / clippy / test**. There is no Docker image job.

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust 1.89 `cargo check --locked`, `fmt`, `clippy`, and `test` once a `Cargo.toml` exists.
- **Release** (`.github/workflows/release.yml`, on tags `v*`): builds `vibe` for Linux x86_64 and aarch64, macOS x86_64 and aarch64, and Windows x86_64. Archives are `tar.gz` (Unix) or `zip` (Windows), plus `SHA256SUMS`, on a GitHub Release. Nothing is pushed to GHCR or crates.io.
- **CD** (`.github/workflows/cd.yml`): GHCR image publishing is disabled.
