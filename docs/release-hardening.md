# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (#70, live: ruleset 25067953 "release tags v*")** — Target **tags** matching `refs/tags/v*`, enforcement active, rules: `update`, `deletion`, `non_fast_forward` (block moving, deleting and force-pushing tags). There is **no creation rule**: anyone with write access can create a `v*` tag (the product manager, on Jack's behalf, chose option (i)). `bypass_actors` is empty, so not even organization admins can move or delete a published tag.
   - Gating today: creating a tag does not publish anything; the publishing job runs in the `release` environment, whose only reviewer is `j113203` with `prevent_self_review: false`.
   - **Residual risk (accepted by Jack):** all agents push as `j113203`, so any `j113203` agent can create a `v*` tag **and self-approve the release**.
   - `vibe build --release-approval` (spec §2C.3a) was **withdrawn** by the product manager (2026-10-16); the only human approval gate is Jack approving the `release` environment; the automatic gates remain decisions 113/114/118.
   - Option (i) is a **temporary** scheme. Target (iii), not in effect: a dedicated release GitHub App is the sole `v*` tag creator, and `prevent_self_review` is turned on.
   - The existing `main` ruleset (25057946, "main protection") keeps its OrganizationAdmin bypass; it does not apply to `v*` tags (spec decision 96). Nothing in the SDK compensates for this bypass (see residual risks).
2. **Environment `release`** — Settings → Environments → `release`. Live: reviewer `j113203`, `prevent_self_review: false` (all agents push as `j113203`), deployments limited to the `v*` tag pattern. Administrator bypass stays **enabled** (`can_admins_bypass: true`): Jack decided to keep it and accepts the risk that an admin can skip the environment approval. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.

## Rate-limit capacity limits (#71, #73)

The in-memory limiter keeps at most 10,000 active buckets per `(route, key type)` shard (`SHARD_CAPACITY`). A single client network may own at most 100 of them (`OWNER_CAP`: one IPv4 address or one IPv6 /48), and a wider network at most 1,000 (`NETWORK_CAP`: one IPv4 /24 or IPv6 /32). The last 1,000 slots (`RESERVE`) only go to clients below the reserve caps:

- IPv4: fewer than 5 buckets per address (`RESERVE_OWNER_CAP`) and fewer than 10 per /24 (`RESERVE_NETWORK_CAP`).
- IPv6: two tiers. Fewer than 10 new keys per /48 (`RESERVE_V6_48_CAP`) and fewer than 50 in total per /32 across all of its /48s (`RESERVE_V6_32_CAP`).

**This is a mitigation, not a guarantee.** The caps bound one network, not a distributed client. Cases that can still fill a shard and lock out new clients on that route:

- IPv4: 9 /24s at `NETWORK_CAP` take the 9,000 non-reserve slots, then about 100 /24s with 2 addresses each (5 buckets per address, 10 per /24) fill the 1,000 reserve slots.
- IPv6: many /32s. 9 /32s at `NETWORK_CAP` take the non-reserve slots, then 20 /32s with 5 /48s each (10 per /48, 50 per /32) fill the reserve; an attacker with addresses in that many /32s (or a mix of /32s and /24s) can do it.
- Many clients behind one address or /24 (CGNAT, or a proxy when `VIBE_TRUSTED_PROXIES` is unset) share that network's caps, so they can be refused together.

When a shard is filled, new clients get 429 on that route only (existing clients and other routes are unaffected) until the 60-second window passes, for as long as the attacker keeps refreshing their buckets. Mitigations outside the SDK: rate limiting at the load balancer or CDN, and a WAF for the public routes.

### Saturation alert

Every refused new key increments a process-wide counter (`vibe::runtime::saturation_refusals()`), and each saturated shard logs at most once per minute at `WARN` with target `vibe::rate_limit_saturation`:

```text
WARN vibe::rate_limit_saturation: rate-limit shard saturated; refusing new keys shard="login:ip" reason="reserve" buckets=10000 owners=290 refused=50 refused_total=50
```

`reason` is `owner_cap`, `network_cap`, `reserve` or `pinned`. Before release, add a log-based alert on that target (for example, any `reason="reserve"` or `reason="pinned"` line, or `refused` above a few dozen per minute), since those mean new clients are being turned away rather than one heavy network being capped.

## Manifest audit stamp (#251, decision 118)

`vibe manifest` adds an `audit` object to the manifest before signing it, so
the stamp is covered by the manifest signature:

| Field | Source |
| --- | --- |
| `tag` | `GITHUB_REF_NAME` when `GITHUB_REF_TYPE=tag`, else `null` |
| `commit` | `GITHUB_SHA`, else `null` |
| `run_id` | `GITHUB_RUN_ID`, else `null` |
| `compiler_hashes` | sorted, de-duplicated `compiler-hash` of every lock in the build |
| `built_at` | build time, RFC 3339 UTC |

Only these variables are read; no key or secret is ever written to the stamp.
Local builds get `null` for the CI fields. Manifests produced before this
change have no `audit` object and still verify.

## Residual risks

- `release` environment admin bypass is enabled (`can_admins_bypass: true`). An admin can deploy without Jack's environment approval. Jack accepted this risk.
- An admin can bypass the `main` ruleset (OrganizationAdmin bypass). The only human approval gate is Jack approving the `release` environment; the automatic gates remain decisions 113/114/118. The same account (`j113203`) can approve its own release. Admin bypass stays on; Jack accepted this risk.
