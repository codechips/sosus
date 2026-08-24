#!/bin/bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
CHANGELOG_FILE=${SOSUS_CHANGELOG_FILE:-"$REPOSITORY_ROOT/CHANGELOG.md"}
REQUIRE_UNRELEASED=false

if [ "${1:-}" = "--require-unreleased" ]; then
    REQUIRE_UNRELEASED=true
elif [ "$#" -ne 0 ]; then
    printf 'usage: %s [--require-unreleased]\n' "$0" >&2
    exit 2
fi

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

[ -f "$CHANGELOG_FILE" ] || fail "CHANGELOG.md is missing"
[ "$(head -n 1 "$CHANGELOG_FILE")" = '# Changelog' ] || fail "CHANGELOG.md must start with # Changelog"

unreleased_count=$(grep -c '^## Unreleased$' "$CHANGELOG_FILE" || true)
[ "$unreleased_count" -eq 1 ] || fail "CHANGELOG.md must contain exactly one ## Unreleased section"

if ! awk '
    /^## Unreleased$/ { in_unreleased = 1; next }
    in_unreleased && /^## / { exit }
    in_unreleased && /^[-*] / { found = 1 }
    END { exit(found ? 0 : 1) }
' "$CHANGELOG_FILE"; then
    if [ "$REQUIRE_UNRELEASED" = true ]; then
        fail "## Unreleased needs at least one bullet before a release"
    fi
fi

if grep -nE '^## [0-9]{4}\.[0-9]{1,2}\.[0-9]{1,2}(-beta\.[1-9][0-9]*|-r[2-9][0-9]*)? — [0-9]{4}-[0-9]{2}-[0-9]{2}$' "$CHANGELOG_FILE" >/dev/null; then
    :
else
    fail "CHANGELOG.md needs at least one dated calendar-version release heading"
fi
