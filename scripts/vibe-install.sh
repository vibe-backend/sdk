#!/bin/sh
set -eu
PROGRAM=vibe
REPOSITORY=${VIBE_INSTALL_REPOSITORY:-vibe-backend/sdk}
DOWNLOAD_URL=${VIBE_INSTALL_DOWNLOAD_URL:-https://github.com}
# Provenance identity is fixed (D118, spec 2C.3b/9C); no environment variable can change it.
ATTEST_REPO=vibe-backend/sdk
ATTEST_WORKFLOW=vibe-backend/sdk/.github/workflows/release.yml
# Minimum installable release (#169). Hard-coded on purpose: no environment variable
# (including VIBE_INSTALL_*) or argument can lower it. It applies to install, upgrade and
# a resolved "latest", so an overridden download source cannot serve an older release.
# v0.1.0 is the planned first official release (Cargo.toml), which must include the
# v1-blocker security fixes (#116 release verification, #197 TLS).
# TODO(release checklist, docs/release-hardening.md): bump whenever a security fix ships.
MIN_VERSION=v0.1.0
VERSION=
EXPECTED_SHA256=
ALLOW_DOWNGRADE=0
INSTALL_DIR=${VIBE_INSTALL_DIR:-"${HOME:-}/.local/bin"}
ACTION=
usage() {
 cat <<'USAGE'
Manage the vibe binary installed from verified GitHub Releases.
Usage:
  vibe-install.sh install --version vX.Y.Z [options]
  vibe-install.sh upgrade [--version vX.Y.Z] [options]
  vibe-install.sh remove [options]
Options:
  --version VERSION   Exact stable tag; required for install. Upgrade resolves latest if omitted.
  --sha256 DIGEST     Require the downloaded archive to have this exact SHA-256.
  --allow-downgrade   Allow installing a version older than the installed one
                      (never below the built-in minimum).
  --install-dir DIR   Destination (default: $HOME/.local/bin).
  --non-interactive   Never prompt (operations are non-interactive by default).
  -h, --help          Show help.
Versions below the built-in minimum are always refused.
Requires curl, tar, SHA-256 tooling, and an authenticated GitHub CLI (`gh`) for
artifact-attestation verification.
USAGE
}
die(){ printf '%s\n' "error: $*" >&2; exit 1; }
need(){ command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }
# semver_cmp A B: prints -1, 0 or 1. Numeric per segment; prerelease per SemVer 2.0.0 section 11.
semver_cmp(){ awk -v a="$1" -v b="$2" '
function numcmp(x,y){ sub(/^0+/,"",x); sub(/^0+/,"",y); if(length(x)!=length(y)) return length(x)<length(y)?-1:1; if(x==y) return 0; return x<y?-1:1 }
function isnum(x){ return x ~ /^[0-9]+$/ }
function idcmp(x,y){ if(isnum(x)&&isnum(y)) return numcmp(x,y); if(isnum(x)) return -1; if(isnum(y)) return 1; if(x==y) return 0; return x<y?-1:1 }
function split_v(v,core,pre){ sub(/^v/,"",v); sub(/\+.*/,"",v); i=index(v,"-"); if(i){ pre[0]=substr(v,i+1); v=substr(v,1,i-1) } else pre[0]=""; return split(v,core,".") }
BEGIN{ split_v(a,ca,pa); split_v(b,cb,pb)
 for(k=1;k<=3;k++){ r=numcmp(ca[k],cb[k]); if(r){print r; exit} }
 if(pa[0]==""&&pb[0]==""){print 0; exit} if(pa[0]==""){print 1; exit} if(pb[0]==""){print -1; exit}
 na=split(pa[0],xa,"."); nb=split(pb[0],xb,".")
 for(k=1;k<=na&&k<=nb;k++){ r=idcmp(xa[k],xb[k]); if(r){print r; exit} }
 print (na<nb?-1:(na>nb?1:0)) }'; }
[ "$#" -gt 0 ] || { usage >&2; exit 2; }
if [ "$1" = __compare-versions ]; then [ "$#" -eq 3 ] || exit 2; semver_cmp "$2" "$3"; exit 0; fi
case $1 in install|upgrade|remove) ACTION=$1; shift;; -h|--help) usage; exit 0;; *) die "unknown operation: $1";; esac
while [ "$#" -gt 0 ]; do case $1 in
 --version) [ "$#" -ge 2 ] || die "--version requires a value"; VERSION=$2; shift 2;;
 --sha256) [ "$#" -ge 2 ] || die "--sha256 requires a value"; EXPECTED_SHA256=$(printf '%s' "$2" | tr 'A-F' 'a-f'); shift 2
  case $EXPECTED_SHA256 in *[!0-9a-f]*|'') die "--sha256 must be a 64-character hex digest";; esac; [ "${#EXPECTED_SHA256}" -eq 64 ] || die "--sha256 must be a 64-character hex digest";;
 --allow-downgrade) ALLOW_DOWNGRADE=1; shift;;
 --install-dir) [ "$#" -ge 2 ] || die "--install-dir requires a value"; INSTALL_DIR=$2; shift 2;;
 --non-interactive) shift;; -h|--help) usage; exit 0;; *) die "unknown option: $1";; esac; done
