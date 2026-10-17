#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo 'usage: check-runtime-binary.sh <runtime-binary> <sdk-binary>' >&2; exit 1; }
# On x86_64-apple-darwin, format! strings with arguments were not contiguous in the binary; markers must be plain literals without format arguments.
for marker in 'Compile the source into lock markdown sections only' 'CI and production compilation require an https public host' 'the fake LLM (LLM_PROVIDER=fake) only supports' 'Write these files? [y/N]'; do
 runtime_count=$(grep -c -F -a "$marker" "$1") || [ "$?" -eq 1 ]
 sdk_count=$(grep -c -F -a "$marker" "$2") || [ "$?" -eq 1 ]
 if [ "$runtime_count" -ne 0 ] || [ "$sdk_count" -eq 0 ]; then
  printf '::error::SDK-only marker check failed: %s (runtime count %s, sdk count %s)\n' "$marker" "$runtime_count" "$sdk_count" >&2
  exit 1
 fi
done
