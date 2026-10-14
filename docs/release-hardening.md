# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (#70, live: ruleset 25067953 "release tags v*")** — Target **tags** matching `refs/tags/v*`, enforcement active, rules: `update`, `deletion`, `non_fast_forward` (block moving, deleting and force-pushing tags). There is **no creation rule**: anyone with write access can create a `v*` tag (the product manager, on Jack's behalf, chose option (i)). `bypass_actors` is empty, so not even organization admins can move or delete a published tag.
   - Gating today: creating a tag does not publish anything; the publishing job runs in the `release` environment, whose only reviewer is `j113203` with `prevent_self_review: false`.
   - **Residual risk (accepted by Jack):** all agents push as `j113203`, so any `j113203` agent can create a `v*` tag **and self-approve the release**.
   - `vibe build --release-approval` (spec §2C.3a) is designed but **not implemented yet**; it provides no protection today.
   - Option (i) is a **temporary** scheme. Target (iii), not in effect: a dedicated release GitHub App is the sole `v*` tag creator, and `prevent_self_review` is turned on.
   - The existing `main` ruleset (25057946, "main protection") keeps its OrganizationAdmin bypass; it does not apply to `v*` tags (spec decision 96).
2. **Environment `release`** — Settings → Environments → `release`. Live: reviewer `j113203`, `prevent_self_review: false` (all agents push as `j113203`), deployments limited to the `v*` tag pattern. Required but **not yet set live**: disable administrator bypass (`can_admins_bypass` is currently `true`). The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.

## Rate-limit capacity limits (#71, #73)

The in-memory limiter keeps at most 10,000 active buckets per `(route, key type)` shard (`SHARD_CAPACITY`). A single client network may own at most 100 of them (`OWNER_CAP`: one IPv4 address or one IPv6 /48), a wider network at most 1,000 (`NETWORK_CAP`: one IPv4 /24 or IPv6 /32), and the last 1,000 slots (`RESERVE`) only go to clients that hold fewer than 5 buckets (and whose wider network holds fewer than 10, `RESERVE_NETWORK_CAP`), so 29 /24s with 10 busy addresses each cannot fill a shard; it takes over 100 wider networks.

These caps bound one network, not a distributed client. In review, 290 IPv4 addresses spread over 29 /24 networks filled a 10,000-slot shard with at-limit buckets: 50 of 50 new users then got 429 on that route, and the route recovered once the 60-second window passed. An attacker with a few hundred addresses across a few dozen /24s can therefore block new clients on one route (existing clients and other routes are unaffected) for as long as they keep refreshing their buckets. Mitigations outside the SDK: rate limiting at the load balancer or CDN, and a WAF for the public routes.

### Saturation alert

Every refused new key increments a process-wide counter (`vibe::runtime::saturation_refusals()`), and each saturated shard logs at most once per minute at `WARN` with target `vibe::rate_limit_saturation`:

```text
WARN vibe::rate_limit_saturation: rate-limit shard saturated; refusing new keys shard="login:ip" reason="reserve" buckets=10000 owners=290 refused=50 refused_total=50
```

`reason` is `owner_cap`, `network_cap`, `reserve` or `pinned`. Before release, add a log-based alert on that target (for example, any `reason="reserve"` or `reason="pinned"` line, or `refused` above a few dozen per minute), since those mean new clients are being turned away rather than one heavy network being capped.
