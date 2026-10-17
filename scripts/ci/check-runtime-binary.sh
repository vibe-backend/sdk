#!/bin/sh
export LC_ALL=C
set -eu
[ "$#" -eq 2 ] || { echo 'usage: check-runtime-binary.sh <runtime-binary> <sdk-binary>' >&2; exit 1; }
# Markers are plain string literals from SDK-only modules; LC_ALL=C makes BSD grep match binary bytes.
failed=0
for marker in 'Compile the source into lock markdown sections only' 'CI and production compilation require an https public host' 'the fake LLM (LLM_PROVIDER=fake) only supports' 'Write these files? [y/N]'; do
 runtime_count=$(grep -c -F -a "$marker" "$1") || [ "$?" -eq 1 ]
 sdk_count=$(grep -c -F -a "$marker" "$2") || [ "$?" -eq 1 ]
 printf '%s: runtime %s, sdk %s\n' "$marker" "$runtime_count" "$sdk_count" >&2
 if [ "$runtime_count" -ne 0 ] || [ "$sdk_count" -eq 0 ]; then
  printf '::error::SDK-only marker check failed: %s (runtime count %s, sdk count %s)\n' "$marker" "$runtime_count" "$sdk_count" >&2
  failed=1
 fi
done
exit "$failed"
