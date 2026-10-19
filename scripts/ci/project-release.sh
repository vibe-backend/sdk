#!/bin/sh
# Validate untrusted workflow metadata before producing GitHub outputs.
set -eu
fail() { echo "::error::$*" >&2; exit 1; }
version_ok() {
    printf '%s\n' "$1" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'
}
sha_ok() { printf '%s\n' "$1" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$'; }
# Outputs are single-line records; reject control characters before regex checks.
carriage_return=$(printf '\r')
for argument do
    case $argument in
        *'
'*|*"$carriage_return"*) fail 'metadata must be a single line' ;;
    esac
done
case ${1-} in
    guard)
        [ "$#" -ge 5 ] && [ "$#" -le 6 ] || fail 'guard needs event, ref type, ref, workflow ref and optional version'
        [ "$2" = push ] && [ "$3" = tag ] || fail 'release requires a tag push'
        case $4 in refs/tags/v*) version=${4#refs/tags/v} ;; *) fail 'release requires refs/tags/v<SemVer>' ;; esac
        version_ok "$version" || fail 'invalid release version'
        case $5 in *"@$4") caller=${5%"@$4"} ;; *) fail 'caller workflow must use the release tag' ;; esac
        printf '%s\n' "$caller" | LC_ALL=C grep -Eq '^[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+/\.github/workflows/[A-Za-z0-9_.-]+\.ya?ml$' || fail 'invalid caller workflow path'
        [ -z "${6-}" ] || [ "$6" = "$version" ] || fail 'release-version differs from tag'
        printf 'version=%s\nexpect_identity=https://github.com/%s@refs/tags/v*\nself_identity=https://github.com/%s\n' "$version" "$caller" "$5"
        ;;
    sdk-ref)
        [ "$#" -eq 3 ] || fail 'sdk-ref needs workflow ref and SHA'
        case $2 in vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v*) version=${2#vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v} ;; *) fail 'SDK workflow must use its official release tag, not a branch or SHA' ;; esac
        version_ok "$version" || fail 'invalid SDK version'
        sha_ok "$3" || fail 'SDK SHA must be 40 lowercase hex characters'
        printf 'sdk_version=%s\nsdk_sha=%s\n' "$version" "$3"
        ;;
    dry-run-ref)
        [ "$#" -eq 3 ] || fail 'dry-run-ref needs repository and SHA'
        [ "$2" = vibe-backend/sdk ] || fail 'dry-run SDK repository must be vibe-backend/sdk'
        sha_ok "$3" || fail 'SDK SHA must be 40 lowercase hex characters'
        printf 'sdk_sha=%s\n' "$3"
        ;;
    *) fail 'unknown project-release subcommand' ;;
esac
