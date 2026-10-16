# Installing and managing the `vibe` binary

> 繁體中文：[zh-hant/install.md](zh-hant/install.md)

`scripts/vibe-install.sh` installs the binary from `vibe-backend/sdk` GitHub Releases on Linux (x86_64/aarch64) and macOS (x86_64/arm64). On Windows, download the release zip directly; this POSIX shell manager does not support Windows.

## Usage

```sh
./scripts/vibe-install.sh install --version v1.2.3 --non-interactive
./scripts/vibe-install.sh upgrade --version v1.3.0 --non-interactive
./scripts/vibe-install.sh upgrade --non-interactive # resolves the latest stable release
./scripts/vibe-install.sh remove --non-interactive
```

The default destination is `$HOME/.local/bin/vibe`; override it with `--install-dir /absolute/path`. If the directory is not on `PATH`, the script prints a hint and never edits shell profiles. Run `--help` for all options.

## Security model

1. Platforms are allowlisted; unsupported platforms fail closed.
2. `install` requires an explicit stable `vX.Y.Z`. When `upgrade` has no version, it only accepts a GitHub `releases/latest` redirect within the same repository.
3. All downloads use HTTPS only (`--proto '=https'`) with TLS 1.2 or later (`--tlsv1.2`); any download failure stops the run.
4. The asset must first match the same release's `SHA256SUMS`, then pass `gh attestation verify` with an authenticated GitHub CLI (decision 118, spec 2C.3b/9C):

   ```sh
   gh attestation verify <asset> \
     --repo vibe-backend/sdk \
     --signer-workflow vibe-backend/sdk/.github/workflows/release.yml \
     --source-ref refs/tags/v<version> \
     --source-digest <commit sha of the tag> \
     --deny-self-hosted-runners
   ```

   The commit SHA is resolved from the tag with `gh api repos/vibe-backend/sdk/commits/refs/tags/v<version>`. The attestation identity is hard-coded in the script; `VIBE_INSTALL_REPOSITORY` and `VIBE_INSTALL_DOWNLOAD_URL` cannot change it. If any check fails, the existing version is left in place.
5. The archive may contain only a single top-level, non-symlink `vibe`. Each verified version lives in an immutable version directory, and the public `vibe` symlink is switched atomically with a single rename. An operation lock prevents install, upgrade and remove from racing.
6. The script never overwrites an unmanaged `vibe`. `remove` only accepts a managed symlink pointing at `.vibe-managed/versions/.../vibe`, and deletes only the managed root and that symlink, never the install directory or other files.
7. Re-installing the same version with identical content is a no-op.

### Trust boundaries and limits

The checksum detects transport corruption; the GitHub build attestation binds the asset to this repository's release workflow, tag and commit. Release environment, tag protection and workflow pinning controls are still required. The script never uses sudo; system directories need suitable permissions set up by the operator. SIGKILL or power loss can leave an unreferenced version directory, but the current symlink never points at a partial install; the next successful run cleans up old directories.

## Tests

```sh
python3 tests/install_manager.py
```

The tests use local mock release, curl and attestation commands and never contact GitHub. They cover fresh install, upgrade to latest, repeated runs, remove, preserving the existing version on network/checksum/attestation failure, the pinned attestation identity and flags, unsupported platforms, invalid destinations, the operation lock, and never overwriting or removing unmanaged files.
