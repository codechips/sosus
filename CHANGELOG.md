# Changelog

## Unreleased

### Fixed

- Preserve and recover a completed transcript when optional diarization is interrupted or disabled.
- Make saving a selected transcription model explicit in Settings.
- Process long Whisper recordings in context-sized chunks and fail instead of exporting an empty result.
- Update Whisper.cpp to v1.8.5 to avoid the macOS Metal initialization crash.
- Explain safe transcription failure causes in the TUI instead of only reporting a pipeline exit.

### Added

- Show the transcription backend and model at the top of new transcript files.
- Show the configured backend and model before re-transcribing a recording.
- Identify the selected model in the live transcription status.

## 2026.8.24-beta.5 — 2026-08-24

### Added

- Vocabulary corrections, editable with `sosus vocabulary`.
- An activity spinner in supported terminal window and tab titles.

### Fixed

- Long Parakeet recordings are processed in safe chunks instead of exceeding the model limit.
- Imported media is copied into its meeting folder so the archive remains resumable.
- Processing durations use human-readable minutes and hours.
