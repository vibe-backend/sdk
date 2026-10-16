# vibe-backend SDK

Rust SDK and `vibe` CLI for a markdown-driven serverless backend. Source files under `providers/`, `models/`, `migrations/`, and `api/` compile into structured `{name}.lock.md` files and a signed manifest. The HTTP runtime loads that manifest. It does not interpret natural language.

The specification is [docs/spec-v1.7.md](docs/spec-v1.7.md). The draft [docs/spec-v1.8.md](docs/spec-v1.8.md) is under review.

## v1.8 設計（實作中）

The commands below are the v1.8 design and are not implemented yet. Until then, use the steps in the next sections.

```bash
vibe dev
curl http://localhost:8080/catalog/v1/acme/items
```

`vibe dev` will use an in-memory database for development and testing only; all data is lost when the server stops. It enforces tenant isolation and the role and column grants the same way PostgreSQL does for the SQL vibe generates (including the SELECT privilege PostgreSQL needs for columns in `WHERE` and `RETURNING`), and refuses with `MEMORY_UNSUPPORTED_SQL` anything it cannot model: any RLS setting or policy other than the standard tenant policy, and grants for other roles.

Updates need `If-Match`: the api lock must map `req.header.if-match` to the method input `if_match` in its request table. Without that mapping the method never sees the header, so updates through that api cannot pass the version check.

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

`--memory` is a dev executor. With PostgreSQL every tenant has its own database roles (spec decision 97, #98); there is no shared runtime login and no `SET ROLE`. An operator pre-creates, per tenant `<id>` (a system-generated opaque id, `^[a-z0-9_]{8,61}$`; a derived role name over 63 bytes is refused, never truncated):

- three login roles, `tenant_<id>_rw`, `tenant_<id>_public_reader` and `tenant_<id>_public_writer`, each `LOGIN NOINHERIT PASSWORD '...'` with no other attributes and no role memberships in either direction;
- the migration role `tenant_<id>_owner`, which owns the tenant's schema (`CREATE ROLE tenant_<id>_owner LOGIN` plus `GRANT CREATE ON DATABASE app TO tenant_<id>_owner`; it must not be a superuser, BYPASSRLS, or a member of any other tenant's roles, and should have no `CREATE` privilege in any other schema, for example revoke `CREATE` on schema `public`).

`vibe migrate` connects as `tenant_<id>_owner` (`VIBE_MIGRATE_DATABASE_URL`) and refuses any other login by name. It also refuses to run if that login can own or use another tenant's schema, if the three tenant roles are missing or unsafe, or if a runtime role owns anything. After migrating it verifies, in the same transaction, that the schema is owned by `tenant_<id>_owner`, the runtime roles own nothing and every table has FORCE ROW LEVEL SECURITY; otherwise nothing is committed. It creates `t_<id>`, gives only that tenant's three roles USAGE and exactly the table and column grants from the model locks (`## Policies` `grants:`), and installs on every table ENABLE + FORCE row-level security with exactly the policy the compiler writes into each model lock (`policy: tenant_isolation TO tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer USING (tenant_id = '<id>') WITH CHECK (tenant_id = '<id>')`, with `<id>` substituted). `app.tenant_id` is still set per transaction, but only for logs; it is never part of an access decision.

`vibe serve` opens one pool per (tenant, role class) and logs in directly as the tenant role: `VIBE_DATABASE_URL` (or `DATABASE_URL`) names the server and database (the user in the URL is ignored). Credentials:

- **Production:** `VIBE_TENANT_DB_SECRET_REF=env:<PREFIX>`, a secret-manager reference. The password of `tenant_acme0001_rw` is read from the environment variable `<PREFIX>TENANT_ACME0001_RW`, injected by your secret manager (Kubernetes/ECS/Cloud Run secrets, Vault agent, ...). Other secret managers plug in through the `TenantSecretSource` trait. In production `vibe serve` refuses to start if `VIBE_TENANT_DB_SECRETS` is set or a file exists at `.vibe/tenant-db-secrets.json`.
- **Development and tests only:** `VIBE_TENANT_DB_SECRETS=<path>`, a JSON object mapping each tenant role to its password (`{"tenant_acme0001_rw": "..."}`). The file must be mode 0600 (or stricter); looser permissions are refused. It is gitignored; never commit it.

Passwords are never logged or included in errors. At startup it logs in as every role in that file and refuses to start unless the role is exactly the expected tenant role, NOINHERIT, not a superuser, without BYPASSRLS, has no role memberships (so `SET ROLE` reaches nothing), owns nothing, can reach no schema or relation outside `t_<id>` (other tenants, shared tables in `public`, CREATE anywhere), and every relation in `t_<id>` has ENABLE + FORCE RLS and only the canonical policy. It also compares each model's compiled lock policy with the policy installed in the database (`pg_policies`) for every tenant, and refuses to start on any mismatch. Each statement also checks that the connection is logged in as the right role. A statement the role is not allowed to run returns 403 `FORBIDDEN`; the details are only logged.

