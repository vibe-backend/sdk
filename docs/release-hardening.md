# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (#70, live: ruleset 25067953 "release tags v*")** — Target **tags** matching `refs/tags/v*`, enforcement active, rules: `update`, `deletion`, `non_fast_forward` (block moving, deleting and force-pushing tags). There is **no creation rule**: anyone with write access can create a `v*` tag (the product manager, on Jack's behalf, chose option (i)). `bypass_actors` is empty, so not even organization admins can move or delete a published tag.
   - Gating: creating a tag does not publish anything. The publishing job runs in the `release` environment and needs Jack's approval; high-risk releases also need `vibe build --release-approval` (spec §2C.3a).
   - Residual risk: all agents push as `j113203`, so any write-capable agent can create a `v*` tag. Protection is the environment approval plus the release-approval check.
   - The existing `main` ruleset (25057946, "main protection") keeps its OrganizationAdmin bypass; it does not apply to `v*` tags (spec decision 96).
2. **Environment `release`** — Settings → Environments → `release` with Jack as required reviewer, Prevent self-review **disabled** (all agents push as `j113203`), deployments limited to `v*` tags, and administrator bypass disabled. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.
