#!/bin/sh
# Annotated tags: main ancestry uses the peeled commit, and the signer digest
# is the tag ref object. A local tag that actions/checkout may have peeled
# must not change the digest.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
script=$root/scripts/ci/tag-signer-digest.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
git init -q --bare "$tmp/origin.git"
git init -q "$tmp/work"
cd "$tmp/work"
git config user.email test@example.com
git config user.name test
echo base > file
git add file
git commit -qm 'on main'
git branch -M main
git remote add origin "$tmp/origin.git"
git push -q origin main
on_main=$(git rev-parse HEAD)
git tag -a v1.0.0-rc.8 -m 'annotated release'
tag_object=$(git rev-parse refs/tags/v1.0.0-rc.8)
peeled=$(git rev-parse 'refs/tags/v1.0.0-rc.8^{commit}')
[ "$tag_object" != "$peeled" ]
[ "$peeled" = "$on_main" ]
git tag v1.0.0 "$on_main"
git checkout -q -b side
echo side > file
git add file
git commit -qm 'not on main'
off_main=$(git rev-parse HEAD)
git tag -a v9.9.9 -m 'annotated off main'
off_tag=$(git rev-parse refs/tags/v9.9.9)
[ "$off_tag" != "$off_main" ]
git push -q origin refs/tags/v1.0.0-rc.8 refs/tags/v1.0.0 refs/tags/v9.9.9
git checkout -q main
# Simulate actions/checkout storing the annotated tag as the peeled commit.
git tag -d v1.0.0-rc.8 >/dev/null
git tag v1.0.0-rc.8 "$peeled"
[ "$(git rev-parse refs/tags/v1.0.0-rc.8)" = "$peeled" ]
[ "$(git cat-file -t refs/tags/v1.0.0-rc.8)" = commit ]
# merge-base may peel a tag object, so it cannot separate these two SHAs.
# The script resolves each one itself.

run() {
    "$script" "$@"
}
output=$(run v1.0.0-rc.8 origin/main origin "$peeled")
printf '%s\n' "$output" | grep -Fx "signer_digest=$tag_object"
printf '%s\n' "$output" | grep -Fx "tagged_commit=$peeled"
# Local peeled tag must not win.
[ "$(git rev-parse refs/tags/v1.0.0-rc.8)" != "$tag_object" ]

output=$(run v1.0.0 origin/main origin "$on_main")
printf '%s\n' "$output" | grep -Fx "signer_digest=$on_main"
printf '%s\n' "$output" | grep -Fx "tagged_commit=$on_main"

if output=$(run v9.9.9 origin/main origin "$off_main" 2>&1); then
    echo 'off-main tag was accepted' >&2
    exit 1
fi
case $output in
    *"::error::"*"commit $off_main is not on origin/main"*) ;;
    *) printf 'ancestry did not use the peeled commit\n%s\n' "$output" >&2; exit 1 ;;
esac
case $output in
    *"$off_tag"*) echo 'ancestry error used the tag object' >&2; exit 1 ;;
esac

if output=$(run v1.0.0-rc.8 origin/main origin "$tag_object" 2>&1); then
    echo 'tag object was accepted as the peeled commit' >&2
    exit 1
fi
case $output in
    *"::error::"*"peels to $peeled, not $tag_object"*) ;;
    *) printf 'expected-commit check failed open\n%s\n' "$output" >&2; exit 1 ;;
esac

if output=$(run v0.0.0 origin/main origin 2>&1); then
    echo 'missing tag was accepted' >&2
    exit 1
fi
case $output in
    *'::error::'*'was not found'*) ;;
    *) printf 'missing tag error changed\n%s\n' "$output" >&2; exit 1 ;;
esac

if grep -n 'VIBE_SDK_COMMIT:.*github\.sha' "$root/.github/workflows/release.yml"; then
    echo 'release workflow still compiles github.sha as the signer digest' >&2
    exit 1
fi
grep -q 'needs.verify-tag.outputs.signer_digest' "$root/.github/workflows/release.yml"
grep -q 'tag-signer-digest.sh' "$root/.github/workflows/release.yml"
grep -F -- "--is-ancestor \"\$commit\" \"\$main_ref\"" "$script" >/dev/null
echo 'tag signer digest tests passed'
