# vibe-backend SDK

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The HTTP runtime loads that manifest. It does not interpret natural language.

The specification is [docs/spec-v1.7.md](docs/spec-v1.7.md).

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

## Run the sample shop

From a checkout with Rust 1.89 (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` pins the same channel):

```bash
vibe compile --project examples/shop
vibe approve --project examples/shop \
  --reviewer ada@example.com --reviewer grace@example.com \
  --reason "sample shop locks"
vibe validate --project examples/shop --deploy
vibe manifest --project examples/shop -o manifest.json
vibe serve --manifest manifest.json --project examples/shop --memory
```

`--memory` is a dev executor. PostgreSQL needs two roles. `vibe migrate` connects with the owner role (`VIBE_MIGRATE_DATABASE_URL`) and grants SELECT, INSERT and UPDATE to the runtime role named in `VIBE_RUNTIME_DB_ROLE`. `vibe serve` connects as that runtime role (`VIBE_DATABASE_URL`, or `DATABASE_URL`) and refuses to start if the role is, or can `SET ROLE` to, a superuser or BYPASSRLS role, or can own any tenant schema, relation, function or type, because row-level security would not apply. `VIBE_RUNTIME_DB_ROLE` is required:

```bash
VIBE_MIGRATE_DATABASE_URL=postgres://owner@db/app VIBE_RUNTIME_DB_ROLE=vibe_app \
  vibe migrate --manifest manifest.json --project examples/shop --tenant tenant0001
VIBE_DATABASE_URL=postgres://vibe_app@db/app \
  vibe serve --manifest manifest.json --project examples/shop
```

Structured markdown compiles with no model call. Natural-language sources need the variables in [.env.example](.env.example). `LLM_*` wins over `OPENAI_*`. Sampling parameters are sent only when the model is in the adapter capability table. Copy `.env.example` to `.env` (gitignored) and do not put a real key in the repo.

The runtime refuses to start when `LLM_*` or `OPENAI_*` is set. Public catalog reads resolve the tenant from `.vibe/slugs.json`, or from `slugs.example.json` in the project when that file is missing. The sample shop maps slug `acme` to tenant `acme0001`. Add another with `vibe slug add <slug> --tenant <id>`. Staff writes need a JWT (`VIBE_JWT_PUBLIC_KEY_PEM`, `VIBE_JWT_ISS`, `VIBE_JWT_AUD`, `VIBE_JWT_KID`).

`vibe approve --reviewer` is repeatable. High-risk locks (public endpoints, providers, and migrations) need two reviewers. A failed approve writes nothing.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

Implemented in 0.1.0: discovery and source hashing, OpenAI-compatible / Anthropic / Gemini adapters, a fake adapter for tests, deterministic compile of structured markdown, lock rendering, the validator, local ed25519 approval, a signed manifest, JWT and API-key auth, built-in CRUD, offset pagination (platform `max-page-size` 500), soft delete, per-row `owner-key` scoping, and a single-connection Postgres executor with schema-per-tenant RLS settings.

Deferred: platform KMS and GitHub review binding, JWKS fetch, refresh tokens, account lockout, real captcha, persisted quotas, cross-instance rate limits, crypto-shredding, the email outbox, webhooks, queues, schedules, Postgres TLS, and a connection pool per tenant. Custom method steps other than `method`, `respond`, and `validate` are not executed. Challenge proofs are accepted when the header is non-empty.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## CI/CD

Required CI checks are **Markdown lint & link check** and **Rust fmt / clippy / test**. There is no Docker image job.

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust 1.89 `cargo check --locked`, `fmt`, `clippy`, and `test` once a `Cargo.toml` exists.
- **Release** (`.github/workflows/release.yml`, on tags `v*`): builds `vibe` for Linux x86_64 and aarch64, macOS x86_64 and aarch64, and Windows x86_64. Archives are `tar.gz` (Unix) or `zip` (Windows), plus `SHA256SUMS`, on a GitHub Release. Nothing is pushed to GHCR or crates.io.
- **CD** (`.github/workflows/cd.yml`): GHCR image publishing is disabled.
