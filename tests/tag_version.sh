#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
check() {
 printf '[dependencies]\nversion = "9.9.9"\n[package]\nname = "test"\nversion = "%s"\n[dependencies.after]\nversion = "8.8.8"\n' "$1" > "$tmp/Cargo.toml"
 "$root/scripts/ci/check-tag-version.sh" "$2" "$tmp/Cargo.toml"
}
check 1.0.0-rc.2 v1.0.0-rc.2
check 1.0.0-rc.2 1.0.0-rc.2
check 1.0.0 v1.0.0
printf '[package]\nname = "test"\nversion = "1.0.0-rc.2"\n[dependencies]\nversion = "9.9.9"\n' > "$tmp/Cargo.toml"
"$root/scripts/ci/check-tag-version.sh" v1.0.0-rc.2 "$tmp/Cargo.toml"
if check 1.0.0-rc.2 v1.0.0-rc.1 > "$tmp/error" 2>&1; then
 echo 'mismatch unexpectedly accepted' >&2; exit 1
fi
grep -q '::error::' "$tmp/error"
# Exercise the strict awk fallback even on hosts with Python tomllib.
mkdir "$tmp/bin"
printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/python3"
chmod +x "$tmp/bin/python3"
PATH="$tmp/bin:$PATH"
export PATH
check 1.0.0-rc.2 v1.0.0-rc.2
check 1.0.0-rc.2 1.0.0-rc.2
check 1.0.0 v1.0.0
printf '[package]\nname = "test"\nversion = "1.0.0-rc.2"\n[dependencies]\nversion = "9.9.9"\n' > "$tmp/Cargo.toml"
"$root/scripts/ci/check-tag-version.sh" v1.0.0-rc.2 "$tmp/Cargo.toml"
if check 1.0.0-rc.2 v1.0.0-rc.1 > "$tmp/error" 2>&1; then
 echo 'awk fallback accepted mismatch' >&2; exit 1
fi
grep -q '::error::' "$tmp/error"
echo 'tag version tests passed'
