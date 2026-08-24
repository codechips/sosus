#!/bin/bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPOSITORY_ROOT"

previous_tag=$(git describe --tags --abbrev=0 2>/dev/null || true)
if [ -n "$previous_tag" ]; then
    range="$previous_tag..HEAD"
    printf 'Candidate changes since %s:\n\n' "$previous_tag"
else
    range='HEAD'
    printf 'Candidate changes:\n\n'
fi

git log --no-merges --format='- %s (%h)' "$range"
printf '\nReview these candidates and write only user-visible changes under ## Unreleased in CHANGELOG.md.\n'
