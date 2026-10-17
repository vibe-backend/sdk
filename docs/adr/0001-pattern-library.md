# ADR 0001: Best-practice pattern library

- **Status:** Proposed (draft, not yet reviewed). The PM hard rules below were set by the PM on 2026-10-17 09:59 HKT; the §9 points were approved by the PM on 2026-10-17 10:08 HKT (decisions 158–163, [record](https://github.com/vibe-backend/sdk/pull/274#issuecomment-6228340971)); P8–P10 on 10:21 HKT ([record](https://github.com/vibe-backend/sdk/pull/274#issuecomment-6228450139)); D166 and D167 are drafts pending PM confirmation. This ADR feeds the v1.1 spec and does not block any v1 blocker.
- **Date:** 2026-10-17
- **Spec:** [`../spec-pattern-library-v1.1.md`](../spec-pattern-library-v1.1.md) (decisions 150–168)
- **Related:** decisions 79, 113, 117, 118, 119 (provenance, lock as build output, Sigstore keyless), 149 (read-only sign-method); PRs #147, #95.

This ADR describes defensive design only.

## Context

The compiler turns natural-language Markdown into a lock and then into an executable artifact. Many endpoints follow well-known patterns. Today all of them are built into the SDK core:

- **Security patterns** (auth, rate limiting, challenge, RLS, refresh token) define the security boundary. They must be released and reviewed together with the SDK.
- **Business patterns** (CRUD, pagination, search, upload, email) change faster and do not define the security boundary. Shipping them with the core couples their release cadence to it.

## Decision

1. **Two kinds of pattern.** Security patterns stay in the core (`source: builtin`). In v1.1, business patterns are split into separately versioned packages, and `vibe compile` fetches only the ones an endpoint needs.
2. **v1 uses the same interface internally.** In v1 every pattern is built in, but it is written behind the same manifest and trait, so that v1.1 only changes where business patterns come from.
3. **Manifest.** Each pattern has `id`, a semver `version`, `kind: security | business`, `provides` (capabilities) and `requires` (DB privileges, outbound network defaulting to none with an allowlist, secret names, and the security pipelines it depends on).
4. **Trait.** `manifest()`, `matches(endpoint AST)`, and `plan(endpoint, ctx) -> IR fragment`. A pattern produces IR only.
5. **Selection.** Markdown → AST → required capabilities → candidate patterns. Security patterns are always applied. If more than one business pattern matches, compilation fails, and the author picks one explicitly with `use: <id>@<range>`. There is no implicit scoring.
6. **Provenance.** Registry packages are verified with Sigstore against a pinned signer identity (issuer + SAN for one exact repo and workflow). Verification works offline (bundle plus a cached trust root) and fails closed. `vibe serve` never downloads patterns.
7. **Lock.** The lock records the core SDK version and hash, compiler version, IR schema version, every pattern used (id, version, source, artifact sha256, Sigstore bundle details), and, per endpoint, which pattern provided each capability.

## PM hard rules (2026-10-17 09:59 HKT)

1. A pattern MUST only output IR and MUST NOT run its own code at runtime.
2. `kind: security` MUST only be accepted from `source: builtin`; a pattern's `requires` MUST be a subset of what the endpoint declares.
3. More than one match MUST be a compile error, resolved only by an explicit `use:`; there MUST NOT be implicit scoring.
4. The signer identity MUST be pinned to an exact repo and workflow (issuer + SAN). Offline verification (bundle + cached trust root) MUST be supported so deploys still work when Sigstore/Rekor is down. Verification failure MUST fail closed. A `vibe serve` cold start MUST NOT download anything.
5. The v1 internal refactor onto the same interface MUST wait until #147, #95 and the D149 implementation have merged, MUST be additive with no behaviour change, and MUST be scheduled at the end of the GA plan.

## Draft decisions pending PM confirmation (P11, P12)

- Pattern IR rules (D166, P11): the core MUST enforce RLS and `owner-key` on every pattern IR, a pattern MUST NOT choose or switch DB roles (the executor decides), and a secret MUST only be sent to a host declared in `egress:`. Violations are compile errors.
- `use:` (D167, P12) is a structured endpoint frontmatter field, consistent with #92.

## PM decisions (2026-10-17 10:21 HKT)

1. Pins MUST live in a committed `patterns.md` (exact version + sha256); a mismatch MUST fail closed; `use:` MUST only choose a declared version (D164).
2. Endpoint `egress:` and `secrets:` are added in v1.1 and count as empty in v1; a pattern's egress and secrets MUST be a subset of the endpoint's; `egress:` MUST list explicit hosts, and a wildcard is a validate error (D165).
3. With no local verified withdrawal list, compile MUST start from the signed list built into the core release, and MUST fail closed only when no built-in list exists (D168).

## Consequences

- Positive: business patterns can be released on their own schedule; the security boundary stays in reviewed core code; every compiled endpoint is traceable to exact pattern versions.
- Negative: a registry and its signing workflow must be operated; compile needs network access or a pre-populated cache for business patterns; the IR schema becomes a versioned contract.
- Neutral for v1: no behaviour change; locks gain fields with `source: builtin`.

## Alternatives considered

- **Keep everything in the core.** Simplest, but business patterns stay coupled to the core release cycle.
- **Run pattern code at runtime (plugins).** Rejected: it widens the runtime trust boundary (rule 1).
- **Score multiple matches and pick the best.** Rejected: not deterministic or reviewable (rule 3).
- **Allow third-party security patterns.** Rejected: the security boundary must stay in the reviewed core (rule 2).

## PM decisions (2026-10-17 10:08 HKT)

1. The registry MUST accept only packages signed by one official repo and its release workflow, with SAN refs limited to `refs/tags/v*`; third-party patterns wait for vNext (D158).
2. The cached trust root MUST be updated with core releases, as in D119 (D159).
3. Offline compile MUST use a pre-filled local cache or mirror whose packages pass full verification and the withdrawal-list check (D160).
4. The withdrawal list MUST be signed by the same pinned identity, MUST carry a version and an expiry, and compile MUST reject a list older than the cached one; after expiry compile MUST warn and keep applying the latest verified list (D161).
5. The core MUST accept only the same major IR schema version (D162).
6. After a business pattern is split out, the builtin copy MUST stay for one minor release; while both exist, `use:` is required (D163).
