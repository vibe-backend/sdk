# Implementation status

Moved from the README. Reflects the current source tree; nothing has been published as a release yet (see [CHANGELOG.md](../CHANGELOG.md)).

Implemented: discovery and source hashing, OpenAI-compatible / Anthropic / Gemini adapters, a fake adapter for tests, deterministic compile of structured markdown, lock rendering, the validator, local ed25519 approval, a signed manifest, JWT and API-key auth, built-in CRUD, offset pagination (platform `max-page-size` 500), soft delete, per-row `owner-key` scoping, a pooled PostgreSQL executor with schema-per-tenant RLS settings, PostgreSQL TLS (see [runtime-config.md](runtime-config.md)), method ops `query`, `verify-password`, `hash-password`, `random-token`, `hash-token` and `sign-jwt`, and server-side challenge verification with Cloudflare Turnstile (see [challenge.md](challenge.md)).

Deferred: platform KMS and GitHub review binding, refresh tokens, account lockout, pow challenges and hCaptcha, persisted quotas, cross-instance rate limits, crypto-shredding, the email outbox, webhooks, queues, schedules, and a connection pool per tenant.
