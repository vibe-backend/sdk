# Keyless release signing with Sigstore

Release manifests are signed **keyless**: there is no long-lived signing key.
The release job proves who built the manifest with its GitHub Actions OIDC
token; Sigstore turns that into a short-lived Fulcio certificate and logs
the signature in the public Rekor transparency log (the repo is public, so
`actions/attest-build-provenance` uses public-good Sigstore). This matches
the decision 113 / 118 attestation: one artifact, built once on a `v*` tag
in the `release` environment, verified with `gh attestation verify`.

## Who may sign

| Field | Required value |
| --- | --- |
| OIDC issuer | `https://token.actions.githubusercontent.com` |
| Source repository | `https://github.com/vibe-backend/sdk` (compiled in; override only in relaxed mode) |
| Signer workflow | `vibe-backend/sdk/.github/workflows/release.yml` (compiled in; override only in relaxed mode) at `refs/tags/v<version>` |
| Ref | `refs/tags/v<version>` exactly (deployer `--expect-version` / `VIBE_EXPECT_VERSION`, matching `provenance.version`) |
| Commit | certificate source commit == deployer `--expect-commit` / `VIBE_EXPECT_COMMIT` == `provenance.commit` |
| Bundle media type | v0.2 or v0.3 (v0.1 is refused) |
| Rekor | inclusion proof **and** a SET or checkpoint for that entry |
| Runner | GitHub-hosted |
| Subject | SHA-256 of the exact `manifest.json` bytes |

Anything else fails closed: a missing bundle, a missing or wrong field, no
Rekor entry, a bad DSSE signature, a different manifest, a key-based
(non-keyless) bundle, or a manifest commit that is not the certificate's
commit.

## Where it is verified

- **Deploy job** (`docs/ci/vibe-release.yml`): `gh attestation verify` with
  `--bundle`, `--repo`, `--signer-workflow`, `--source-ref "$GITHUB_REF"`,
  `--source-digest`, `--cert-oidc-issuer`, `--predicate-type` and
  `--deny-self-hosted-runners`. This is the full cryptographic check: Fulcio
  chain to the Sigstore trust root, Rekor inclusion and SET, identity.
- **`vibe serve` / `vibe migrate`** (`src/attest.rs`), outside relaxed mode:
  the bundle must exist and pass every row of the table above. Verification
  uses the `sigstore-verify` verifier with the embedded Sigstore public-good
  trust root (`trusted_root.json`): Fulcio certificate chain, SCT, Rekor
  inclusion proof and SET (or checkpoint), certificate validity at the
  integrated time, DSSE signature, and subject digest. Identity, issuer,
  runner, version/ref and commit checks then follow decision 119: the
  deployer supplies the expected version and commit (`--expect-version` /
  `--expect-commit` or `VIBE_EXPECT_*`); they must equal `provenance.version`
  / `provenance.commit` and the certificate.

## `VIBE_RELEASE_SIGNING_KEY`

Release builds (`vibe build`, `vibe manifest --release`) ignore
`VIBE_RELEASE_SIGNING_KEY`, even when set; the release job signs keyless. The secret itself is
not removed by this change; Jack decides whether to delete it. Dev builds
(`vibe build --dev`) keep using `.vibe/signing.key`.

## Setup for Jack

Nothing new in a cloud account: no keys, no KMS, no service accounts. Keep or
check:

1. The `release` environment with Jack as required reviewer, deployments
   limited to `v*` tags.
2. The `v*` tag ruleset (no update, deletion or non-fast-forward).
3. Copy `docs/ci/vibe-release.yml` to `.github/workflows/release.yml`
   (the path the identity check expects). Repo/workflow overrides and other
   relaxed config exist only in debug builds and need `VIBE_ENV=development`
   (or the debug-only `--insecure-dev` / `VIBE_INSECURE_DEV` when `VIBE_ENV`
   is unset). Release binaries have no `--insecure-dev` flag and always
   require a verified Sigstore bundle, whatever `VIBE_ENV` or `--memory` say.
4. Ship `manifest.json.sigstore.json` next to `manifest.json` (or set
   `VIBE_MANIFEST_BUNDLE`).
