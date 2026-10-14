# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### Breaking

- **Ownership (#53):** The implicit admin ownership bypass is removed. Models that need admin-wide access must declare `ownership: role(admin)`. `owner-key: id` is only allowed on user-like models.
- **Environment / slugs / pepper (#52):** Unknown `VIBE_ENV` values are treated as production. With PostgreSQL, unset `VIBE_ENV` is production. `--memory` is refused outside development. Outside development, `VIBE_API_KEY_PEPPER` is required (≥ 32 bytes). Missing, empty, or malformed `.vibe/slugs.json` fails closed (no silent fallback except `--memory` + `slugs.example.json`).
- **Manifest path (#31):** `vibe manifest` without `-o` writes `./manifest.json` relative to the current working directory, not `<project>/manifest.json`.
- **Migrate DB URL (#51, when merged):** Migrate requires `VIBE_MIGRATE_DATABASE_URL` and `VIBE_RUNTIME_DB_ROLE`; it no longer reads `VIBE_DATABASE_URL` for migrations.
- **Rate-limit keys (#54, when merged):** Account-based rate-limit keys are deprecated; some key families are rejected until auth flows (#26) land.
- **Approve / lock signing (v1.8):** Spec v1.8 removes `vibe approve` and lock signatures. Prefer `vibe build` / Git platform review. See `docs/spec-v1.8.md`.

### Security

- Release workflow hardening (#28): pinned Actions SHAs, build provenance, no release clobber, `release` environment (see `docs/release-hardening.md`).
