#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo 'usage: check-runtime-binary.sh <runtime-binary> <sdk-binary>' >&2; exit 1; }
for marker in 'Compile the source into lock markdown sections only' 'LLM_API_KEY was rejected' 'the fake LLM (LLM_PROVIDER=fake) only supports' 'Write these files? [y/N]'; do
 runtime_count=$(grep -c -F -a "$marker" "$1") || [ "$?" -eq 1 ]
 sdk_count=$(grep -c -F -a "$marker" "$2") || [ "$?" -eq 1 ]
 if [ "$runtime_count" -ne 0 ] || [ "$sdk_count" -eq 0 ]; then
  printf '::error::SDK-only marker check failed: %s\n' "$marker" >&2
  exit 1
 fi
done