[ -n "$INSTALL_DIR" ] || die "install directory is empty"; case $INSTALL_DIR in /*) :;; *) die "install directory must be absolute";; esac
BIN="$INSTALL_DIR/$PROGRAM"; ROOT="$INSTALL_DIR/.$PROGRAM-managed"; LOCK="$INSTALL_DIR/.$PROGRAM-install.lock"
sha256_file(){ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"|awk '{print $1}'; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"|awk '{print $1}'; else die "sha256sum or shasum is required"; fi; }
mkdir -p -- "$INSTALL_DIR" || die "cannot create install directory: $INSTALL_DIR"; [ -w "$INSTALL_DIR" ] || die "install directory is not writable"
mkdir "$LOCK" 2>/dev/null || die "another install, upgrade, or remove operation is active"
tmp=; unlock(){ [ -z "$tmp" ] || rm -rf -- "$tmp"; rmdir "$LOCK" 2>/dev/null || :; }; interrupted(){ trap - EXIT HUP INT TERM; unlock; exit 130; }; trap unlock EXIT; trap interrupted HUP INT TERM
managed_target(){ [ -L "$BIN" ] || return 1; link=$(readlink "$BIN") || return 1; case $link in .$PROGRAM-managed/versions/*/vibe) printf '%s/%s\n' "$INSTALL_DIR" "$link";; *) return 1;; esac; }
if [ "$ACTION" = remove ]; then
 target=$(managed_target) || die "no script-managed installation found"
 [ -f "$target" ] || die "managed target is missing; refusing unsafe removal"
 mv -- "$BIN" "$LOCK/removed-link" || die "failed to atomically detach managed binary"
 rm -rf -- "$ROOT" || { mv -- "$LOCK/removed-link" "$BIN"; die "failed to remove managed files"; }
 rm -f -- "$LOCK/removed-link"; printf 'Removed script-managed vibe from %s\n' "$INSTALL_DIR"; exit 0
