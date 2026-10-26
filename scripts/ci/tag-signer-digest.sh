#!/bin/sh
# Resolve the OIDC build signer digest for an SDK release tag, and check that
# the tag's target commit is on main.
#
# Usage: tag-signer-digest.sh <tag> <main-ref> <remote> [expected-commit]
#
# <tag> is the tag name (GITHUB_REF_NAME), such as v1.0.0-rc.9.
# <main-ref> is a local ref that main was fetched into, such as origin/main.
# <remote> is the git remote (or URL) that advertises refs/tags/<tag>.
# <expected-commit>, when set, must be the peeled commit. On a tag push that
# is GITHUB_SHA. It is never the signer digest.
#
# Prints:
#   signer_digest=<object SHA of refs/tags/<tag>>
#   tagged_commit=<peeled commit SHA>
#
# signer_digest is the value GitHub OIDC writes into build_signer_digest when
# a caller pins @<tag>: the annotated tag object, or the commit for a
# lightweight tag. git ls-remote reads that ref from <remote>. A local tag
# created by actions/checkout is ignored, because a shallow checkout can store
# an annotated tag as a lightweight tag pointing at the commit.
#
# tagged_commit is the only SHA passed to merge-base. An annotated tag object
# is not an ancestor of main even when the commit it points at is.
set -eu
fail() { echo "::error::$*" >&2; exit 1; }
sha_ok() { printf '%s\n' "$1" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$'; }

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    fail 'usage: tag-signer-digest.sh <tag> <main-ref> <remote> [expected-commit]'
fi
tag=$1
main_ref=$2
remote=$3
expected=${4-}
carriage=$(printf '\r')
newline='
'
tab=$(printf '\t')
case $tag in
    *"$newline"*|*"$carriage"*) fail 'invalid release tag name' ;;
esac
case $main_ref in
    *"$newline"*|*"$carriage"*) fail 'invalid main ref' ;;
esac
case $remote in
    ''|-*|*' '*|*"$tab"*|*"$newline"*|*"$carriage"*) fail 'invalid remote' ;;
esac
printf '%s\n' "$tag" | LC_ALL=C grep -Eq '^v[0-9A-Za-z.+-]+$' || fail 'invalid release tag name'
printf '%s\n' "$main_ref" | LC_ALL=C grep -Eq '^[A-Za-z0-9._/-]+$' || fail 'invalid main ref'
if [ -n "$expected" ]; then
    sha_ok "$expected" || fail 'expected commit must be 40 lowercase hex characters'
fi

ref="refs/tags/$tag"
# Ask for the ref and its peeled form in one listing. Annotated tags advertise
# both lines; lightweight tags advertise only the commit. The peel is fully
# dereferenced, so a nested tag's second line is still the commit.
listing=$(git ls-remote "$remote" "$ref" "${ref}^{}") || fail "tag lookup failed for $ref"

signer=
commit=
rest=$listing
while [ -n "$rest" ]; do
    case $rest in
        *"$newline"*)
            line=${rest%%"$newline"*}
            rest=${rest#*"$newline"}
            ;;
        *)
            line=$rest
            rest=
            ;;
    esac
    [ -n "$line" ] || continue
    case $line in
        *"$tab"*) ;;
        *) fail 'tag listing is invalid' ;;
    esac
    sha=${line%%"$tab"*}
    name=${line#*"$tab"}
    sha_ok "$sha" || fail 'tag ref SHA is invalid'
    case $name in
        "$ref")
            [ -z "$signer" ] || fail 'duplicate tag ref'
            signer=$sha
            ;;
        "${ref}^{}")
            [ -z "$commit" ] || fail 'duplicate peeled tag'
            commit=$sha
            ;;
        *) fail "unexpected tag listing: $name" ;;
    esac
done

[ -n "$signer" ] || fail "tag ref $ref was not found"
# No peel line means the ref already points at the commit (lightweight tag).
if [ -z "$commit" ]; then
    commit=$signer
fi
sha_ok "$commit" || fail 'peeled commit is invalid'
if ! git cat-file -e "$commit" 2>/dev/null; then
    fail "peeled commit $commit is not in this clone"
fi
obj_type=$(git cat-file -t "$commit")
[ "$obj_type" = commit ] || fail "peeled tag target $commit is $obj_type, not a commit"
if [ -n "$expected" ] && [ "$commit" != "$expected" ]; then
    fail "Tag $tag peels to $commit, not $expected"
fi
# Ancestry uses the peeled commit only. The tag object is not a commit.
if ! git merge-base --is-ancestor "$commit" "$main_ref"; then
    fail "Tag $tag commit $commit is not on $main_ref"
fi
printf 'signer_digest=%s\ntagged_commit=%s\n' "$signer" "$commit"
