#!/usr/bin/env bash
# Bind release assets to SHA256SUMS (#134).
#
#   release-assets.sh verify <dir>          check <dir> and print the asset list
#   release-assets.sh publish <dir> <tag>   verify, then create the GitHub Release
#
# Rules: SHA256SUMS passes `sha256sum -c --strict`; every listed name is a plain
# file name (no path, no leading dash) of an expected archive type and appears
# once; the files in <dir> are exactly SHA256SUMS plus the listed names. Only
# those files are uploaded (never a glob of <dir>).
set -euo pipefail

die() { echo "::error::$*" >&2; exit 1; }

verify() {
  local dir=$1
  [ -f "$dir/SHA256SUMS" ] || die "missing $dir/SHA256SUMS"
  local listed actual
  if LC_ALL=C grep -Evq '^[0-9a-f]{64} [ *]vibe-(sdk|runtime)-[A-Za-z0-9._-]+$' "$dir/SHA256SUMS"; then
    die "malformed SHA256SUMS line"
  fi
  listed=$(cut -c67- "$dir/SHA256SUMS")
  [ -n "$listed" ] || die "SHA256SUMS lists no files"
  local name
  while IFS= read -r name; do
    case "$name" in
      -*|.*|SHA256SUMS) die "unexpected name in SHA256SUMS: $name" ;;
      vibe-sdk-*.tar.gz|vibe-sdk-*.zip|vibe-runtime-*.tar.gz|vibe-runtime-*.zip) ;;
      *) die "unexpected name in SHA256SUMS: $name" ;;
    esac
  done <<<"$listed"
  [ "$(LC_ALL=C sort <<<"$listed" | uniq -d)" = "" ] || die "duplicate names in SHA256SUMS"
  actual=$(cd "$dir" && LC_ALL=C find . -mindepth 1 -maxdepth 1 ! -name SHA256SUMS -printf '%f\n' | LC_ALL=C sort)
  if [ "$actual" != "$(LC_ALL=C sort <<<"$listed")" ]; then
    diff <(LC_ALL=C sort <<<"$listed") <(printf '%s\n' "$actual") >&2 || true
    die "files in $dir do not match SHA256SUMS (left: listed, right: present)"
  fi
  (cd "$dir" && sha256sum -c --strict --quiet SHA256SUMS) || die "SHA256SUMS check failed"
  printf '%s\n' "$listed"
}

publish() {
  local dir=$1 tag=$2 list
  list=$(verify "$dir")
  local -a files=()
  while IFS= read -r name; do files+=("$dir/$name"); done <<<"$list"
  files+=("$dir/SHA256SUMS")
  if gh release view "$tag" --repo "$GITHUB_REPOSITORY" >/dev/null 2>&1; then
    die "Release $tag already exists; refusing to overwrite (no --clobber)."
  fi
  gh release create "$tag" "${files[@]}" \
    --repo "$GITHUB_REPOSITORY" \
    --title "$tag" \
    --generate-notes
}

case "${1:-}" in
  verify) [ $# -eq 2 ] || die "usage: $0 verify <dir>"; verify "$2" ;;
  publish) [ $# -eq 3 ] || die "usage: $0 publish <dir> <tag>"; publish "$2" "$3" ;;
  *) die "usage: $0 verify <dir> | publish <dir> <tag>" ;;
esac
