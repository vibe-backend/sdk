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

**Breaking (#31):** `vibe manifest` without `-o` writes `./manifest.json` relative to the current working directory (same as `-o` / `serve --manifest`), not `<project>/manifest.json`.

`--memory` is a dev executor. PostgreSQL uses `VIBE_DATABASE_URL` (or `DATABASE_URL`):

```bash
vibe migrate --manifest manifest.json --project examples/shop --tenant tenant0001
vibe serve --manifest manifest.json --project examples/shop
```

Structured markdown compiles with no model call. Natural-language sources need the variables in [.env.example](.env.example). `LLM_*` wins over `OPENAI_*`. Sampling parameters are sent only when the model is in the adapter capability table. Copy `.env.example` to `.env` (gitignored) and do not put a real key in the repo.

The runtime refuses to start when `LLM_*` or `OPENAI_*` is set. Public catalog reads resolve the tenant from `.vibe/slugs.json`. Only `--memory` falls back to `slugs.example.json` when that file is missing; otherwise public slug routes return 404. `VIBE_ENV` (case-insensitive) is development only for `dev`, `development`, `local` or `test`; `ci` is CI, and any other value is production. Unset counts as development with `--memory` and as production with PostgreSQL. Outside development, `--memory` is refused and `VIBE_API_KEY_PEPPER` must be set to at least 32 bytes (not characters); generate one with `openssl rand -base64 48`. The sample shop maps slug `acme` to tenant `acme0001`. Add another with `vibe slug add <slug> --tenant <id>`. Staff writes need a JWT (`VIBE_JWT_PUBLIC_KEY_PEM`, `VIBE_JWT_ISS`, `VIBE_JWT_AUD`, `VIBE_JWT_KID`).

Per-IP rate limits and public-write quotas use the TCP peer address. Set `VIBE_TRUSTED_PROXIES` (comma-separated IPs or CIDR blocks, e.g. `10.0.0.0/8,2001:db8::/32`) behind a load balancer. `X-Forwarded-For` is honoured only from those proxies, and the rightmost hop that is not a trusted proxy counts. Hops may carry a port (`1.2.3.4:5678`, `[2001:db8::1]:443`), and IPv4-mapped IPv6 addresses count as IPv4. IPv6 clients are limited per /64. Rate and quota state is kept per route and key type with a fixed capacity; when a shard is full, its least recently used bucket is evicted, so flooding never blocks other clients.

`vibe approve` needs `--reason` and one `--reviewer` per reviewer (repeat the flag, e.g. `--reviewer a@x --reviewer b@x`). High-risk locks (public endpoints, providers, and migrations) need two reviewers. A failed approve writes nothing.

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
