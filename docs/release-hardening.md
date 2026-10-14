# Release hardening (#28)

Before the first `v*` tag. Items 1–2 are repository settings decided by the product manager; this PR does not change them:

1. **Release tag ruleset (decision in #70)** — A separate ruleset dedicated to release tags: target **tags** matching `v*`, `bypass_actors: []` (empty, nobody can bypass, not even organization admins), block force-push and deletion, and restrict tag creation to the release maintainers. The existing `main` ruleset (25057946, "main protection") is left unchanged, including its current `bypass_actors`; that bypass does not apply to `v*` tags because tags are governed only by the release ruleset. Create this ruleset before the first `v*` tag.
2. **Environment `release`** — Settings → Environments → `release` with required reviewers. The release job in `.github/workflows/release.yml` uses this environment.
3. **Pinned actions and runners** — `release.yml` pins `actions/checkout`, `upload-artifact`, `download-artifact`, `attest-build-provenance`, and `dtolnay/rust-toolchain` to full commit SHAs; runners are pinned images (no `*-latest`); checkout uses `persist-credentials: false`.
4. **No clobber** — Existing GitHub Releases are never overwritten (`gh release upload --clobber` removed).
5. **Provenance** — Each matrix asset and `SHA256SUMS` are attested. Only the build/checksum jobs get `id-token`/`attestations`; the publishing job has `contents: write` only.
6. **No unused container build** — The unused root `Dockerfile` / `.dockerignore` were removed; ship the release binaries from this workflow only.
7. **Tag on main** — The workflow fails unless the tagged commit is reachable from `main`.
