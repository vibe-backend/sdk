#!/bin/sh
set -eu
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo '::error::usage: check-tag-version.sh <tag> [Cargo.toml]' >&2; exit 1; }
tag=${1#v}
manifest=${2:-Cargo.toml}
if command -v python3 >/dev/null 2>&1 && python3 -c 'import tomllib' 2>/dev/null; then
 version=$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' "$manifest")
else
 version=$(awk '
 /^[[:space:]]*\[/ { package = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/) }
 package && /^[[:space:]]*version[[:space:]]*=/ {
   if ($0 !~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*(#.*)?$/) exit 1
   sub(/^[^"]*"/, ""); sub(/".*$/, ""); value=$0; count++
 }
 END { if(count != 1) exit 1; print value }
 ' "$manifest")
fi
[ "$tag" = "$version" ] || { printf '::error::Tag %s does not match Cargo.toml version %s\n' "$1" "$version" >&2; exit 1; }
