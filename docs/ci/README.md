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
4. **Run.** `vibe-runtime verify --artifact dist` checks an artifact without serving. `vibe-runtime --artifact manifest.json --db postgres` verifies release
   manifests in every environment using deployer-provided project identity and
   issuer pins, optional repository id, and `VIBE_RELEASE_VERSION`.
   See [release manifest pins](../runtime-config.md#release-manifest-pins-decision-182).
   Both the caller pin and the SDK reusable signer fixed in the runtime must
   match. Verification uses the Sigstore bundle and compiled-in trust root
   offline; `cosign` must be on `PATH`. Development keys are never trusted for
   release manifests. The reusable workflow that produces accepted project
   manifests lands in a follow-up (#309).

## Setup (done by a repo admin, not by the SDK)

- `release` environment: required reviewers, deployment tags `v*` only.
- Secret in the `release` environment (not repo level): `VIBE_LLM_API_KEY`.
  No signing secret exists.
- Repo or PR-scoped secret `VIBE_LLM_API_KEY_PREVIEW` (low quota, cannot reach
  release secrets).
- `staging` and `production` environments for the deploy job.
- Runtime configuration: `VIBE_RELEASE_VERSION` = deployed version (e.g.
  `1.8.0`), `VIBE_EXPECT_IDENTITY`, `VIBE_EXPECT_ISSUER`, optional
  `VIBE_EXPECT_REPOSITORY_ID`, and `cosign` installed; see
  [runtime pins](../runtime-config.md#release-manifest-pins-decision-182).
- Make `preview` a required status check on `main`.

## Local development

`vibe build` creates `.vibe/signing.key` (mode 0600) and `.vibe/signing.pub`
for development; release builds need no key. Every build runs the secret scan
and fails on findings. It also validates and lists `.vibe/slugs.json` and
`.vibe/disabled.txt` (edit these files directly; restart for disabled-endpoint
changes), and warns about destructive migrations. Build writes
locks and `.vibe/build-record.json`; builds (including `--release`) refuse
locks that differ from that record. The record is a local
tripwire, not a trust root; only the attested release artifact is deployable.

For PostgreSQL deployment, `vibe run --db postgres` and
`vibe-runtime run --db postgres` apply pending migrations before serving.
Use `VIBE_MIGRATE_DATABASE_URL` for owner credentials and `VIBE_DATABASE_URL`
for the non-owner serving connection. Once nothing is pending, subsequent
startups may omit owner credentials. See [automatic migrations](../runtime-config.md#automatic-migrations-decision-179)
for tenant selection, advisory locks and destructive-migration recovery.
