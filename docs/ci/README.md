# Release builds and deploy gate (spec v1.8)

Locks are build output. They are not committed (`*.lock.md` is in
`.gitignore`) and are never a deploy input.

## Flow

1. **Review the source PR.** People review the markdown sources and
   frontmatter. PR CI (`preview` job) runs `vibe compile --check`, which
   compiles in memory and runs the deterministic policy, then writes the
   result to the job summary. It is informational and never deployed. It uses
   `VIBE_LLM_API_KEY_PREVIEW`.
2. **Build once, on a `v*` tag.** An `install` job with no secrets builds
   vibe from a pinned SDK commit (`VIBE_SDK_REV`) and later jobs check its
   digest. `release-policy` runs the LLM compile and deploy policy with the
   LLM key only. `release-build` then runs in the `release` environment
   (Jack approves the run): `vibe build` compiles every source in memory,
   validates the in-memory locks (policy, dependency hashes, SQL allowlist,
   inherited auth), records per-lock hashes and the commit, and writes the
   compiled locks to a local build directory (`--locks-dir`, default
   `.vibe/build/locks`; never the repo). Lock files on disk are not read.
   High-risk items fail closed until release-environment approval
   verification (#80) is implemented; `--release-approval` is not approval.
   Release builds never read `.env` and send the LLM key only to an
   allowlisted host (`api.openai.com`, the default when `LLM_BASE_URL` is
   unset; `api.anthropic.com`; `generativelanguage.googleapis.com`). The
   build takes **no signing key** and refuses `VIBE_RELEASE_SIGNING_KEY`.
3. **Sign keyless.** The same job attests `manifest.json` with
   `actions/attest-build-provenance`: Sigstore keyless signing with the
   job's GitHub OIDC token, a short-lived Fulcio certificate and a Rekor
   transparency log entry. The bundle is shipped as
   `manifest.json.sigstore.json`. This is the only job with `id-token: write`.
4. **Promote only.** `deploy` checks the digest and runs
   `gh attestation verify --bundle --repo --signer-workflow --source-ref
   --source-digest --deny-self-hosted-runners` (fail closed), then deploys the
   same two files to staging and then production.
5. **Run.** Outside development, `vibe serve` / `vibe migrate` accept only a
   keyless release manifest whose bundle passes the identity check (see
   [sigstore-release.md](sigstore-release.md)). No key is trusted there;
   `VIBE_MANIFEST_PUBKEY` and `.vibe/signing.pub` are development only.

Details, what is verified where, and what is deferred:
[sigstore-release.md](sigstore-release.md).

## Setup (done by a repo admin, not by the SDK)

- `release` environment: required reviewers, deployments limited to `v*`
  tags.
- Tag ruleset on `refs/tags/v*` (no update, deletion or force push).
- Secret in the `release` environment (not repo level): `VIBE_LLM_API_KEY`.
  No signing secret is needed. An existing `VIBE_RELEASE_SIGNING_KEY` secret
  is no longer read by release builds (and makes them fail if passed in);
  whether to delete it is Jack's call.
- Repo or PR-scoped secret `VIBE_LLM_API_KEY_PREVIEW` (low quota, cannot reach
  release secrets).
- `staging` and `production` environments for the deploy job.
- Runtime configuration: ship `manifest.json.sigstore.json` next to the
  manifest (or set `VIBE_MANIFEST_BUNDLE`). Repo and workflow identity use the
  compiled-in defaults (`vibe-backend/sdk`, `.github/workflows/release.yml`);
  overrides need relaxed mode (`VIBE_ENV=development` and a debug build or
  `--insecure-dev`). Deploy with `--expect-version` / `--expect-commit` (or
  `VIBE_EXPECT_VERSION` / `VIBE_EXPECT_COMMIT`) matching the release tag and
  commit recorded in the manifest.
- Make `preview` a required status check on `main`.

## Local development

Add `.vibe/` to your project's `.gitignore`: it holds the dev signing key,
the build record, the LLM output cache, and the build output
(`.vibe/build/locks`).

`vibe keygen` creates `.vibe/signing.key` (mode 0600). `vibe build --dev`
signs with it and marks the manifest `build-mode: dev`; production refuses
dev manifests. `vibe compile` writes locks and `.vibe/build-record.json`;
`vibe validate --deploy` and `vibe manifest` refuse locks that differ from
that record. The record is a local tripwire, not a trust root; only the
attested release artifact is deployable.
