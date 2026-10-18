#!/bin/sh
set -eu
BINARY=${VIBE_INSTALL_BINARY-sdk}
REPOSITORY=${VIBE_INSTALL_REPOSITORY:-vibe-backend/sdk}
DOWNLOAD_URL=${VIBE_INSTALL_DOWNLOAD_URL:-https://github.com}
# Provenance identity is fixed (D118, spec 2C.3b/9C); no environment variable can change it.
ATTEST_REPO=vibe-backend/sdk
ATTEST_WORKFLOW=vibe-backend/sdk/.github/workflows/release.yml
VERSION=
INSTALL_DIR=${VIBE_INSTALL_DIR:-"${HOME:-}/.local/bin"}
ACTION=
usage() {
 cat <<'USAGE'
Manage the vibe SDK or runtime binary installed from verified GitHub Releases.
Usage:
  vibe-install.sh install --version VERSION [options]
  vibe-install.sh upgrade [--version VERSION] [options]
  vibe-install.sh remove [options]
Options:
  --sdk / --runtime   Select binary (default: sdk; VIBE_INSTALL_BINARY=sdk|runtime).
  --version VERSION   Exact vX.Y.Z or vX.Y.Z-rc.N tag; required for install.
                      Upgrade resolves latest if omitted.
  --install-dir DIR   Destination (default: $HOME/.local/bin).
  --non-interactive   Never prompt (operations are non-interactive by default).
  -h, --help          Show help.
Requires curl, tar, SHA-256 tooling, and an authenticated GitHub CLI (`gh`) for
artifact-attestation verification.
USAGE
}
die(){ printf '%s\n' "error: $*" >&2; exit 1; }
need(){ command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }
[ "$#" -gt 0 ] || { usage >&2; exit 2; }
case $1 in install|upgrade|remove) ACTION=$1; shift;; -h|--help) usage; exit 0;; *) die "unknown operation: $1";; esac
while [ "$#" -gt 0 ]; do case $1 in
 --sdk) BINARY=sdk; shift;;
 --runtime) BINARY=runtime; shift;;
 --version) [ "$#" -ge 2 ] || die "--version requires a value"; VERSION=$2; shift 2;;
 --install-dir) [ "$#" -ge 2 ] || die "--install-dir requires a value"; INSTALL_DIR=$2; shift 2;;
 --non-interactive) shift;; -h|--help) usage; exit 0;; *) die "unknown option: $1";; esac; done
case $BINARY in sdk) PROGRAM=vibe;; runtime) PROGRAM=vibe-runtime;; *) die "VIBE_INSTALL_BINARY must be sdk or runtime";; esac
[ -n "$INSTALL_DIR" ] || die "install directory is empty"; case $INSTALL_DIR in /*) :;; *) die "install directory must be absolute";; esac
BIN="$INSTALL_DIR/$PROGRAM"; ROOT="$INSTALL_DIR/.$PROGRAM-managed"; LOCK="$INSTALL_DIR/.$PROGRAM-install.lock"
sha256_file(){ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"|awk '{print $1}'; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"|awk '{print $1}'; else die "sha256sum or shasum is required"; fi; }
mkdir -p -- "$INSTALL_DIR" || die "cannot create install directory: $INSTALL_DIR"; [ -w "$INSTALL_DIR" ] || die "install directory is not writable"
mkdir "$LOCK" 2>/dev/null || die "another install, upgrade, or remove operation is active"
tmp=; unlock(){ [ -z "$tmp" ] || rm -rf -- "$tmp"; rmdir "$LOCK" 2>/dev/null || :; }; interrupted(){ trap - EXIT HUP INT TERM; unlock; exit 130; }; trap unlock EXIT; trap interrupted HUP INT TERM
managed_target(){ [ -L "$BIN" ] || return 1; link=$(readlink "$BIN") || return 1; case $link in .$PROGRAM-managed/versions/*/"$PROGRAM") printf '%s/%s\n' "$INSTALL_DIR" "$link";; *) return 1;; esac; }
if [ "$ACTION" = remove ]; then
 target=$(managed_target) || die "no script-managed installation found"
 [ -f "$target" ] || die "managed target is missing; refusing unsafe removal"
 mv -- "$BIN" "$LOCK/removed-link" || die "failed to atomically detach managed binary"
 rm -rf -- "$ROOT" || { mv -- "$LOCK/removed-link" "$BIN"; die "failed to remove managed files"; }
 rm -f -- "$LOCK/removed-link"; printf 'Removed script-managed %s from %s\n' "$PROGRAM" "$INSTALL_DIR"; exit 0
fi
[ "$ACTION" != install ] || [ -n "$VERSION" ] || die "install requires --version vX.Y.Z or vX.Y.Z-rc.N"
need curl; need tar; need awk; need gh
if [ -z "$VERSION" ]; then
 no_stable="no stable release of $REPOSITORY exists yet (pre-releases are not 'latest'); pass --version vX.Y.Z-rc.N"
 # Keep curl's failure status separate from its HTTP status (404 means no stable).
 latest_status=0
 latest_response=$(curl -fsSIL --proto '=https' --tlsv1.2 -o /dev/null -w '%{http_code} %{url_effective}' "$DOWNLOAD_URL/$REPOSITORY/releases/latest") || latest_status=$?
 latest_code=${latest_response%% *}
 latest=${latest_response#* }
 [ "$latest_code" != 404 ] || die "$no_stable"
 [ "$latest_status" -eq 0 ] || die "failed to resolve latest release"
 prefix="$DOWNLOAD_URL/$REPOSITORY/releases/tag/"
 case $latest in
  "$prefix"*) VERSION=${latest#"$prefix"};;
  "$DOWNLOAD_URL/$REPOSITORY/releases"|"$DOWNLOAD_URL/$REPOSITORY/releases/") die "$no_stable";;
  *) die "latest release redirected outside expected repository";;
 esac
