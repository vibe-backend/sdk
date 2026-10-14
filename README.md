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

Archives are named `vibe-<target>.tar.gz` (Linux, macOS) or `vibe-x86_64-pc-windows-msvc.zip` (Windows). Each release also has `SHA256SUMS`. On Linux or macOS:

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

Database roles, `VIBE_ENV`, LLM variables, JWT/JWKS, API keys and proxy settings: [docs/runtime-config.md](docs/runtime-config.md).

Locks are build output: they are not committed (`*.lock.md` is gitignored) and carry no approval or signature (spec v1.8). Deployments use only the manifest built and attested in the protected `release` environment (`vibe manifest --release`); see [docs/ci/README.md](docs/ci/README.md). Review happens in pull requests; see [docs/pr-review-guide.md](docs/pr-review-guide.md). `security-items` in a lock are informational. The manifest is still signed.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Status

See [docs/status.md](docs/status.md) and [CHANGELOG.md](CHANGELOG.md).

## CI/CD

Required checks: **Markdown lint & link check** and **Rust fmt / clippy / test**. Tags `v*` run `.github/workflows/release.yml`, which publishes binaries and `SHA256SUMS` to GitHub Releases.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
