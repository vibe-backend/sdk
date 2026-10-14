# vibe-backend SDK

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The HTTP runtime loads that manifest. It does not interpret natural language.

The spec snapshot in this repo is [docs/spec-v1.5.md](docs/spec-v1.5.md). This crate is the first working slice of the later contract: compile, validate, approve, and serve CRUD.

## Run the sample shop

From a checkout with Rust stable:

```bash
vibe compile --project examples/shop
vibe approve --project examples/shop \
  --reviewer ada@example.com --reviewer grace@example.com \
  --reason "sample shop locks"
vibe validate --project examples/shop --deploy
vibe manifest --project examples/shop -o manifest.json
vibe serve --manifest manifest.json --project examples/shop --memory
```

`--memory` is a dev executor. PostgreSQL uses `VIBE_DATABASE_URL` (or `DATABASE_URL`):

```bash
vibe migrate --manifest manifest.json --project examples/shop --tenant tenant0001
vibe serve --manifest manifest.json --project examples/shop
```

Structured markdown compiles with no model call. Natural-language sources need the variables in [.env.example](.env.example). `LLM_*` wins over `OPENAI_*`. Temperature is fixed at 0. Copy `.env.example` to `.env` (gitignored) and do not put a real key in the repo.

The runtime refuses to start when `LLM_*` or `OPENAI_*` is set. Public catalog reads resolve the tenant from `.vibe/slugs.json`. Staff writes need a JWT (`VIBE_JWT_PUBLIC_KEY_PEM`, `VIBE_JWT_ISS`, `VIBE_JWT_AUD`, `VIBE_JWT_KID`).

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

Implemented in 0.1.0: discovery and source hashing, OpenAI-compatible / Anthropic / Gemini adapters, a fake adapter for tests, deterministic compile of structured markdown, lock rendering, the validator, local ed25519 approval, a signed manifest, JWT and API-key auth, built-in CRUD, offset pagination, soft delete, and a single-connection Postgres executor with schema-per-tenant RLS settings.

Deferred: platform KMS and GitHub review binding, JWKS fetch, refresh tokens, account lockout, real captcha, persisted quotas, cross-instance rate limits, crypto-shredding, the email outbox, webhooks, queues, schedules, Postgres TLS, and a connection pool per tenant. Custom method steps other than `method`, `respond`, and `validate` are not executed. Challenge proofs are accepted when the header is non-empty.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## CI/CD

- **CI** (`.github/workflows/ci.yml`, on PRs and pushes to `main`): Markdown lint + local link check; Rust `fmt`/`clippy`/`test` once a `Cargo.toml` exists; Docker build (no push) once a `Dockerfile` exists.
- **CD** (`.github/workflows/cd.yml`, on pushes to `main` and `v*` tags): builds and pushes `ghcr.io/vibe-backend/sdk` from the root Dockerfile.
