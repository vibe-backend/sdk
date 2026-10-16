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

### Tenant and platform network-range caps (#132, D130)

On top of the shard caps, every request that creates new buckets is charged to two layers of network-range slot counters. They count **new counter slots (reservations)**, not requests, the same meaning as the reserve caps above:

Before counting, IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are converted to IPv4.

- Tenant layer: `(tenant_id, IPv4 /24)` at most 10 new slots per window (`TENANT_NET_CAP`); `(tenant, IPv6 /48)` at most 10 and `(tenant, IPv6 /32)` at most 50 (`TENANT_V6_32_CAP`).
- Platform layer: IPv4 /24 and IPv6 /48 across all tenants, at most 5x the tenant cap by default (also in new counter slots). Set `VIBE_RATE_PLATFORM_FACTOR` (positive integer, default 5, at most 100) to change the multiplier.

A request must pass both layers. Each tenant has a guaranteed minimum share of the platform table and of the memory cap (see below). #73's in-shard owner and reserve caps still apply on top. A refusal is the usual 429 `RATE_LIMITED` with `Retry-After`, and the response never says which layer refused it. `Retry-After` is the time until every live counter the request touches (in both layers) has reset, so its value does not depend on the layer. Requests that only hit existing buckets are not counted.

These counters use **fixed** 60-second windows. Every other limit stays sliding-window. A client can get up to 2x a cap across a window edge (for example 10 new slots at second 59 and 10 more at second 60). This is an accepted risk.

Counter slots are bounded and partitioned, so a full partition only refuses requests charged to it:

- Tenant counters are partitioned by tenant: 4,096 slots per tenant (`slots_per_tenant`). The global tenant budget is sized for `expected_tenants x 4,096` slots (default 256 tenants, about 1M slots; set `VIBE_RATE_EXPECTED_TENANTS`, 1 to 4,096). A tenant holding fewer than 64 slots (`tenant_slot_floor`) is admitted even when the global budget is used up.
- Platform counters are partitioned by network aggregate (IPv4 /24, IPv6 /32): 4,096 slots per aggregate. Each platform counter is also charged to the tenant that created it. A tenant holding fewer than 64 platform counters (`platform_slot_floor`) is admitted even when the shared platform total is used up; only growth above that floor is refused (`table_full`). The platform total is sized as `tenant budget + expected_tenants x 64` (default 256 x 4,096 + 256 x 64 = 1,064,960 slots; `VIBE_RATE_EXPECTED_TENANTS` resizes both). The aggregate partition still applies inside the floor.

Expired counters are cleared first, oldest first (60-second fixed windows). A counter still inside its window is never evicted. If a slot table is full and nothing has expired, the request fails closed with 429 and the usual `Retry-After`. Every refusal is counted in `vibe_rate_limit_slot_refused_total{layer="tenant"|"platform", reason}` (`vibe::runtime::slot_refused_total` / `slot_refused_metrics`), where `reason` is one of:

- `network_cap`: a network is over its new-slot cap (either layer);
- `table_full`: a tenant's own 4,096-slot partition, the platform total, or the limiter memory cap is full;
- `tenant_budget_exhausted`: the global tenant budget is used up and the tenant holds 64 or more slots;
- `prefix_cap_exceeded`: a platform IPv4 /24 or IPv6 /32 partition is full.

`counter_slot_refusals()` remains as the total of every reason except `network_cap`. `layer` only appears in this internal metric, never in a response. For every reason except `network_cap` the limiter logs at most once per minute per layer and reason at `WARN` with target `vibe::rate_limit_counter_slots` (fields `layer`, `reason`, `refused`). Add alerts on that target, in particular for `tenant_budget_exhausted` and `prefix_cap_exceeded`.

**Memory cap:** `VIBE_RATE_TABLE_MEMORY_MAX_BYTES` (default 268435456 = 256 MiB, at least 1 MiB) caps the estimated memory of all limiter tables combined: rate-limit shards, both D130 tables and the daily quotas. A request that would push the estimate over the cap is refused with reason `table_full` (same 429 + `Retry-After`); quota growth past the cap is refused the same way.

Part of the cap is reserved per-tenant floor within the memory cap: `floor_reserve_bytes` = `expected_tenants x 64 x 3 KiB` (one tenant counter, one platform counter and one rate bucket per floor slot), about 48 MiB at the default 256 tenants (192 KiB per tenant), capped at half of `VIBE_RATE_TABLE_MEMORY_MAX_BYTES`. A request that keeps the tenant within both its tenant floor (64) and platform floor (64) is checked against the full cap; any other growth is checked against the cap minus the reserve. So with the defaults, above-floor growth across all tenants is limited to about 208 MiB, and a tenant under its floor is not refused because other tenants used the rest. The hard cap still holds for every request. Estimates per entry are upper bounds of the figures measured by `tests/limiter_memory.rs` (counting allocator, 8,000 entries, x86_64 Linux debug build; run with `--nocapture`):

| Entry | Measured | Estimate used |
| --- | --- | --- |
| D130 counter slot (tenant or platform) | ~481 B | 1,024 B (covers worst-case window queues) |
| Rate bucket (5 hits per window) | ~991 B | 1,024 B + 32 B per allowed hit |
| Daily quota entry | ~196 B | 256 B |

At the defaults the slot tables alone could reach about 1M tenant slots (~504 MB measured) and about 1M platform slots (~504 MB measured), so the memory cap binds first: about 262,000 D130 slots at the 1,024-byte estimate, or roughly 80,000 new buckets per window (each new bucket adds one bucket and at least two slots).

**Test-only client IP override:** `honor_test_ip` (header `x-vibe-test-ip`) only takes effect in debug builds (`cfg!(debug_assertions)`). Release builds ignore the header, and `vibe serve` / `router` refuse to start if it is set in a release build.

**Multi-instance deploys:** all limiter state, including these counters, lives in process memory, so each instance enforces its own caps. Deploys with more than one instance need shared storage (#157).

## Residual risks

- Rate-limit counter slots (D130): the above-floor part of the limiter memory cap (`VIBE_RATE_TABLE_MEMORY_MAX_BYTES` minus the floor reserve) and the above-floor part of the platform slot total are shared by every tenant and network; when they are used up, above-floor growth gets 429 (`table_full`) until counters expire, while each tenant can still open up to 64 tenant and platform slots. The floor reserve is sized for `VIBE_RATE_EXPECTED_TENANTS`; with many more active tenants than that, floor growth can also reach the hard cap. Rate buckets are reserved at their base size; buckets with high per-minute limits use more than the reserve estimate. The global tenant budget is sized for `VIBE_RATE_EXPECTED_TENANTS`; past it, only tenants holding fewer than 64 slots can open new ones (`tenant_budget_exhausted`). An IPv6 /32 with more than 4,096 live /48s per minute fails closed for its own users (`prefix_cap_exceeded`). The platform layer is cross-tenant by design, so one tenant's traffic can still cap other tenants on the same /24 or /48. Measured memory per entry is in the table above (#195).

- `release` environment admin bypass is enabled (`can_admins_bypass: true`). An admin can deploy without Jack's environment approval. Jack accepted this risk.
- An admin can bypass the `main` ruleset (OrganizationAdmin bypass). The only human approval gate is Jack approving the `release` environment; the automatic gates remain decisions 113/114/118. The same account (`j113203`) can approve its own release. Admin bypass stays on; Jack accepted this risk.
