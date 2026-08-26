# Changelog

## Unreleased

### Fixed

- Format saved recording durations in the TUI as minutes or hours instead of raw seconds.
- Equalize voice levels before diarization. A voice recorded well below another (a speaker sitting away from the microphone, or a remote participant mixed low) no longer collapses the meeting into a single speaker.
- Record release checksums with a bare archive name so `shasum -c` verifies a downloaded artifact instead of failing on the build machine's absolute path.
- Render the entire transcript in the reader pane instead of silently stopping after 100 segments.
- Report transcription progress in the pipeline status instead of a static line for the whole run.

### Changed

- Drop the `YYYY.M.D-rN` same-day revision version. A corrected build takes the next calendar date, because a hyphen suffix sorts below the plain version under SemVer precedence.
- Document installing the signed binary from a GitHub release, including the prerelease-aware way to resolve the newest tag.

## 2026.8.26-beta.3 — 2026-08-26

### Added

- Seek audio preview playback by clicking a transcript segment or the preview progress strip.

## 2026.8.26-beta.2 — 2026-08-26

### Added

- Preview recordings in the TUI with playback, pause, stopping, skipping, and transcript-position seeking.

## 2026.8.26-beta.1 — 2026-08-26

### Fixed

- Preserve and recover a completed transcript when optional diarization is interrupted or disabled.
- Make saving a selected transcription model explicit in Settings.
- Process long Whisper recordings in context-sized chunks and fail instead of exporting an empty result.
- Update Whisper.cpp to v1.8.5 to avoid the macOS Metal initialization crash.
- Explain safe transcription failure causes in the TUI instead of only reporting a pipeline exit.
- Allow Whisper automatic language detection to continue into transcription.

### Added

- Show the transcription backend and model at the top of new transcript files.
- Show the configured backend and model before re-transcribing a recording.
- Identify the selected model in the live transcription status.
- Choose a transient transcription language with `l` during recording or before re-transcribing a meeting.

## 2026.8.24-beta.5 — 2026-08-24

### Added

- Vocabulary corrections, editable with `sosus vocabulary`.
- An activity spinner in supported terminal window and tab titles.

### Fixed

- Long Parakeet recordings are processed in safe chunks instead of exceeding the model limit.
- Imported media is copied into its meeting folder so the archive remains resumable.
- Processing durations use human-readable minutes and hours.
