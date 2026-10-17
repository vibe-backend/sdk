# Release builds and deploy gate (spec v1.8)

Locks are build output. They are not committed (`*.lock.md` is in
`.gitignore`) and are never a deploy input.

## Flow

1. **Review the source PR.** People review the markdown sources and
   frontmatter. PR CI (`preview` job) runs `vibe build --check`, which
   compiles in memory and runs the deterministic policy, then writes the
   result to the job summary. It is informational and never deployed. It uses
   `VIBE_LLM_API_KEY_PREVIEW`.
2. **Build once.** A `v*` tag runs the `release` job in the `release`
   environment (Jack approves it): `vibe build --release` compiles every
   source in memory, validates the in-memory locks (policy, dependency hashes,
   SQL allowlist, inherited auth) and writes an unsigned manifest recording
   per-lock hashes, the version and the commit. High-risk security items are
   listed in the deploy report; they do not block (decision 90 is withdrawn).
   Lock files on disk are not read. The job then signs the manifest with
   Sigstore keyless (`cosign sign-blob`, GitHub OIDC identity, Fulcio
   certificate, Rekor entry) into `manifest.json.sigstore.json`, and attests
   it with `actions/attest-build-provenance`. There is no signing key and no
   fallback: if OIDC, Fulcio or Rekor is unavailable the job fails. Only this
   job has `id-token: write`.
3. **Promote only.** `deploy` checks the digest, runs
   `gh attestation verify --signer-workflow vibe-backend/sdk/.github/workflows/release.yml
   --source-ref refs/tags/v<version> --source-digest <commit> --deny-self-hosted-runners`
   and `cosign verify-blob` with the exact identity
   `https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<version>`,
   issuer `https://token.actions.githubusercontent.com` and the commit
   (fail closed), then deploys the same files to staging and then production.
4. **Run.** `vibe serve` is a temporary hidden run step; the next release adds
   `vibe run`. Outside development, `vibe serve` / `vibe migrate` accept only
   `build_mode: release` manifests with a Sigstore bundle
   (`<manifest>.sigstore.json`, or `VIBE_MANIFEST_BUNDLE`) for the deployed
   version `VIBE_RELEASE_VERSION`. They check that the certificate identity
   equals exactly the identity above, the issuer, the manifest's commit, this
   repository and ref, a GitHub-hosted runner (Fulcio runner-environment
   extension) and a Rekor inclusion proof, then run `cosign verify-blob
   --offline` against the Sigstore `trusted_root.json` compiled into the
   binary (`assets/sigstore/trusted_root.json`; there is no runtime override).
   `cosign` must be on `PATH`. `.vibe/signing.pub` and dev manifests are
   never trusted there.

## Setup (done by a repo admin, not by the SDK)

- `release` environment: required reviewers, deployment tags `v*` only.
- Secret in the `release` environment (not repo level): `VIBE_LLM_API_KEY`.
  No signing secret exists.
- Repo or PR-scoped secret `VIBE_LLM_API_KEY_PREVIEW` (low quota, cannot reach
  release secrets).
- `staging` and `production` environments for the deploy job.
- Runtime configuration: `VIBE_RELEASE_VERSION` = deployed version (e.g.
  `1.8.0`), and `cosign` installed.
- Make `preview` a required status check on `main`.

## Local development

`vibe keygen` creates `.vibe/signing.key` (mode 0600). `vibe build` writes
locks and `.vibe/build-record.json`; builds (including `--release`) refuse
locks that differ from that record. The record is a local
tripwire, not a trust root; only the attested release artifact is deployable.
