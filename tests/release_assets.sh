#!/usr/bin/env bash
# Tests for scripts/ci/release-assets.sh, including a rehearsal of the tag-path
# publish step with a stub `gh` (no token, nothing is published).
# Usage: tests/release_assets.sh [dist-dir]   (default: synthetic archives)
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
script="$here/scripts/ci/release-assets.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0

mkdist() {
  rm -rf "$work/dist"; mkdir -p "$work/dist"
  if [ -n "${SRC:-}" ]; then
    cp "$SRC"/vibe-* "$work/dist/"
  else
    echo a >"$work/dist/vibe-sdk-x86_64-unknown-linux-gnu.tar.gz"
    echo c >"$work/dist/vibe-runtime-x86_64-unknown-linux-gnu.tar.gz"
    echo d >"$work/dist/vibe-runtime-x86_64-pc-windows-msvc.zip"
    echo b >"$work/dist/vibe-sdk-x86_64-pc-windows-msvc.zip"
  fi
  (cd "$work/dist" && sha256sum -- vibe-* >SHA256SUMS)
}
expect_ok() { if "$@" >/dev/null 2>&1; then echo "ok   $T"; else echo "FAIL $T"; fail=1; fi; }
expect_err() { if "$@" >/dev/null 2>&1; then echo "FAIL $T (accepted)"; fail=1; else echo "ok   $T"; fi; }

SRC=${1:-}

T="valid dist";             mkdist; expect_ok "$script" verify "$work/dist"
T="extra unlisted file";    mkdist; echo x >"$work/dist/vibe-extra.tar.gz"; expect_err "$script" verify "$work/dist"
T="listed file missing";    mkdist; first=$(cut -c67- "$work/dist/SHA256SUMS" | head -1); rm "$work/dist/$first"; expect_err "$script" verify "$work/dist"
T="tampered file";          mkdist; f="$work/dist/$(cut -c67- "$work/dist/SHA256SUMS" | head -1)"; echo tamper >>"$f"; expect_err "$script" verify "$work/dist"
T="missing SHA256SUMS";     mkdist; rm "$work/dist/SHA256SUMS"; expect_err "$script" verify "$work/dist"
T="malformed line";         mkdist; echo "garbage" >>"$work/dist/SHA256SUMS"; expect_err "$script" verify "$work/dist"
T="path in name";           mkdist; h=$(sha256sum "$f" | cut -c1-64); echo "$h  ../vibe-x.tar.gz" >>"$work/dist/SHA256SUMS"; expect_err "$script" verify "$work/dist"
T="duplicate entry";        mkdist; line=$(head -1 "$work/dist/SHA256SUMS"); echo "$line" >>"$work/dist/SHA256SUMS"; expect_err "$script" verify "$work/dist"
T="unexpected file type";   mkdist; echo x >"$work/dist/notes.txt"; expect_err "$script" verify "$work/dist"

# Tag-path rehearsal: run the publish step exactly as the release job does,
# with a stub gh that records its arguments.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
if [ "$1 $2" = "release view" ]; then exit 1; fi   # release does not exist yet
printf '%s\n' "$@" >"$GH_LOG"
STUB
chmod +x "$work/bin/gh"
export GH_LOG="$work/gh.log" GITHUB_REPOSITORY=example/sdk
T="publish uploads exactly SHA256SUMS + listed files"; mkdist
if PATH="$work/bin:$PATH" "$script" publish "$work/dist" v0.0.0-rehearsal >/dev/null; then
  got=$(grep "^$work/dist/" "$GH_LOG" | sed "s|^$work/dist/||" | LC_ALL=C sort)
  want=$( (cut -c67- "$work/dist/SHA256SUMS"; echo SHA256SUMS) | LC_ALL=C sort)
  if [ "$got" = "$want" ] && grep -qx "v0.0.0-rehearsal" "$GH_LOG"; then echo "ok   $T"; echo "       ${got//$'\n'/ }"; else echo "FAIL $T"; cat "$GH_LOG"; fail=1; fi
else echo "FAIL $T"; fail=1; fi
T="publish refuses tampered dist"; mkdist; rm -f "$GH_LOG"; echo x >"$work/dist/vibe-extra.zip"
if PATH="$work/bin:$PATH" "$script" publish "$work/dist" v0.0.0-rehearsal >/dev/null 2>&1 || [ -e "$GH_LOG" ]; then echo "FAIL $T"; fail=1; else echo "ok   $T"; fi
T="publish refuses existing release"; mkdist
printf '#!/usr/bin/env bash\nexit 0\n' >"$work/bin/gh"
expect_err env PATH="$work/bin:$PATH" "$script" publish "$work/dist" v0.0.0-rehearsal

exit $fail
