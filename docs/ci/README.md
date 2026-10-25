# Release builds and deploy gate (spec v1.8)

Locks are build output. They are not committed (`*.lock.md` is in
`.gitignore`) and are never a deploy input.

## Flow

1. **Review the source PR.** People review the markdown sources and
   frontmatter. PR CI (`preview` job) runs `vibe build --check`, which
   compiles in memory and runs the deterministic policy, then writes the
   result to the job summary. It is informational and never deployed. It uses
   `VIBE_LLM_API_KEY_PREVIEW`.
2. **Build once through the reusable workflow.** Copy [the caller example](vibe-release.yml)
   to your project's `.github/workflows/release.yml`. A `v*` tag push calls
   `vibe-backend/sdk/.github/workflows/vibe-project-release.yml@v1.0.0-rc.8`.
   The SDK reference must be a release **tag**,
   never a branch or SHA; Dependabot can bump it. The reusable workflow is
   available from `v1.0.0-rc.5`, the first SDK release tag that contains it;
   select that release or later, with the SDK tag
   version equal to the deployed runtime version. The workflow checks that
   release tag against the reusable workflow SHA. An annotated tag matches
   when the SHA is the tag object or the commit it points at; a lightweight
   tag must match the commit. The certificate build signer digest is a
   different value: the tag ref's object SHA (the annotated tag object, or
   the commit for a lightweight tag), compiled into the runtime as
   `VIBE_SDK_COMMIT`. It is not the peeled commit. `github.sha` on the SDK's
   own tag push is that peeled commit and is used only to check that the
   commit is on `main`. It then installs the SDK and runtime through the
   unchanged checksum and SDK attestation checks.
   Its `build-and-sign` job uses the protected caller environment, validates
   the caller tag and explicit LLM model/key, builds a release manifest,
   signs with keyless OIDC/Fulcio/Rekor, self-verifies with caller pins, attests
   and uploads exactly the manifest and bundle. Signing failures fail the job
   without fallback. The key is passed through a 0600 temporary file and deleted.
   Signing disables cosign's TUF signing config to require a Rekor v1 entry with inclusion proof and integrated time for offline runtime verification; self-verification fails closed if this ever diverges.
   The caller job must always grant `contents: write`, `id-token: write`, and
   `attestations: write`, even if publishing is disabled: GitHub validates called
   workflow job permissions at parse time, including the skipped `publish` job's
   `contents: write`. Top-level permissions are empty.
   Internally only `build-and-sign` has OIDC/attestation write permission and
   `contents: read`. The separate `publish` job has only `contents: write`:
   it checks the digest and exact two-file artifact, then creates a release or
   adds assets without overwriting. Prereleases use `--prerelease --latest=false`;
   stable tags use `--latest`.
3. **Promote only.** Use the outputs `manifest-sha256`, `artifact-name`,
   `expect-identity`, `expect-issuer`, and `repository-id`. Download the manifest
   and bundle, check the digest, verify the attestation against the **project**
   repository and tag/commit, then deploy those same files without rebuilding:

   ```sh
   gh attestation verify manifest.json --repo <owner>/<repo> \
     --signer-workflow vibe-backend/sdk/.github/workflows/vibe-project-release.yml \
     --source-ref refs/tags/v<version> --source-digest <commit> --deny-self-hosted-runners
   ```

   SDK release assets still use the SDK's `release.yml` signer identity.
4. **Run.** `vibe-runtime verify --artifact dist` checks an artifact without serving. `vibe-runtime --artifact manifest.json --db postgres` verifies release
   manifests in every environment using deployer-provided project identity and
   issuer pins, optional repository id, and `VIBE_RELEASE_VERSION`.
   See [release manifest pins](../runtime-config.md#release-manifest-pins-decision-182).
   Both the caller pin and the SDK reusable signer fixed in the runtime must
   match. Verification uses the Sigstore bundle and compiled-in trust root
   offline; `cosign` must be on `PATH`. Development keys are never trusted for
   release manifests. Official SDK release binaries embed `VIBE_SDK_COMMIT`;
   local builds without it refuse all release manifests.

## Setup (done by a repo admin, not by the SDK)

- `release` environment: required reviewers, deployment tags `v*` only.
- Secret in the `release` environment (not repo level): `VIBE_LLM_API_KEY`.
  Pass it explicitly as `secrets: llm-api-key: ${{ secrets.VIBE_LLM_API_KEY }}`;
  do not use `secrets: inherit`. `on.workflow_call` cannot target an environment:
  `build-and-sign` declares `environment: ${{ inputs.environment }}`, so
  environment secrets of that name in the **caller repository** resolve there.
  No signing secret exists.
- Repo or PR-scoped secret `VIBE_LLM_API_KEY_PREVIEW` (low quota, cannot reach
  release secrets).
- `staging` and `production` environments for the deploy job.
- Runtime configuration: `VIBE_RELEASE_VERSION` = deployed version (e.g.
  `1.8.0`), `VIBE_EXPECT_IDENTITY`, `VIBE_EXPECT_ISSUER`, optional
  `VIBE_EXPECT_REPOSITORY_ID`, and `cosign` installed; see
  [runtime pins](../runtime-config.md#release-manifest-pins-decision-182).
- Make `preview` a required status check on `main`.

## Dry-run CI

Call the reusable workflow with `dry-run: true`, `llm-provider: fake`,
`llm-model: fake`, a fixture `project-path`, and `publish-release: false`,
without passing secrets. The only running job has `contents: read`, checks out
and builds the SDK at the called commit (no release tag required), exercises
rejection of non-tag events, runs check/dev builds and verification refusal
checks. It never signs, publishes, or receives the LLM key, even if supplied.
The caller still needs the full permissions ceiling above because GitHub
validates skipped called jobs at parse time; the dry-run job narrows it.
The SDK's PR CI calls the local workflow this way.

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

## Replay previews without a key

Review and commit recorded responses under `fixtures/llm/` as described in
[replay and recording](../README-details.md#llm-replay-and-recording-decision-189).
PR CI can run `LLM_PROVIDER=replay vibe build --check` without any key, URL or
model and without LLM network access. Replay is for test/preview builds;
release builds require a real provider. CI cannot record new responses.

`vibe build --release` sends the key and prompts only to the hard-coded hosts
`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`,
and `ai-proxy-sub2api.clinicone.me`. The ClinicOne proxy is an exact hostname
on the release supply-chain trust boundary: a compromise of that host can
affect release compilation, and the API key and prompts go through it. Other
`clinicone.me` names, `http`, userinfo, a non-standard port, an IP literal,
and redirects off the allowlist are refused. The list cannot be extended
through the environment. See
[Release host allowlist](../best-practices/llm-provider-setup.md#release-host-allowlist).

To include rendered locks in the GitHub step summary, build first (check only
compiles in memory), then append the generated, untracked locks:

```sh
LLM_PROVIDER=replay vibe build
while IFS= read -r -d '' f; do
  printf '\n### %s\n\n' "$f"
  cat "$f"
done < <(git ls-files -z -o --exclude-standard --ignored '*.lock.md') >> "$GITHUB_STEP_SUMMARY"
```

This Bash snippet includes ignored lock output; locks remain uncommitted.
