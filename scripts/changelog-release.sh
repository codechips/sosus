#!/bin/bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
CHANGELOG_FILE=${SOSUS_CHANGELOG_FILE:-"$REPOSITORY_ROOT/CHANGELOG.md"}
VERSION=$(
    /usr/bin/awk '
        $0 == "[package]" { in_package = 1; next }
        /^\[/ { in_package = 0 }
        in_package && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
    ' "$REPOSITORY_ROOT/Cargo.toml"
)

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

[ -n "$VERSION" ] || fail "could not read the package version from Cargo.toml"
if ! [[ "$VERSION" =~ ^([0-9]{4})\.([0-9]{1,2})\.([0-9]{1,2})(-beta\.[1-9][0-9]*|-r[2-9][0-9]*)?$ ]]; then
    fail "version must use YYYY.M.D, YYYY.M.D-beta.N, or YYYY.M.D-rN"
fi

"$REPOSITORY_ROOT/scripts/changelog-check.sh" --require-unreleased

year=${BASH_REMATCH[1]}
month=${BASH_REMATCH[2]}
day=${BASH_REMATCH[3]}
printf -v date '%04d-%02d-%02d' "$year" "$month" "$day"
heading="## $VERSION — $date"
if grep -Fqx "$heading" "$CHANGELOG_FILE"; then
    fail "CHANGELOG.md already contains $heading"
fi

temporary=$(mktemp "${CHANGELOG_FILE}.XXXXXX")
trap 'rm -f "$temporary"' EXIT

/usr/bin/awk -v heading="$heading" '
    /^## Unreleased$/ {
        print
        print ""
        in_unreleased = 1
        next
    }
    in_unreleased && /^## / {
        print heading
        print ""
        printf "%s", unreleased
        if (unreleased != "" && unreleased !~ /\n$/) {
            print ""
        }
        print
        emitted = 1
        in_unreleased = 0
        next
    }
    in_unreleased {
        unreleased = unreleased $0 "\n"
        next
    }
    { print }
    END {
        if (in_unreleased && !emitted) {
            print heading
            print ""
            printf "%s", unreleased
        }
    }
' "$CHANGELOG_FILE" >"$temporary"

mv "$temporary" "$CHANGELOG_FILE"
trap - EXIT
printf 'promoted Unreleased to %s\n' "$heading"
