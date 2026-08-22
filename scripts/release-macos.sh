#!/bin/bash

set -euo pipefail
umask 077

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
BINARY="$REPOSITORY_ROOT/target/aarch64-apple-darwin/release/sosus"
ENTITLEMENTS="$REPOSITORY_ROOT/packaging/macos/sosus.entitlements"
DIST_DIRECTORY="$REPOSITORY_ROOT/dist"
RELEASE_VERSION=""
RELEASE_DATE=""
ARCHIVE=""
CHECKSUM=""
SIGNING_IDENTITY=""
TEMPORARY_FILES=("")

cleanup() {
    local path
    for path in "${TEMPORARY_FILES[@]}"; do
        [ -n "$path" ] || continue
        if [ -f "$path" ] && [ ! -L "$path" ]; then
            unlink "$path"
        fi
    done
}

trap cleanup EXIT

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

make_temporary_file() {
    local name=$1
    local prefix=$2
    local path
    path=$(mktemp "${TMPDIR:-/tmp}/${prefix}.XXXXXX")
    TEMPORARY_FILES+=("$path")
    printf -v "$name" '%s' "$path"
}

assert_regular_binary() {
    [ -f "$BINARY" ] || fail "release binary not found; build it first"
    [ ! -L "$BINARY" ] || fail "release binary must not be a symbolic link"
    [ -x "$BINARY" ] || fail "release artifact is not executable"
}

