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
        if [ "$#" -lt 5 ] || [ "$#" -gt 6 ]; then
            fail 'guard needs event, ref type, ref, workflow ref and optional version'
        fi
        if [ "$2" != push ] || [ "$3" != tag ]; then
            fail 'release requires a tag push'
        fi
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
    sdk-commit)
        # job.workflow_sha is the annotated tag object when a caller pins a tag.
        # The commits API peels that object, so comparing those SHAs always fails.
        # Resolve the named tag ourselves: a lightweight tag is its commit; an
        # annotated tag matches when the workflow SHA is that tag object or the
        # commit it points at. Any other object is rejected, including a different
        # tag that merely peels to the same commit.
        # The printed peeled commit is only the tag's target. It is not the OIDC
        # build_signer_digest and must not be compiled into VIBE_SDK_COMMIT.
        # That digest is the tag ref object SHA, embedded by release.yml.
        [ "$#" -eq 3 ] || fail 'sdk-commit needs version and workflow SHA'
        version=$2
        workflow_sha=$3
        version_ok "$version" || fail 'invalid SDK version'
        sha_ok "$workflow_sha" || fail 'SDK SHA must be 40 lowercase hex characters'
        # Command substitution strips trailing newlines, so this has to be a literal.
        newline='
'
        ref_meta=$(gh api "repos/vibe-backend/sdk/git/ref/tags/v$version" --jq '"\(.object.type) \(.object.sha)"') || fail 'SDK tag lookup failed'
        case $ref_meta in
            *"$newline"*|*"$carriage_return"*) fail 'SDK tag ref metadata is invalid' ;;
        esac
        ref_type=${ref_meta%% *}
        ref_sha=${ref_meta#* }
        [ "$ref_type $ref_sha" = "$ref_meta" ] || fail 'SDK tag ref metadata is invalid'
        sha_ok "$ref_sha" || fail 'SDK tag ref SHA is invalid'
        case $ref_type in
            commit)
                [ "$workflow_sha" = "$ref_sha" ] || fail 'SDK tag commit differs from reusable workflow commit'
                printf 'sdk_commit=%s\n' "$ref_sha"
                ;;
            tag)
                tag_meta=$(gh api "repos/vibe-backend/sdk/git/tags/$ref_sha" --jq '"\(.tag)\n\(.object.type)\n\(.object.sha)"') || fail 'SDK annotated tag lookup failed'
                case $tag_meta in
                    *"$carriage_return"*|*"$newline"*"$newline"*"$newline"*) fail 'annotated tag metadata is invalid' ;;
                    *"$newline"*"$newline"*) ;;
                    *) fail 'annotated tag metadata is invalid' ;;
                esac
                tag_name=${tag_meta%%"$newline"*}
                rest=${tag_meta#*"$newline"}
                target_type=${rest%%"$newline"*}
                target_sha=${rest#*"$newline"}
                [ "$tag_name" = "v$version" ] || fail 'annotated tag name differs from SDK release tag'
                [ "$target_type" = commit ] || fail 'annotated tag must point at a commit'
                sha_ok "$target_sha" || fail 'peeled SDK commit is invalid'
                [ "$workflow_sha" = "$ref_sha" ] || [ "$workflow_sha" = "$target_sha" ] || fail 'SDK tag commit differs from reusable workflow commit'
                printf 'sdk_commit=%s\n' "$target_sha"
                ;;
            *) fail 'SDK tag ref must be a commit or annotated tag' ;;
        esac
        ;;
    dry-run-ref)
        [ "$#" -eq 3 ] || fail 'dry-run-ref needs repository and SHA'
        [ "$2" = vibe-backend/sdk ] || fail 'dry-run SDK repository must be vibe-backend/sdk'
        sha_ok "$3" || fail 'SDK SHA must be 40 lowercase hex characters'
        printf 'sdk_sha=%s\n' "$3"
        ;;
    *) fail 'unknown project-release subcommand' ;;
esac
