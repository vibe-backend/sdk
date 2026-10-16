# Sandbox tag-path rehearsal (#252)

Before the first real `v*` release of `vibe-backend/sdk`, the tag-only release
path (`verify-tag`, `attest-build`, `attest-checksums`, `release`) is run once
in a separate sandbox repository with a test tag. No test tag is ever pushed to
`vibe-backend/sdk`.

## Setup (sandbox repository)

1. Create a private sandbox repository (not `vibe-backend/sdk`) and push the
   current `main` of `vibe-backend/sdk` to the sandbox `main`, so `verify-tag`
   sees the tagged commit on `main`.
2. Create a `release` environment with required reviewer `j113203`, limited to
   `v*` tags, matching decision 89/96 settings.
3. Add the tag ruleset from decision 96 (`update`, `deletion`,
   `non_fast_forward`; empty `bypass_actors`).

## Run

1. Push a test tag such as `v0.0.0-rehearsal.1` on the sandbox `main` head.
2. Jack approves the `release` environment deployment.
3. Record the run id.

## Checks

- `verify-tag` passes for the tag on `main`.
- `attest-build` / `attest-checksums` obtain an OIDC token and create
  attestations; `gh attestation verify <archive> --repo <sandbox>
  --source-ref refs/tags/v0.0.0-rehearsal.1` succeeds for each archive.
- `release` runs only after approval, uses the environment-scoped write token,
  and uploads exactly `SHA256SUMS` plus the listed archives.
- A second run for the same tag refuses to overwrite the release.

## Result

Record the sandbox repository, tag, run id and outcome of each check here (or
in #252), then delete the sandbox release and tag.