```bash
VIBE_MIGRATE_DATABASE_URL=postgres://tenant_tenant0001_owner@db/app \
  vibe migrate --manifest manifest.json --project tests/fixtures/shop --tenant tenant0001
VIBE_DATABASE_URL=postgres://db/app VIBE_TENANT_DB_SECRET_REF=env:VIBE_TDB_ \
  vibe serve --manifest manifest.json --project tests/fixtures/shop
```

`vibe serve` uses one connection pool per tenant role (`VIBE_DB_POOL_SIZE`, default 16, capped at 5 per pool, spec §7.1). `VIBE_DB_MAX_CONNECTIONS` (default 200) caps connections in use across all pools; above it a request is refused at once with 503 `DB_POOL_EXHAUSTED` and `Retry-After: 1`, and the `pool_exhausted_total` counter is incremented. Idle pools are not closed yet (#165, GA blocker). It waits up to `VIBE_DB_POOL_TIMEOUT_MS` (default 5000) for a free connection, and each request transaction runs with `statement_timeout` = `VIBE_DB_STATEMENT_TIMEOUT_MS` (default 5000). Connections to hosts other than localhost or loopback require TLS (certificate and hostname verified against the webpki roots) unless the URL says `sslmode=disable`. `sslmode=require`, `verify-ca` and `verify-full` always use verified TLS.

Structured markdown compiles with no model call. Natural-language sources need the variables in [.env.example](.env.example). `LLM_*` wins over `OPENAI_*`. Sampling parameters are sent only for models in the adapter capability registry (`src/llm.rs`): Gemini 1.5 / 2.0 / 2.5 text models get `temperature: 0`, `topP: 1`, `topK: 1` and the seed (not tts, image, live or audio variants); OpenAI `gpt-5.1` / `gpt-5.2` get `temperature: 0`, `top_p: 1`, the seed and `reasoning_effort: none` (not `-pro`, `-codex` or `-chat` variants); OpenAI `gpt-4.1` / `gpt-4o` get `temperature: 0`, `top_p: 1` and the seed (not search, audio, realtime, transcribe or tts variants); exactly these Claude models get `temperature: 0` only (never together with `top_p`): Opus 4, 4.1, 4.5 and 4.6; Sonnet 4, 4.5 and 4.6; Haiku 4.5; Claude 3.7 Sonnet, 3.5 Sonnet, 3.5 Haiku, 3 Opus and 3 Haiku (each as the bare id or with a `-YYYYMMDD`, `-YYYY-MM-DD` or `-latest` suffix). Gemini 3 and later get none, because Google recommends the default temperature of 1.0 (lower values can loop). Every other Claude model (Opus 4.7 and later, Sonnet 5 and later, any newer Haiku, Claude 2.x) gets none; the newer ones reject non-default sampling parameters. A registered id must be followed by `-` or the end of the name, so `gpt-4.10` or `claude-opus-4-10` does not match. With `LLM_API_STYLE=responses` the seed is not sent. Other models and providers get none, and `vibe compile` warns. Set `LLM_SEND_SAMPLING=0` to send none even for registered models. Copy `.env.example` to `.env` (gitignored) and do not put a real key in the repo.

The runtime refuses to start when `LLM_*` or `OPENAI_*` is set. Public catalog reads resolve the tenant from `.vibe/slugs.json`. Only `--memory` falls back to `slugs.example.json` when that file is missing; otherwise public slug routes return 404. `VIBE_ENV` (case-insensitive) is development only for `dev`, `development`, `local` or `test`; `ci` is CI, and any other value is production. Unset counts as development with `--memory` and as production with PostgreSQL. Outside development, `--memory` is refused and `VIBE_API_KEY_PEPPER` must be set to at least 32 bytes (not characters); generate one with `openssl rand -base64 48`. The test fixture's [slugs.example.json](tests/fixtures/shop/slugs.example.json) maps slug `acme` to tenant `acme0001`. Add another with `vibe slug add <slug> --tenant <id>`. Staff writes need a JWT (`VIBE_JWT_PUBLIC_KEY_PEM`, `VIBE_JWT_ISS`, `VIBE_JWT_AUD`, `VIBE_JWT_KID`). The PEM's algorithm is detected (ES256 or RS256); override with `VIBE_JWT_ALG` if needed. To use an identity provider's keys instead, set `VIBE_JWT_JWKS_URI` (https only — an `http://` issuer or JWKS URI refuses to start). Do not set PEM and JWKS together. If no PEM is set, the default is `{VIBE_JWT_ISS}/.well-known/jwks.json`. Keys are cached for 10 minutes, an unknown `kid` triggers a refetch at most once a minute (single-flight; other requests keep using the cache), and a failed fetch falls back to the old keys for at most an hour. For local JWKS experiments, point `VIBE_JWT_JWKS_URI` at an https endpoint that serves a static JWKS (a tunnel or a local reverse proxy with TLS); plain `http://localhost` is refused. `VIBE_JWT_ROLES_CLAIM` (default `roles`) names the roles claim (dotted paths like `realm_access.roles` are supported); a route's auth `roles-claim` overrides it. Reserved claims (`aud`, `scope`, …) are rejected. A route's auth block may also set `iss`, `aud` and `jwks-uri` (spec §4.1) to pin a different identity provider: `iss` and `jwks-uri` must be https, `jwks-uri` needs `iss`, and without `jwks-uri` the keys come from `{iss}/.well-known/jwks.json`. Route `iss`/`jwks-uri` hosts must be public: loopback, private, link-local (for example `169.254.169.254`), CGNAT and `.local`/`.internal` names are refused by `vibe validate` and at startup, with no development exception for lock values. Hosts are normalized before the check (one trailing dot stripped, lower-cased), so `localhost.` or `idp.internal.` are refused too, and names with an empty label (such as `localhost..`) are refused; IPv4-compatible, IPv4-translated (`::ffff:0:0/96`), discard (`100::/64`) and NAT64/6to4 IPv6 forms, site-local, multicast and reserved ranges are also refused. Only the env JWKS may point at a private host, and only with `VIBE_JWT_ALLOW_PRIVATE_HOST=true` in development; this applies both to `VIBE_JWT_JWKS_URI` and to the default `{VIBE_JWT_ISS}/.well-known/jwks.json` derived from `VIBE_JWT_ISS`. Limitation: the check looks at the URL's host as written. A public DNS name that resolves to a private address is not refused yet. Each URI gets its own cache, created at startup (an invalid one refuses to start); such a route accepts only its own issuer's keys and never falls back to the env keys, and an unreachable route JWKS returns 401. A JWT `scope` (space-separated string or array) or `scp` claim, and API-key scopes, are checked against the route's `scopes`; the caller must hold all of them. Empty route `scopes` means scopes are opt-in (not required). A malformed lock `roles:` or `scopes:` list (for example `roles: staff` without brackets) is a build error, never an empty allow-all. API keys are loaded from `VIBE_API_KEYS_FILE`: a JSON array of `{key_id, secret_hash, env, tenant_id, scopes, expires_unix, revoked}` records, where `secret_hash` is the HMAC of the secret with `VIBE_API_KEY_PEPPER` (see `hash_api_secret` / mint with a 12-char base32 `key_id` and `mbk_{live|test}_{key_id}_{secret}`). The file must be `chmod 600` and is re-read when its mtime changes (at most once a second); a bad reload keeps the previous keys. Keys are `mbk_live_…` when `VIBE_ENV` is production, and `mbk_test_…` otherwise. Auth failures return a generic 401; enable debug logging to see the operator-facing reason.

Per-IP rate limits and public-write quotas use the TCP peer address. Set `VIBE_TRUSTED_PROXIES` (comma-separated IPs or CIDR blocks, e.g. `10.0.0.0/8,2001:db8::/32`) behind a load balancer. `X-Forwarded-For` is honoured only from those proxies, and the rightmost hop that is not a trusted proxy counts. Hops may carry a port (`1.2.3.4:5678`, `[2001:db8::1]:443`), and IPv4-mapped IPv6 addresses count as IPv4. IPv6 clients are limited per /64. Rate and quota state is kept per route and key type with a fixed capacity; when a shard is full, its least recently used bucket is evicted, so flooding never blocks other clients. Buckets at their limit stay until their one-minute window ends, so each client network (an IPv4 address or an IPv6 /48) may create at most 100 active buckets per route and key type, and each wider network (IPv4 /24, IPv6 /32) at most 1,000; past that, only that network gets 429 for new keys. The last 1,000 of a shard's 10,000 slots are reserved for small clients: for IPv4, fewer than 5 buckets per address and fewer than 10 per /24; for IPv6, fewer than 10 new keys per /48 and fewer than 50 in total per /32. New clients usually still get in after heavy clients use up the rest, but this is a mitigation, not a guarantee. Clients sharing one address (CGNAT, or a proxy when `VIBE_TRUSTED_PROXIES` is unset) share that address's 100. A distributed client can still fill a shard and lock out new clients on that route, for example about 100 IPv4 /24s with 2 addresses each, or a few dozen IPv6 /32s (on top of the networks that fill the non-reserve slots); see [docs/release-hardening.md](docs/release-hardening.md). Use load-balancer or CDN rate limiting as well. Saturated shards are logged as warnings.

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
