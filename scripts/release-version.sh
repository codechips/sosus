#!/bin/bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
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
if ! [[ "$VERSION" =~ ^[0-9]{4}\.[0-9]{1,2}\.[0-9]{1,2}(-beta\.[1-9][0-9]*)?$ ]]; then
    fail "version must use YYYY.M.D or YYYY.M.D-beta.N"
fi

DATE=${VERSION%%-*}
DATE=${DATE//./-}
SUFFIX=${VERSION#${VERSION%%-*}}
TAG="v$VERSION"

printf '%s\n' \
    "version: $VERSION" \
    "tag: $TAG" \
    "archive: sosus-${DATE}${SUFFIX}-macos-arm64.zip"

if git -C "$REPOSITORY_ROOT" rev-parse --verify --quiet "refs/tags/$TAG" >/dev/null; then
    TAG_COMMIT=$(git -C "$REPOSITORY_ROOT" rev-parse "$TAG^{commit}")
    HEAD_COMMIT=$(git -C "$REPOSITORY_ROOT" rev-parse HEAD)
    if [ "$TAG_COMMIT" = "$HEAD_COMMIT" ]; then
        printf '%s\n' 'tag status: points at HEAD'
    else
        printf '%s\n' "tag status: exists at ${TAG_COMMIT:0:12}, not HEAD"
    fi
else
    printf '%s\n' 'tag status: not created'
fi
