#!/bin/bash

set -euo pipefail

REPOSITORY_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPOSITORY_ROOT"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

find_source_files() {
    find . \
        \( -path './.git' -o -path './.beads' -o -path './dist' -o -path './target' \) -prune \
        -o "$@" -print0
}

if find_source_files -type f \
    \( -iname '*.gguf' -o -iname '*.onnx' -o -iname '*.safetensors' \) \
    | /usr/bin/grep -q .; then
    fail 'model weights must not be committed to the repository'
fi

find_source_files -type f \
    \( \
        -iname '*.wav' -o -iname '*.aac' -o -iname '*.aif' -o -iname '*.aiff' \
        -o -iname '*.caf' -o -iname '*.m4a' -o -iname '*.mp3' -o -iname '*.mp4' \
        -o -iname '*.m4v' -o -iname '*.mov' -o -iname '*.flac' -o -iname '*.ogg' \
        -o -iname '*.opus' -o -iname '*.webm' \
    \) \
    | while IFS= read -r -d '' fixture; do
        case "$fixture" in
            ./tests/fixtures/*) ;;
            *) fail "media files are allowed only under tests/fixtures: $fixture" ;;
        esac

        metadata="${fixture}.license.toml"
        [ -f "$metadata" ] || fail "fixture is missing provenance metadata: $metadata"
        /usr/bin/grep -Eq '^redistributable[[:space:]]*=[[:space:]]*true[[:space:]]*$' "$metadata" \
            || fail "fixture is not marked redistributable: $metadata"
        /usr/bin/grep -Eq '^contains_private_data[[:space:]]*=[[:space:]]*false[[:space:]]*$' "$metadata" \
            || fail "fixture privacy review is missing: $metadata"
        /usr/bin/grep -Eq '^source[[:space:]]*=[[:space:]]*".+"[[:space:]]*$' "$metadata" \
            || fail "fixture source is missing: $metadata"
        /usr/bin/grep -Eq '^license[[:space:]]*=[[:space:]]*".+"[[:space:]]*$' "$metadata" \
            || fail "fixture license is missing: $metadata"
        /usr/bin/grep -Eq '^sha256[[:space:]]*=[[:space:]]*"[a-fA-F0-9]{64}"[[:space:]]*$' "$metadata" \
            || fail "fixture digest is missing: $metadata"
        /usr/bin/grep -Eq '^synthetic[[:space:]]*=[[:space:]]*(true|false)[[:space:]]*$' "$metadata" \
            || fail "fixture synthetic status is missing: $metadata"
    done

printf 'repository asset policy passed\n'
