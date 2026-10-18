# Installing and managing the `vibe` binary

> 繁體中文：[zh-hant/install.md](zh-hant/install.md)

`scripts/vibe-install.sh` installs the binary from `vibe-backend/sdk` GitHub Releases on Linux (x86_64/aarch64) and macOS (x86_64/arm64). On Windows, download the release zip directly; this POSIX shell manager does not support Windows.

The default **vibe-sdk** installs `vibe` for interactive development, builds,
and `vibe run` (the three SDK commands are `vibe`, `vibe build`, `vibe run`). **vibe-runtime** installs `vibe-runtime` for
deployment: it runs artifacts only, with no LLM client, build or interactive code.
`vibe-runtime run --artifact <dir>` and bare `vibe-runtime` accept the same flags;
artifact defaults to `.`, with `--addr`, `--db` / `VIBE_DB`, and
`--allow-test-build` (development only).

Archives are `vibe-sdk-<target>.tar.gz` / `.zip` and
`vibe-runtime-<target>.tar.gz` / `.zip`. SHA256SUMS and attestations cover both.
Flags `--sdk` / `--runtime` override `VIBE_INSTALL_BINARY=sdk|runtime` (default sdk).
Both can coexist: SDK uses `.vibe-managed` and `.vibe-install.lock`; runtime uses
`.vibe-runtime-managed` and `.vibe-runtime-install.lock`. Removal affects only the
chosen binary. Each archive contains exactly its selected regular binary file.
SDK alone supports legacy `vibe-<target>.tar.gz` assets from before rc.2 when the
new SDK name is absent from SHA256SUMS. Runtime has no legacy fallback.
Checksum and pinned attestation verification are identical for both binaries.

## Usage

```sh
./scripts/vibe-install.sh install --version v1.0.0-rc.2
./scripts/vibe-install.sh install --version v1.0.0-rc.2 --runtime
VIBE_INSTALL_BINARY=runtime ./scripts/vibe-install.sh install --version v1.0.0-rc.2
./scripts/vibe-install.sh upgrade --non-interactive
./scripts/vibe-install.sh remove --runtime
./scripts/vibe-install.sh remove
```

`install --version` and `upgrade --version` accept `vX.Y.Z` or `vX.Y.Z-rc.N`. Every number must be zero or a positive integer without leading zeros; other pre-release labels and build metadata are rejected.

`upgrade` without `--version` requires a stable GitHub Release: pre-releases are not `latest`. Until GA, pass `--version v1.0.0-rc.2` explicitly. If no stable release exists, the script stops with guidance to use `--version`.

The default destination is `$HOME/.local/bin/vibe`; override it with `--install-dir /absolute/path`. If the directory is not on `PATH`, the script prints a hint and never edits shell profiles. Run `--help` for all options.

## Build from source

```sh
cargo build --release --bin vibe
cargo build --release --no-default-features --bin vibe-runtime
```

## Security model

1. Platforms are allowlisted; unsupported platforms fail closed.
2. `install` requires an explicit `vX.Y.Z` or `vX.Y.Z-rc.N` tag. When `upgrade` has no version, it only accepts a GitHub `releases/latest` redirect within the same repository.
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

   The commit SHA is resolved from the tag with `gh api repos/vibe-backend/sdk/commits/refs/tags/v<version>`. Stable releases and pre-releases use the identical attestation identity: workflow `.github/workflows/release.yml` of `vibe-backend/sdk`, source ref `refs/tags/v<version>` (including any `-rc.N` suffix), and issuer `https://token.actions.githubusercontent.com` (the `gh` default). This identity is hard-coded in the script; `VIBE_INSTALL_REPOSITORY` and `VIBE_INSTALL_DOWNLOAD_URL` cannot change it. If any check fails, the existing version is left in place.
5. The archive may contain only a single top-level, non-symlink selected binary (`vibe` or `vibe-runtime`). Each verified version lives in an immutable version directory, and the public binary symlink is switched atomically with a single rename. An operation lock prevents install, upgrade and remove from racing.
6. The script never overwrites an unmanaged selected binary. `remove` only accepts a managed symlink pointing into the selected managed root, and deletes only the managed root and that symlink, never the install directory or other files.
7. Re-installing the same version with identical content is a no-op.

### Trust boundaries and limits

The checksum detects transport corruption; the GitHub build attestation binds the asset to this repository's release workflow, tag and commit. Release environment, tag protection and workflow pinning controls are still required. The script never uses sudo; system directories need suitable permissions set up by the operator. SIGKILL or power loss can leave an unreferenced version directory, but the current symlink never points at a partial install; the next successful run cleans up old directories.

## Tests

```sh
python3 tests/install_manager.py
```

The tests use local mock release, curl and attestation commands and never contact GitHub. They cover installing `vX.Y.Z` or `vX.Y.Z-rc.N`, upgrades from stable to rc and rc to stable, rejection of invalid and multi-line versions before network calls, upgrade to latest, repeated runs, remove, preserving the existing version on network/checksum/attestation failure (including rc verification failures), the pinned attestation identity and flags, unsupported platforms, invalid destinations, the operation lock, and never overwriting or removing unmanaged files.
