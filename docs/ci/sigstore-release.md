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
| Source repository | `https://github.com/vibe-backend/sdk` (`VIBE_RELEASE_REPO`) |
| Signer workflow | `vibe-backend/sdk/.github/workflows/vibe-release.yml` (`VIBE_RELEASE_WORKFLOW`) at the same ref |
| Ref | `refs/tags/v*` (non-empty after `v`) |
| Runner | GitHub-hosted |
| Subject | SHA-256 of the exact `manifest.json` bytes |
| Rekor | an entry with an inclusion proof or promise, integrated while the certificate was valid, recording this envelope |

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
- **`vibe serve` / `vibe migrate`** (`src/attest.rs`), outside development:
  the bundle must exist and pass every row of the table above. The DSSE
  signature is verified with the certificate's key, the subject digest with
  the manifest bytes, and the Rekor entry body must record this envelope's
  payload hash and signature.

### Deferred in the runtime

The runtime does **not** yet verify the Fulcio certificate chain to the
Sigstore root or the Rekor checkpoint / signed entry timestamp signatures.
Both need the Sigstore trust root, which is distributed through TUF and
rotates; fetching it at server start adds a network dependency and a new
failure mode, and pinning a copy in the binary needs a rotation process.
`sigstore-rs` can do this, but its bundle verification API is still marked
unstable and pulls a large dependency tree. Until then the deploy job is the
cryptographic gate and the runtime check is defence in depth (it stops a
manifest whose bundle names the wrong repo, workflow, ref or log entry, or
does not match the file). Follow-up: adopt `sigstore-rs` with a pinned
trusted root plus a rotation runbook.

## `VIBE_RELEASE_SIGNING_KEY`

Release builds (`vibe build`, `vibe manifest --release`) take no key and
**refuse to run** if `VIBE_RELEASE_SIGNING_KEY` is set. The secret itself is
not removed by this change; Jack decides whether to delete it. Dev builds
(`vibe build --dev`) keep using `.vibe/signing.key`.

## Setup for Jack

Nothing new in a cloud account: no keys, no KMS, no service accounts. Keep or
check:

1. The `release` environment with Jack as required reviewer, deployments
   limited to `v*` tags.
2. The `v*` tag ruleset (no update, deletion or non-fast-forward).
3. Copy `docs/ci/vibe-release.yml` to `.github/workflows/vibe-release.yml`
   (the path the identity check expects), or set `VIBE_RELEASE_WORKFLOW` in
   the runtime and the workflow `env` to the path you use.
4. Ship `manifest.json.sigstore.json` next to `manifest.json` (or set
   `VIBE_MANIFEST_BUNDLE`).
