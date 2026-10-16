# Release builds and deploy gate (spec v1.8)

Locks are build output. They are not committed (`*.lock.md` is in
`.gitignore`) and are never a deploy input.

## Flow

1. **Review the source PR.** People review the markdown sources and
   frontmatter. PR CI (`preview` job) runs `vibe compile --check`, which
   compiles in memory and runs the deterministic policy, then writes the
   result to the job summary. It is informational and never deployed. It uses
   `VIBE_LLM_API_KEY_PREVIEW`.
2. **Build once.** After the merge to protected `main`, an `install` job with
   no secrets builds vibe from a pinned SDK commit (`VIBE_SDK_REV`) and later
   jobs check its digest. `release-policy` runs the LLM compile and deploy
   policy with the LLM key only. `release-build` then runs `vibe build` in the
   `release` environment: it compiles every source in memory, validates the
   in-memory locks (policy, dependency hashes, SQL allowlist, inherited auth),
   records per-lock hashes and the commit in the signed manifest, and writes
   the compiled locks to a local build directory (`--locks-dir`, default
   `.vibe/build/locks`; never the repo). Lock files on disk are not read.
   High-risk items fail closed until release-environment approval
   verification (#80) is implemented; `--release-approval` is not approval.
   The manifest is attested with `actions/attest-build-provenance`. Release
   builds never read `.env` and send the LLM key only to an allowlisted host
   (`api.openai.com`, the default when `LLM_BASE_URL` is unset;
   `api.anthropic.com`; `generativelanguage.googleapis.com`).
3. **Promote only.** `deploy` checks the digest, runs
   `gh attestation verify --repo --signer-workflow --source-ref refs/heads/main`
   (fail closed), and deploys the same file to staging and then production.
4. **Run.** Outside development, `vibe serve` / `vibe migrate` verify the
   manifest with the pinned release public key only: a key compiled into the
   binary with `VIBE_RELEASE_PUBKEY` (the runtime `VIBE_MANIFEST_PUBKEY`
   cannot override it; a differing value is refused), otherwise
   `VIBE_MANIFEST_PUBKEY`. They run only
   `build_mode: release` manifests. `.vibe/signing.pub` is ignored there.

## Setup (done by a repo admin, not by the SDK)

- `release` environment: required reviewers, deployment branch `main` only.
- Secrets in the `release` environment (not repo level):
  `VIBE_LLM_API_KEY`, `VIBE_RELEASE_SIGNING_KEY` (hex ed25519 seed).
- Repo or PR-scoped secret `VIBE_LLM_API_KEY_PREVIEW` (low quota, cannot reach
  release secrets).
- `staging` and `production` environments for the deploy job.
- Runtime configuration: `VIBE_MANIFEST_PUBKEY` = hex public key matching
  `VIBE_RELEASE_SIGNING_KEY`.
- Make `preview` a required status check on `main`.

## Local development

Add `.vibe/` to your project's `.gitignore`: it holds the dev signing key,
the build record, the LLM output cache, and the build output
(`.vibe/build/locks`).

`vibe keygen` creates `.vibe/signing.key` (mode 0600). `vibe compile` writes
locks and `.vibe/build-record.json`; `vibe validate --deploy` and `vibe
manifest` refuse locks that differ from that record. The record is a local
tripwire, not a trust root; only the attested release artifact is deployable.
