# Releases

sosus uses calendar versioning. A version says when a build was released, not an inferred claim about the compatibility impact of every change.

## Version format

Stable releases use `YYYY.M.D`, for example `2026.8.22`.

Pre-release builds add a label: `YYYY.M.D-beta.N`, for example `2026.8.22-beta.1`. If a corrected stable build must be made on the same day, use `YYYY.M.D-rN`, for example `2026.8.22-r2`.

The equivalent Git tag is prefixed with `v`: `v2026.8.22`. Release archives carry the same calendar version, for example `sosus-2026-8-22-macos-arm64.zip`.

## Release rules

- A release is built only from a clean, tagged commit on `main`.
- The tag, CLI version, embedded macOS metadata, archive name, and checksum must identify the same release.
- A signed/notarized beta uses the same release path as a stable build; only its version label differs.
- A release is immutable. Changed code always receives a new date version or same-day revision; an existing artifact is never replaced.
- Models, recordings, transcripts, and local configuration are not part of release artifacts.

## Current convention

The first cross-device test build uses the release date and a beta suffix. If accepted, cut the stable version from the approved source, changing only release metadata; do not include other code changes or replace the beta artifact.
