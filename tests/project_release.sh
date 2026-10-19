#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
script=scripts/ci/project-release.sh
sha=0123456789abcdef0123456789abcdef01234567
sdk=vibe-backend/sdk/.github/workflows/vibe-project-release.yml
caller=acme/shop/.github/workflows/release.yml
ok() { "$script" "$@"; }
bad() {
    if output=$("$script" "$@" 2>&1); then
        echo "unexpected success: $*" >&2; exit 1
    fi
    case $output in *'::error::'*) ;; *) echo 'missing error annotation' >&2; exit 1 ;; esac
}
for v in 1.0.0 01.2.3 1.0.0-rc.4; do
    output=$(ok guard push tag "refs/tags/v$v" "$caller@refs/tags/v$v" "$v")
    printf '%s\n' "$output" | grep -Fx "version=$v"
    printf '%s\n' "$output" | grep -Fx "expect_identity=https://github.com/$caller@refs/tags/v*"
    printf '%s\n' "$output" | grep -Fx "self_identity=https://github.com/$caller@refs/tags/v$v"
    ok sdk-ref "$sdk@refs/tags/v$v" "$sha"
done
ok guard push tag refs/tags/v1.0.0 'acme/shop/.github/workflows/build.yaml@refs/tags/v1.0.0'
ok dry-run-ref vibe-backend/sdk "$sha"
bad guard pull_request tag refs/tags/v1.0.0 "$caller@refs/tags/v1.0.0"
bad guard push branch refs/tags/v1.0.0 "$caller@refs/tags/v1.0.0"
for ref in refs/heads/main refs/tags/1.0.0 refs/tags/v1.0 refs/tags/v1.0.0- 'refs/tags/v1.*' refs/tags/v1.0.0-a..b refs/tags/v1.0.0-.a refs/tags/v1.0.0-a.; do
    bad guard push tag "$ref" "$caller@$ref"
done
bad guard push tag refs/tags/v1.0.0 "$caller@refs/heads/main"
bad guard push tag refs/tags/v1.0.0 "$caller@refs/tags/v1.0.0" 2.0.0
for path in acme/shop/release.yml acme/shop/.github/workflows/release.txt acme/shop/.github/workflows/sub/release.yml /shop/.github/workflows/release.yml; do
    bad guard push tag refs/tags/v1.0.0 "$path@refs/tags/v1.0.0"
done
for ref in "$sdk@refs/heads/main" "$sdk@$sha" 'evil/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v1.0.0' 'vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.0.0' "$sdk@refs/tags/v1.0"; do
    bad sdk-ref "$ref" "$sha"
done
for invalid in ABCDEF0123456789abcdef0123456789abcdef0123 abc ''; do
    bad sdk-ref "$sdk@refs/tags/v1.0.0" "$invalid"
    bad dry-run-ref vibe-backend/sdk "$invalid"
done
bad dry-run-ref evil/sdk "$sha"
bad sdk-ref "$sdk@refs/tags/v1.0.0" "$sha
injected=value"
bad guard push tag 'refs/tags/v1.0.0
injected=value' "$caller@refs/tags/v1.0.0
injected=value"
bad unknown
bad guard
bad sdk-ref
bad dry-run-ref
echo 'project release guards passed'
