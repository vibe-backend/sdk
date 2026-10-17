#!/bin/sh
set -eu
tree=$(cargo tree --locked --no-default-features -e features -i vibe)
if printf '%s\n' "$tree" | grep -F '"sdk"'; then
 echo '::error::runtime enables the sdk feature' >&2
 exit 1
fi