fi
[ "$ACTION" != install ] || [ -n "$VERSION" ] || die "install requires --version vX.Y.Z"
need curl; need tar; need awk; need gh
if [ -z "$VERSION" ]; then latest=$(curl -fsSIL --proto '=https' --tlsv1.2 -o /dev/null -w '%{url_effective}' "$DOWNLOAD_URL/$REPOSITORY/releases/latest") || die "failed to resolve latest release"; prefix="$DOWNLOAD_URL/$REPOSITORY/releases/tag/"; case $latest in "$prefix"*) VERSION=${latest#"$prefix"};; *) die "latest release redirected outside expected repository";; esac; fi
if [ "$(printf '%s\n' "$VERSION" | wc -l)" -eq 1 ] && printf '%s\n' "$VERSION" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then :; else die "invalid stable release version: $VERSION"; fi
[ "$(semver_cmp "$VERSION" "$MIN_VERSION")" -ge 0 ] || die "refusing $VERSION: below the minimum supported version $MIN_VERSION (older releases may lack security fixes)"
if cur=$(managed_target 2>/dev/null) && [ -f "${cur%/vibe}/manifest" ]; then
 installed=$(awk -F= '$1=="version"{print $2; exit}' "${cur%/vibe}/manifest")
 if [ -n "$installed" ]; then
  if [ "$(printf '%s\n' "$installed" | wc -l)" -eq 1 ] && printf '%s\n' "$installed" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
   if [ "$(semver_cmp "$VERSION" "$installed")" -lt 0 ]; then
    [ "$ALLOW_DOWNGRADE" = 1 ] || die "refusing downgrade from $installed to $VERSION; pass --allow-downgrade to override"
   fi
  else
   [ "$ALLOW_DOWNGRADE" = 1 ] || die "refusing to replace non-semver installed version $installed with $VERSION; pass --allow-downgrade to override"
  fi
 fi
fi
os=$(uname -s) || die "cannot detect OS"; arch=$(uname -m) || die "cannot detect architecture"
case "$os:$arch" in Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu;; Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu;; Darwin:x86_64|Darwin:amd64) target=x86_64-apple-darwin;; Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin;; *) die "unsupported platform: $os/$arch";; esac
asset="$PROGRAM-$target.tar.gz"; base="$DOWNLOAD_URL/$REPOSITORY/releases/download/$VERSION"; tmp=$(mktemp -d "$INSTALL_DIR/.vibe-install.XXXXXX") || die "cannot create temporary directory"
curl -fL --retry 2 --proto '=https' --tlsv1.2 -o "$tmp/$asset" "$base/$asset" || die "failed to download $asset"
curl -fL --retry 2 --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "failed to download SHA256SUMS"
expected=$(awk -v n="$asset" '$2==n||$2=="*"n{print $1; found++} END{if(found!=1)exit 1}' "$tmp/SHA256SUMS") || die "SHA256SUMS must contain exactly one asset entry"
case $expected in ''|*[!0-9a-fA-F]*) die "invalid SHA-256";; esac; [ "${#expected}" -eq 64 ] || die "invalid SHA-256 length"
actual=$(sha256_file "$tmp/$asset"); [ "$actual" = "$expected" ] || die "checksum verification failed"
[ -z "$EXPECTED_SHA256" ] || [ "$actual" = "$EXPECTED_SHA256" ] || die "archive SHA-256 does not match --sha256"
source_sha=$(gh api "repos/$ATTEST_REPO/commits/refs/tags/$VERSION" --jq .sha) || die "failed to resolve commit for $VERSION in $ATTEST_REPO"
case $source_sha in *[!0-9a-f]*|'') die "invalid commit SHA for $VERSION";; esac; [ "${#source_sha}" -eq 40 ] || die "invalid commit SHA length for $VERSION"
gh attestation verify "$tmp/$asset" --repo "$ATTEST_REPO" --signer-workflow "$ATTEST_WORKFLOW" --source-ref "refs/tags/$VERSION" --source-digest "$source_sha" --deny-self-hosted-runners >/dev/null || die "GitHub artifact attestation verification failed"
[ "$(tar -tzf "$tmp/$asset" | wc -l | tr -d ' ')" = 1 ] || die "archive must contain exactly one entry"
[ "$(tar -tzf "$tmp/$asset")" = vibe ] || die "archive contains an unexpected path"
mkdir "$tmp/unpack"; tar -xzf "$tmp/$asset" -C "$tmp/unpack" || die "failed to extract archive"; if [ -f "$tmp/unpack/vibe" ] && [ ! -L "$tmp/unpack/vibe" ]; then :; else die "archive entry must be a regular file"; fi
chmod 755 "$tmp/unpack/vibe"; binary_sha=$(sha256_file "$tmp/unpack/vibe"); release_dir="$ROOT/versions/$VERSION-$binary_sha"
if old=$(managed_target 2>/dev/null) && [ "$old" = "$release_dir/vibe" ] && [ -x "$old" ]; then printf 'vibe %s is already installed at %s\n' "$VERSION" "$BIN"; exit 0; fi
if [ -e "$BIN" ] || [ -L "$BIN" ]; then managed_target >/dev/null || die "$BIN exists but is not managed by this script"; fi
mkdir -p -- "$ROOT/versions"; printf '%s\n' managed-by-vibe-install > "$ROOT/OWNER"
if [ ! -d "$release_dir" ]; then mkdir "$release_dir.tmp.$$"; mv -- "$tmp/unpack/vibe" "$release_dir.tmp.$$/vibe"; printf 'version=%s\narchive_sha256=%s\nbinary_sha256=%s\n' "$VERSION" "$expected" "$binary_sha" > "$release_dir.tmp.$$/manifest"; mv -- "$release_dir.tmp.$$" "$release_dir"; fi
ln -s ".$PROGRAM-managed/versions/$VERSION-$binary_sha/vibe" "$LOCK/new-link" || die "failed to stage link"
mv -f -- "$LOCK/new-link" "$BIN" || die "failed to atomically switch installed version"
find "$ROOT/versions" -mindepth 1 -maxdepth 1 -type d ! -name "$VERSION-$binary_sha" -exec rm -rf -- {} +
printf 'Installed vibe %s at %s\n' "$VERSION" "$BIN"; case :$PATH: in *:"$INSTALL_DIR":*) :;; *) printf 'Add %s to PATH to run vibe directly.\n' "$INSTALL_DIR";; esac
