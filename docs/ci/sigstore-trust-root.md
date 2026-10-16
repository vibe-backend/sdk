# Sigstore trust root: alerts and update runbook

Spec v1.8 decision 119, section 2C.3b. `vibe serve` / `vibe migrate` verify
release manifests offline against the Sigstore public-good trust root
(`trusted_root.json`) compiled into the binary (see
[sigstore-release.md](sigstore-release.md)). When Sigstore rotates the Fulcio
CA or a Rekor / CT log key, an old binary keeps trusting only the old root.
After the overlap period it refuses manifests signed under the new keys. That
refusal is correct (fail closed); the fix is a new release, never a weaker
check.

## Signals

| Name | Level | When | Where |
| --- | --- | --- | --- |
| `manifest_trust_root_rejected` | `ERROR` log (with bounded `kind`) | A release manifest is refused because the trust root cannot vouch for its bundle. `kind` is one of: `sct` (signed certificate timestamp from a CT log the root does not know, e.g. CT log rotation), `ca_chain` (Fulcio chain does not lead to a trusted CA), `no_valid_authority` (no CA valid at signing time), `unknown_log` (Rekor log unknown), `timestamp_authority` (timestamp outside the authority's validity), `not_verified` (certificate chain or Rekor log not verified), `trust_root` (embedded root unusable). | `src/attest.rs` (`record_trust_root_rejection`) |
| `manifest_trust_root_rotation_pending` | `WARN` log | At manifest verification, the embedded root lists a CA or log key whose validity ends in the future (being retired) or starts in the future (announced). | `src/attest.rs` (`warn_on_pending_rotation`) |

Both log lines carry an `alert` field with the name above. The alert is
log-based: the SDK ships no metrics exporter and no alert rule file. The
in-process counters (`vibe::attest::trust_root_rejected_total()`,
`trust_root_rejected_by_kind(kind)`) are not exported; they exist for tests
and for an embedding service that exports them itself.

**Where the alert fires:** the deployment's log pipeline must have these
rules (they are not in this repository):

- page: any line with `alert = "manifest_trust_root_rejected"` (any `kind`);
- ticket: any line with `alert = "manifest_trust_root_rotation_pending"`.

Group or label alerts by `kind` only. `reason` is the full verifier error
(unbounded); keep it in the log and never use it as a metric label. Other refusals
(bad signature, digest mismatch, wrong identity, version or commit) are not
counted as trust-root rejections.

The embedded root only knows about rotations announced before the release
was built. Also watch the Sigstore announcements (the
[sigstore/root-signing](https://github.com/sigstore/root-signing) repository
and the Sigstore blog) and treat an announced Fulcio CA or Rekor key change
as the same early warning.

## Runbook: `manifest_trust_root_rejected` fired

Never turn verification off, never fall back to the development key, and
never deploy the manifest by another path to get past this alert.

1. **Collect the refusal.** The log line has a `kind` (bounded, see
   Signals) and a `reason` (the full verifier error). Note the release version and commit being deployed, and the
   running binary version (`vibe --version`).
2. **Tell rotation apart from a bad signature.**
   - Likely an expired or rotated trust root when: the reason is a
     certificate chain error, "no trusted certificate authority is valid",
     "unknown transparency log", or `kind = "sct"` (CT log rotation); the same bundle verifies with a current
     tool (`gh attestation verify <manifest> --bundle <bundle> --repo
     vibe-backend/sdk --signer-workflow
     vibe-backend/sdk/.github/workflows/release.yml --source-ref
     refs/tags/v<version> --source-digest <commit> --cert-oidc-issuer
     https://token.actions.githubusercontent.com
     --deny-self-hosted-runners`); and Sigstore has announced a rotation
     after the running binary was built.
   - Treat it as a genuinely bad signature when: the current tool also
     refuses the bundle, the identity / ref / commit do not match the
     release, or the release workflow run that should have produced the
     bundle does not exist. Stop the deploy and handle it as a security
     incident; do not continue with this runbook.
3. **Rotated root: ship a release with a refreshed trust root** (next
   section), approved through the `release` environment like any release.
4. **Redeploy** the new runtime, then the manifest. Confirm the
   `release manifest attestation verified` log line, and that the log query
   `alert = "manifest_trust_root_rejected"` returns no new lines for the new
   deployment.

## Runbook: `manifest_trust_root_rotation_pending` fired

The running binary already knows about a pending rotation. Plan a release
with a refreshed trust root (next section) before the old key's end date, so
no deployment hits `manifest_trust_root_rejected`.

## How the embedded trust root is refreshed at release time

Today the binary embeds the production `trusted_root.json` shipped with the
`sigstore-trust-root` crate (`SIGSTORE_PRODUCTION_TRUSTED_ROOT`, used by
`verifier()` in `src/attest.rs`). That file is the TUF-distributed root from
the Sigstore public-good TUF repository at the time the crate version was
published. To refresh it:

1. On a branch, update the crate: `cargo update -p sigstore-trust-root`
   (or raise the version in `Cargo.toml` if the new root needs a new minor
   version), then run `cargo test --locked`. The
   `embedded_trust_root_parses_for_rotation_checks` test and the
   `tests/sigstore_verify.rs` fixtures must still pass.
2. Check that the embedded root contains the new key: compare it with the
   current root fetched through a TUF client (for example
   `cosign initialize` or the `sigstore-tuf` crate, which check TUF
   metadata signatures and expiry). Do not copy a `trusted_root.json` from
   an unverified source.
3. Open a PR (dependency change, reviewed as security-relevant), merge, and
   cut a release. The release runs in the `release` environment and needs
   Jack's approval like any other release; the residual risk in decision
   119 (same account can approve) applies.
4. Deploy the new runtime before the old key's end date.

If the crate has not yet published a refreshed root when one is needed, the
same steps apply with a release that embeds a TUF-fetched
`trusted_root.json` in the repository instead of the crate constant; that
switch is a code change and goes through a normal PR.