fi
if [ "$(printf '%s\n' "$VERSION" | wc -l)" -eq 1 ] && printf '%s\n' "$VERSION" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.(0|[1-9][0-9]*))?$'; then :; else die "invalid release version: $VERSION"; fi
os=$(uname -s) || die "cannot detect OS"; arch=$(uname -m) || die "cannot detect architecture"
case "$os:$arch" in Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu;; Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu;; Darwin:x86_64|Darwin:amd64) target=x86_64-apple-darwin;; Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin;; *) die "unsupported platform: $os/$arch";; esac
asset="vibe-$BINARY-$target.tar.gz"; base="$DOWNLOAD_URL/$REPOSITORY/releases/download/$VERSION"; tmp=$(mktemp -d "$INSTALL_DIR/.vibe-install.XXXXXX") || die "cannot create temporary directory"
curl -fL --retry 2 --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "failed to download SHA256SUMS"
count=$(awk -v n="$asset" '$2==n||$2=="*"n{found++} END{print found+0}' "$tmp/SHA256SUMS")
if [ "$count" -eq 0 ]; then
 if [ "$BINARY" = runtime ]; then die "this release has no vibe-runtime asset for $target"; fi
 asset="vibe-$target.tar.gz"
fi
expected=$(awk -v n="$asset" '$2==n||$2=="*"n{print $1; found++} END{if(found!=1)exit 1}' "$tmp/SHA256SUMS") || die "SHA256SUMS must contain exactly one asset entry"
curl -fL --retry 2 --proto '=https' --tlsv1.2 -o "$tmp/$asset" "$base/$asset" || die "failed to download $asset"
case $expected in ''|*[!0-9a-fA-F]*) die "invalid SHA-256";; esac; [ "${#expected}" -eq 64 ] || die "invalid SHA-256 length"
[ "$(sha256_file "$tmp/$asset")" = "$expected" ] || die "checksum verification failed"
source_sha=$(gh api "repos/$ATTEST_REPO/commits/refs/tags/$VERSION" --jq .sha) || die "failed to resolve commit for $VERSION in $ATTEST_REPO"
case $source_sha in *[!0-9a-f]*|'') die "invalid commit SHA for $VERSION";; esac; [ "${#source_sha}" -eq 40 ] || die "invalid commit SHA length for $VERSION"
gh attestation verify "$tmp/$asset" --repo "$ATTEST_REPO" --signer-workflow "$ATTEST_WORKFLOW" --source-ref "refs/tags/$VERSION" --source-digest "$source_sha" --deny-self-hosted-runners >/dev/null || die "GitHub artifact attestation verification failed"
[ "$(tar -tzf "$tmp/$asset" | wc -l | tr -d ' ')" = 1 ] || die "archive must contain exactly one entry"
[ "$(tar -tzf "$tmp/$asset")" = "$PROGRAM" ] || die "archive contains an unexpected path"
mkdir "$tmp/unpack"; tar -xzf "$tmp/$asset" -C "$tmp/unpack" || die "failed to extract archive"; if [ -f "$tmp/unpack/$PROGRAM" ] && [ ! -L "$tmp/unpack/$PROGRAM" ]; then :; else die "archive entry must be a regular file"; fi
chmod 755 "$tmp/unpack/$PROGRAM"; binary_sha=$(sha256_file "$tmp/unpack/$PROGRAM"); release_dir="$ROOT/versions/$VERSION-$binary_sha"
if old=$(managed_target 2>/dev/null) && [ "$old" = "$release_dir/$PROGRAM" ] && [ -x "$old" ]; then printf '%s %s is already installed at %s\n' "$PROGRAM" "$VERSION" "$BIN"; exit 0; fi
if [ -e "$BIN" ] || [ -L "$BIN" ]; then managed_target >/dev/null || die "$BIN exists but is not managed by this script"; fi
mkdir -p -- "$ROOT/versions"; printf '%s\n' managed-by-vibe-install > "$ROOT/OWNER"
if [ ! -d "$release_dir" ]; then mkdir "$release_dir.tmp.$$"; mv -- "$tmp/unpack/$PROGRAM" "$release_dir.tmp.$$/$PROGRAM"; printf 'version=%s\narchive_sha256=%s\nbinary_sha256=%s\n' "$VERSION" "$expected" "$binary_sha" > "$release_dir.tmp.$$/manifest"; mv -- "$release_dir.tmp.$$" "$release_dir"; fi
ln -s ".$PROGRAM-managed/versions/$VERSION-$binary_sha/$PROGRAM" "$LOCK/new-link" || die "failed to stage link"
mv -f -- "$LOCK/new-link" "$BIN" || die "failed to atomically switch installed version"
find "$ROOT/versions" -mindepth 1 -maxdepth 1 -type d ! -name "$VERSION-$binary_sha" -exec rm -rf -- {} +
printf 'Installed %s %s at %s\n' "$PROGRAM" "$VERSION" "$BIN"; case :$PATH: in *:"$INSTALL_DIR":*) :;; *) printf 'Add %s to PATH to run %s directly.\n' "$INSTALL_DIR" "$PROGRAM";; esac
