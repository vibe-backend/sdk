# Changelog

All notable changes to this project are documented in this file. Entries are
added only when the corresponding PR is merged to `main`.

## Unreleased

### Breaking

- **Ownership (#53):** The implicit admin ownership bypass is removed. Models that need admin-wide access must declare `ownership: role(admin)`. `owner-key: id` is only allowed on user-like models.
- **Environment / slugs / pepper (#52):** Unknown `VIBE_ENV` values are treated as production. With PostgreSQL, unset `VIBE_ENV` is production. `--memory` is refused outside development. Outside development, `VIBE_API_KEY_PEPPER` is required (≥ 32 bytes). Missing, empty, or malformed `.vibe/slugs.json` fails closed (only `--memory` falls back to `slugs.example.json`).

<!--
Pending — add each entry only when its PR merges:
- #51: migrate requires VIBE_MIGRATE_DATABASE_URL and VIBE_RUNTIME_DB_ROLE; no longer reads VIBE_DATABASE_URL.
- #54: rate-limit key changes (account deprecated, family rejected until #26).
- #62: `vibe manifest` without -o writes ./manifest.json in the current directory.
- #61: stricter lock validation — unknown step keys and unparseable expressions are errors;
  API validate steps need `when` and `fail` and now actually execute; write SQL needs
  `writes: true` and `writes: true` needs a write; SELECT INTO counts as a write and as DDL
  outside migrations; unparseable or inverted min/max rules are errors.
-->