release_version() {
    RELEASE_VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPOSITORY_ROOT/Cargo.toml")
    [ -n "$RELEASE_VERSION" ] || fail "could not read the package version from Cargo.toml"
    if ! [[ "$RELEASE_VERSION" =~ ^[0-9]{4}\.[0-9]{1,2}\.[0-9]{1,2}(-beta\.[1-9][0-9]*|-r[2-9][0-9]*)?$ ]]; then
        fail "release version must use YYYY.M.D, YYYY.M.D-beta.N, or YYYY.M.D-rN"
    fi
    RELEASE_DATE=${RELEASE_VERSION%%-*}
    RELEASE_DATE=${RELEASE_DATE//./-}
    ARCHIVE="$DIST_DIRECTORY/sosus-${RELEASE_DATE}${RELEASE_VERSION#${RELEASE_VERSION%%-*}}-macos-arm64.zip"
    CHECKSUM="$ARCHIVE.sha256"
}

assert_release_tag() {
    release_version
    [ -z "$(git status --porcelain)" ] || fail "release requires a clean working tree"
    git rev-parse --verify --quiet "refs/tags/v$RELEASE_VERSION" >/dev/null \
        || fail "release requires the v$RELEASE_VERSION tag"
    [ "$(git rev-list -n 1 "v$RELEASE_VERSION")" = "$(git rev-parse HEAD)" ] \
        || fail "v$RELEASE_VERSION must point at HEAD"
}

plist_value() {
    /usr/bin/plutil -extract "$2" raw -o - "$1"
}

assert_plist_value() {
    local plist=$1
    local key=$2
    local expected=$3
    local actual
    actual=$(plist_value "$plist" "$key") || fail "embedded Info.plist is missing $key"
    [ "$actual" = "$expected" ] || fail "embedded Info.plist has an invalid $key"
}

verify_info_plist() {
    local extracted
    local offset
    local section
    local size
    make_temporary_file extracted sosus-info-plist
    section=$(
        /usr/bin/otool -l "$BINARY" | /usr/bin/awk '
            $1 == "sectname" && $2 == "__info_plist" { found = 1; next }
            found && $1 == "size" { size = $2; next }
            found && $1 == "offset" { print $2, size; exit }
        '
    )
    read -r offset size <<<"$section"
    [ -n "${offset:-}" ] && [ -n "${size:-}" ] \
        || fail "release binary has no __TEXT,__info_plist section"
    /bin/dd \
        if="$BINARY" \
        of="$extracted" \
        bs=1 \
        skip="$offset" \
        count="$((size))" \
        2>/dev/null

    if ! /usr/bin/plutil -lint "$extracted" >/dev/null; then
        fail "release binary does not contain a valid __TEXT,__info_plist section"
    fi
    assert_plist_value "$extracted" CFBundleIdentifier dev.sosus.cli
    assert_plist_value "$extracted" CFBundleName sosus
    release_version
    assert_plist_value "$extracted" CFBundleShortVersionString "${RELEASE_VERSION%%-*}"
    assert_plist_value "$extracted" CFBundleVersion "${RELEASE_VERSION%%-*}"
    assert_plist_value "$extracted" CFBundleGetInfoString "sosus $RELEASE_VERSION"
    assert_plist_value \
        "$extracted" \
        NSAudioCaptureUsageDescription \
        "sosus records system audio so it can transcribe your meetings on this Mac."
    assert_plist_value \
        "$extracted" \
        NSMicrophoneUsageDescription \
        "sosus records your microphone so your own speech appears in the transcript."
}

resolve_identity() {
    local identities
    local count
    if [ -n "${SOSUS_SIGNING_IDENTITY:-}" ]; then
        SIGNING_IDENTITY=$SOSUS_SIGNING_IDENTITY
        return
    fi

    identities=$(
        /usr/bin/security find-identity -v -p codesigning 2>/dev/null \
            | /usr/bin/sed -n 's/^[[:space:]]*[0-9][0-9]*) \([A-F0-9]*\) "Developer ID Application:.*$/\1/p'
    )
    count=$(printf '%s\n' "$identities" | /usr/bin/sed '/^$/d' | /usr/bin/wc -l | /usr/bin/tr -d ' ')
    [ "$count" -eq 1 ] \
        || fail "set SOSUS_SIGNING_IDENTITY or install exactly one Developer ID Application identity"
    SIGNING_IDENTITY=$identities
}

verify_identity_available() {
    local available
    resolve_identity
    available=$(/usr/bin/security find-identity -v -p codesigning 2>/dev/null)
    if ! /usr/bin/grep -F -- "$SIGNING_IDENTITY" >/dev/null <<<"$available"; then
        fail "the requested Developer ID Application identity is not available in the keychain"
    fi
    if ! /usr/bin/grep -F -- "$SIGNING_IDENTITY" <<<"$available" \
        | /usr/bin/grep -Fq 'Developer ID Application:'; then
        fail "signing identity must be a Developer ID Application certificate"
    fi
}

preflight() {
    assert_release_tag
    verify_identity_available
    [ -n "${SOSUS_NOTARY_PROFILE:-}" ] \
        || fail "set SOSUS_NOTARY_PROFILE to a notarytool keychain profile"
    printf 'macOS release credentials are configured\n'
}

build_release() {
    cd "$REPOSITORY_ROOT"
    mise exec -- cargo build --locked --release --target aarch64-apple-darwin
    assert_regular_binary
    verify_info_plist
    /usr/bin/file "$BINARY" | /usr/bin/grep -Fq 'Mach-O 64-bit executable arm64' \
        || fail "release artifact is not an arm64 Mach-O executable"
}

sign_release() {
    assert_regular_binary
    verify_identity_available
    /usr/bin/codesign \
        --force \
        --identifier dev.sosus.cli \
        --options runtime \
        --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGNING_IDENTITY" \
        "$BINARY"
}

verify_signature() {
    local signature_details
    local extracted
    assert_regular_binary
    verify_info_plist
    /usr/bin/codesign --verify --strict --verbose=2 "$BINARY"
    signature_details=$(/usr/bin/codesign --display --verbose=4 "$BINARY" 2>&1)
    /usr/bin/grep -Fq 'Identifier=dev.sosus.cli' <<<"$signature_details" \
        || fail "signed binary has an invalid identifier"
    /usr/bin/grep -Fq 'Authority=Developer ID Application:' <<<"$signature_details" \
        || fail "binary is not signed with a Developer ID Application identity"
    /usr/bin/grep -Eq '^TeamIdentifier=.+$' <<<"$signature_details" \
        || fail "signed binary has no Team ID"
    /usr/bin/grep -Eq '^CodeDirectory .*flags=.*runtime' <<<"$signature_details" \
        || fail "binary is not signed with the hardened runtime"

    make_temporary_file extracted sosus-entitlements
    /usr/bin/codesign --display --entitlements :- "$BINARY" >"$extracted" 2>/dev/null
    if [ "$(/usr/libexec/PlistBuddy -c 'Print :com.apple.security.device.audio-input' "$extracted")" != "true" ]; then
        fail "signed binary is missing the audio-input entitlement"
    fi
}

verify_notarization() {
    verify_signature
    /usr/bin/codesign \
        -vvvv \
        -R='notarized' \
        --check-notarization \
        "$BINARY"
}

notarize_release() {
    local result
    [ -n "${SOSUS_NOTARY_PROFILE:-}" ] \
        || fail "set SOSUS_NOTARY_PROFILE to a notarytool keychain profile"
    verify_signature
    release_version
    [ ! -L "$DIST_DIRECTORY" ] || fail "dist directory must not be a symbolic link"
    mkdir -p "$DIST_DIRECTORY"
    chmod 700 "$DIST_DIRECTORY"
    if [ -e "$ARCHIVE" ]; then
        [ -f "$ARCHIVE" ] && [ ! -L "$ARCHIVE" ] \
            || fail "release archive path is not a regular file"
        unlink "$ARCHIVE"
    fi
    if [ -e "$CHECKSUM" ]; then
        [ -f "$CHECKSUM" ] && [ ! -L "$CHECKSUM" ] \
            || fail "release checksum path is not a regular file"
        unlink "$CHECKSUM"
    fi
    /usr/bin/ditto -c -k --keepParent "$BINARY" "$ARCHIVE"
    chmod 600 "$ARCHIVE"
    /usr/bin/shasum -a 256 "$ARCHIVE" >"$CHECKSUM"
    chmod 600 "$CHECKSUM"
    make_temporary_file result sosus-notary-result
    if ! /usr/bin/xcrun notarytool submit \
        "$ARCHIVE" \
        --keychain-profile "$SOSUS_NOTARY_PROFILE" \
        --wait \
        --output-format json >"$result" 2>/dev/null; then
        fail "notarization submission failed; inspect notarytool history using the keychain profile"
    fi
    if [ "$(plist_value "$result" status)" != "Accepted" ]; then
        fail "Apple did not accept the notarization submission"
    fi
    verify_notarization
}

usage() {
    printf '%s\n' \
        "Usage: scripts/release-macos.sh <preflight|build|sign|verify|verify-notarization|notarize|release>" \
        "" \
        "Environment references (never raw credentials):" \
        "  SOSUS_SIGNING_IDENTITY  Developer ID Application identity; auto-detected when unique" \
        "  SOSUS_NOTARY_PROFILE    notarytool keychain profile" \
        "" \
        "Store a profile once with: xcrun notarytool store-credentials <profile>"
}

require_command mise
require_command /bin/dd
require_command /usr/bin/awk
require_command /usr/bin/codesign
require_command /usr/bin/ditto
require_command /usr/bin/file
require_command git
require_command /usr/bin/otool
require_command /usr/bin/plutil
require_command /usr/bin/security
require_command /usr/bin/shasum
require_command /usr/bin/xcrun
require_command /usr/libexec/PlistBuddy

case "${1:-}" in
    preflight)
        preflight
        ;;
    build)
        build_release
        ;;
    sign)
        sign_release
        verify_signature
        ;;
    verify)
        verify_signature
        ;;
    verify-notarization)
        verify_notarization
        ;;
    notarize)
        notarize_release
        ;;
    release)
        preflight
        build_release
        sign_release
        notarize_release
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
