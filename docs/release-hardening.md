# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Tag ruleset** — In the GitHub repo (or org) rulesets, protect tags matching `v*`: block force-push and deletion; limit who can create tags (admins / release bot only).
2. **Environment `release`** — Settings → Environments → `release` with required reviewers. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
