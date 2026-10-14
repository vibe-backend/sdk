# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (#70)** — A separate ruleset dedicated to release tags: target **tags** matching `v*`, restrict creation, block updates, deletion and force-push.
   - **Current state: releases are frozen.** `bypass_actors` is empty, and GitHub's creation restriction only lets bypass actors create matching tags, so nobody (not even organization admins) can create a `v*` tag.
   - **Planned (iii), NOT in effect:** the bypass list will contain exactly one dedicated release GitHub App (no human accounts), and that App creates `v*` tags. Tags are no longer pushed by `j113203`, so Jack as a `release` environment reviewer does not collide with Prevent self-review. The final design will be set with Jack in #70 before the first release. Do not add bypass actors before then.
   - Opening tag creation to everyone with write access was rejected: all agents push as `j113203`, so it would provide no gatekeeping.
   - The existing `main` ruleset (25057946, "main protection") keeps its OrganizationAdmin bypass; that bypass does not apply to `v*` tags, which are governed only by the release ruleset (spec decision 96).
2. **Environment `release`** — Settings → Environments → `release` with required reviewers. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.
