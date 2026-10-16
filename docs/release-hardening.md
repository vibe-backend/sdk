# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (#70, live: ruleset 25067953 "release tags v*")** — Target **tags** matching `refs/tags/v*`, enforcement active, rules: `update`, `deletion`, `non_fast_forward` (block moving, deleting and force-pushing tags). There is **no creation rule**: anyone with write access can create a `v*` tag (the product manager, on Jack's behalf, chose option (i)). `bypass_actors` is empty, so not even organization admins can move or delete a published tag.
   - Gating today: creating a tag does not publish anything; the publishing job runs in the `release` environment, whose only reviewer is `j113203` with `prevent_self_review: false`.
   - **Residual risk (accepted by Jack):** all agents push as `j113203`, so any `j113203` agent can create a `v*` tag **and self-approve the release**.
   - `vibe build --release-approval` (spec §2C.3a) is designed but **not implemented yet**; it provides no protection today.
   - Option (i) is a **temporary** scheme. Target (iii), not in effect: a dedicated release GitHub App is the sole `v*` tag creator, and `prevent_self_review` is turned on.
   - The existing `main` ruleset (25057946, "main protection") keeps its OrganizationAdmin bypass; it does not apply to `v*` tags (spec decision 96).
2. **Environment `release`** — Settings → Environments → `release`. Live: reviewer `j113203`, `prevent_self_review: false` (all agents push as `j113203`), deployments limited to the `v*` tag pattern. Administrator bypass stays **enabled** (`can_admins_bypass: true`): Jack decided to keep it and accepts the risk that an admin can skip the environment approval. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.

## Residual risks

- `release` environment admin bypass is enabled (`can_admins_bypass: true`). An admin can deploy without Jack's environment approval. Jack accepted this risk.
