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
bad sdk-commit
bad dry-run-ref
bad sdk-commit 1.0.0
bad sdk-commit 1.0.0 "$sha" extra

# v1.0.0-rc.5 is an annotated tag: object f1ff9e1… points at commit f08969f….
# Actions passes the tag object as workflow_sha. The commits API peels it, so
# those SHAs differ even though they are the same tag.
tag_object=f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
peeled=f08969fe8dce0ce80b1d08d7164d4506ff9d2600
other_tag=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
fixture_dir=$(mktemp -d)
log=$fixture_dir/log
trap 'rm -rf "$fixture_dir"' EXIT
cat > "$fixture_dir/gh" << 'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "${SDK_TAG_LOG:?}"
[ "${1-}" = api ] || { echo 'unexpected gh command' >&2; exit 1; }
[ "${3-}" = --jq ] || { echo 'expected gh api --jq' >&2; exit 1; }
url=$2
filter=$4
case $url in
    *'/commits/'*) echo 'commits API peels annotated tags' >&2; exit 1 ;;
esac
ref_filter='"\(.object.type) \(.object.sha)"'
tag_filter='"\(.tag)\n\(.object.type)\n\(.object.sha)"'
case ${SDK_TAG_FIXTURE:?} in
    annotated)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5)
                [ "$filter" = "$ref_filter" ] || { echo 'unexpected ref filter' >&2; exit 1; }
                printf '%s %s\n' tag f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
                ;;
            repos/vibe-backend/sdk/git/tags/f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e)
                [ "$filter" = "$tag_filter" ] || { echo 'unexpected tag filter' >&2; exit 1; }
                printf '%s\n%s\n%s\n' v1.0.0-rc.5 commit f08969fe8dce0ce80b1d08d7164d4506ff9d2600
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    lightweight)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.2.3)
                [ "$filter" = "$ref_filter" ] || { echo 'unexpected ref filter' >&2; exit 1; }
                printf '%s %s\n' commit 0123456789abcdef0123456789abcdef01234567
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    nested)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5)
                [ "$filter" = "$ref_filter" ] || exit 1
                printf '%s %s\n' tag f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
                ;;
            repos/vibe-backend/sdk/git/tags/f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e)
                [ "$filter" = "$tag_filter" ] || exit 1
                printf '%s\n%s\n%s\n' v1.0.0-rc.5 tag f08969fe8dce0ce80b1d08d7164d4506ff9d2600
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    wrong-name)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5)
                printf '%s %s\n' tag f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
                ;;
            repos/vibe-backend/sdk/git/tags/f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e)
                printf '%s\n%s\n%s\n' v9.9.9 commit f08969fe8dce0ce80b1d08d7164d4506ff9d2600
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    bad-target)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5)
                printf '%s %s\n' tag f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
                ;;
            repos/vibe-backend/sdk/git/tags/f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e)
                printf '%s\n%s\n%s\n' v1.0.0-rc.5 commit not-a-sha
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    tree)
        case $url in
            repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5)
                printf '%s %s\n' tree f1ff9e1ad8b751ca00234cf8fc6fd80aca0b342e
                ;;
            *) echo "unexpected url $url" >&2; exit 1 ;;
        esac
        ;;
    missing)
        echo 'tag not found' >&2
        exit 1
        ;;
    *) echo "unknown fixture ${SDK_TAG_FIXTURE}" >&2; exit 1 ;;
esac
EOF
chmod +x "$fixture_dir/gh"
run_commit() {
    : > "$log"
    SDK_TAG_FIXTURE=$1 SDK_TAG_LOG=$log PATH="$fixture_dir:$PATH" "$script" sdk-commit "$2" "$3"
}
fail_commit() {
    fixture=$1
    version=$2
    commit_sha=$3
    shift 3
    : > "$log"
    if output=$(SDK_TAG_FIXTURE=$fixture SDK_TAG_LOG=$log PATH="$fixture_dir:$PATH" "$script" sdk-commit "$version" "$commit_sha" 2>&1); then
        echo "unexpected success: sdk-commit $version $commit_sha" >&2
        exit 1
    fi
    case $output in *'::error::'*) ;; *) echo 'missing error annotation' >&2; exit 1 ;; esac
    for needle do
        case $output in *"$needle"*) ;; *) printf 'missing %s\n%s\n' "$needle" "$output" >&2; exit 1 ;; esac
    done
}
output=$(run_commit annotated 1.0.0-rc.5 "$tag_object")
printf '%s\n' "$output" | grep -Fx "sdk_commit=$peeled"
grep -F 'repos/vibe-backend/sdk/git/ref/tags/v1.0.0-rc.5' "$log" >/dev/null
grep -F "repos/vibe-backend/sdk/git/tags/$tag_object" "$log" >/dev/null
if grep -F '/commits/' "$log" >/dev/null; then echo 'used commits API' >&2; exit 1; fi
output=$(run_commit annotated 1.0.0-rc.5 "$peeled")
printf '%s\n' "$output" | grep -Fx "sdk_commit=$peeled"
output=$(run_commit lightweight 1.2.3 "$sha")
printf '%s\n' "$output" | grep -Fx "sdk_commit=$sha"
if grep -F '/git/tags/' "$log" >/dev/null || grep -F '/commits/' "$log" >/dev/null; then
    echo 'lightweight tag used the wrong API' >&2
    exit 1
fi
fail_commit annotated 1.0.0-rc.5 "$other_tag" 'SDK tag commit differs from reusable workflow commit'
if grep -F "$other_tag" "$log" >/dev/null; then echo 'peeled an unrelated tag object' >&2; exit 1; fi
fail_commit annotated 1.0.0-rc.5 "$sha" 'SDK tag commit differs from reusable workflow commit'
fail_commit lightweight 1.2.3 "$peeled" 'SDK tag commit differs from reusable workflow commit'
fail_commit nested 1.0.0-rc.5 "$tag_object" 'annotated tag must point at a commit'
fail_commit wrong-name 1.0.0-rc.5 "$tag_object" 'annotated tag name differs from SDK release tag'
fail_commit bad-target 1.0.0-rc.5 "$tag_object" 'peeled SDK commit is invalid'
fail_commit tree 1.0.0-rc.5 "$tag_object" 'SDK tag ref must be a commit or annotated tag'
fail_commit missing 1.0.0-rc.5 "$tag_object" 'SDK tag lookup failed'
fail_commit annotated 1.0 "$sha" 'invalid SDK version'
[ ! -s "$log" ]
fail_commit annotated 1.0.0-rc.5 ABCDEF 'SDK SHA must be 40 lowercase hex characters'
[ ! -s "$log" ]
if grep -n 'commits/refs/tags' scripts/ci/project-release.sh .github/workflows/vibe-project-release.yml; then
    echo 'commits API peels annotated tags and cannot prove tag identity' >&2
    exit 1
fi
echo 'project release guards passed'
