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

**Method ops (#26, part 1).** Method steps `query`, `verify-password`, `hash-password`, `random-token`, `hash-token` and `sign-jwt` now run. `query` runs the lock's parameterised SQL in the tenant schema under row level security, as `public_reader` (or `public_writer` for a step marked `writes: true`, via `SET LOCAL ROLE`; both database roles must exist and be granted to the runtime role), in a READ ONLY transaction unless the step writes. Lock SQL is checked with a PostgreSQL parser by `vibe validate` and again at runtime: exactly one SELECT, INSERT, UPDATE or DELETE; unqualified plain table names (no `pg_*`, `information_schema` or platform tables); no table functions; no `SELECT … INTO`; no casts to object-identifier types (`regclass`, `regproc`, …); only allowlisted functions (`src/sqlguard.rs`), so no `SET`, settings or dynamic SQL. A data-modifying `WITH` is rejected, and a unique violation (SQLSTATE 23505) returns 409 `CONFLICT`. `query` is PostgreSQL only: with `--memory`, `vibe serve` warns at startup and the step fails with "query steps need PostgreSQL; the --memory executor does not run them" (500). `verify-password` uses argon2id and only accepts hashes made with the platform's own parameters (m=65536, t=3, p=1) and passwords of at most 256 characters (at most four hashes at a time per instance; a request that waits more than 5 s gets 503). When the user does not exist (`missing-user: dummy-hash`) it compares against a dummy hash, built at startup, so timing and the result look the same as a wrong password. Endpoints that reach a `verify-password` step are padded to at least 500 ms. `validate` conditions are `X is empty`, `X is not empty`, `X == true|false|null` or `X != true|false|null` (code-span backticks are allowed); anything else is a validate error and, at runtime, fails closed (the step's `fail` fires), never a silent pass. Step expressions can read `ctx.user.sub`, `ctx.user.roles` and `ctx.user.tenant`. `random-token` returns `{value, hash}` for 32 random bytes; `hash` (and `hash-token`) is HMAC-SHA-256 under a key derived from `VIBE_API_KEY_PEPPER`, as a `bytea` literal (`\x…`) for a `bytea` column (`normalize: email` trims and lowercases first). `sign-jwt` runs only in methods with `capabilities: [sign-token]`. It reads the private PEM from the environment variable named by `key-ref` (`<key-ref>_KID` sets the kid, default `VIBE_JWT_KID`), and `vibe serve` refuses to start if that variable is missing. The runtime sets `iss`, `aud`, `iat`/`nbf`/`exp` (ttl at most 1 h), `jti`, `tenant_id` (always the request's tenant) and the roles claim, which it reads from `users.roles` for the token's `sub`; a lock cannot supply roles. `sub` must be a field of a row read by an earlier `query` step (for example `u.id`), checked by `vibe validate` and at runtime. Other claims must be listed in `custom-claims` and can never be a reserved claim or any configured roles claim. The step returns `{access_token, token_type, expires_in, expires_at}`. `issue-refresh` is refused until refresh tokens land.

## Install

There are currently no published tags, GitHub Releases, or downloadable release assets for this repository. The `vibe` CLI is not published to crates.io or as a container image. For now, build it from a source checkout with Rust 1.89:

```bash
cargo build --release --locked
./target/release/vibe --help
```

Do not use guessed release tags or `releases/download` URLs; they are not a supported installation path until an actual release has been published and verified.

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

Runtime configuration (PostgreSQL roles and TLS, connection pool, LLM variables, `VIBE_ENV`, JWT/JWKS and API keys, rate limits and trusted proxies) is in [docs/runtime-config.md](docs/runtime-config.md).

`vibe approve` needs `--reason` and one `--reviewer` per reviewer (repeat the flag, e.g. `--reviewer a@x --reviewer b@x`). High-risk locks (public endpoints, providers, and migrations) need two reviewers. A failed approve writes nothing.

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
