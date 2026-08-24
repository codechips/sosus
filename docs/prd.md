# sosus — Product Requirements Document

## Product

sosus is a local-first macOS application for recording meetings, transcribing them, and optionally assigning speakers. Audio and transcripts are kept in a portable filesystem archive.

v1 is deliberately narrow. It does not summarize, search, chat, embed, index, use a database, or run an LLM. Users who want analysis can take the exported transcript to the tool of their choice.

## v1 scope

- Record system audio and an optional microphone into a durable meeting folder.
- Show unobtrusive source input meters while recording.
- Transcribe saved recordings locally with Parakeet or Whisper.
- Support automatic language detection and explicit language selection.
- Support verified built-in Whisper models, curated regional models, and local custom GGML/GGUF Whisper models.
- Optionally diarize speakers.
- Export readable `transcript.md`; optionally also write `transcript.json`.
- Browse, read, open, retranscribe, and delete meeting folders from a minimal TUI.
- Resume interrupted post-recording processing.

## Explicit non-goals

- Summaries, titles, action items, and local-LLM interpretation.
- Archive search, embeddings, semantic indexes, SQLite, and chat.
- Cloud services and remote inference.
- Per-process capture, multi-user collaboration, calendars, and scheduling.
- Mandatory Homebrew distribution.

## User flow

1. Start the TUI and press `r` to record. The UI shows recording state, elapsed time, and system/microphone levels.
2. Stop recording. sosus persists audio first, then transcribes and optionally diarizes in the background.
3. Select a meeting to read its transcript. Use `t` to transcribe or retranscribe, `o` to open its folder, and `d` to move it to Trash.
4. Press F2 to change common settings. Changes apply to the next operation.

## Filesystem contract

The archive is the source of truth. A meeting folder is collision-safe and private:

```
<recordings>/<YYYY-MM-DD_HHMM[_N]>/
  recording.wav (or recording.m4a when optional archive compaction is enabled)
  transcript.md
  transcript.json        # only when JSON export is enabled
  .pipeline-state.json   # resumability metadata
```

Incomplete folders are ignored. Audio and exports use sibling partial files then atomic rename. No database is created.

## Transcription and models

Parakeet is the fast default and supplies native word timings. Whisper is the multilingual fallback and supports explicit model selection. Built-in models are defined in an immutable manifest with source, revision, byte size, SHA-256 digest, licence, and allowed download hosts.

The picker shows source, size, and installed state. A missing selected model downloads only when transcription begins, is verified before use, and is reused afterwards. Custom models are selected locally through the macOS file picker and must be compatible Whisper GGML/GGUF files.

Language selection uses human-readable names plus ISO codes. Auto-detection remains the default.

## Diarization

Diarization is optional. It defaults to an expected count of two speakers and assigns deterministic labels by first appearance (`Speaker 1`, `Speaker 2`, and so on). The expected count can be changed to `Auto` or another exact value in settings and with `sosus transcribe --speakers auto|N`. Whisper diarization operates on segment timestamps; Parakeet may use native word timings where available.

## TUI and settings

The normal view has two columns: meetings and transcript. A compact lower recording pane appears only while recording. The header identifies the app and the status bar communicates the current actionable state.

Dialogs are padded, centred, and keyboard-first. Language and model selection use filterable pickers. Progress reports current work only; completed steps and model-download byte counters do not occupy the interface.

F2 exposes microphone on/off, system and microphone gain, language, diarization, engine, model, and JSON export. Config is TOML. Saves preserve comments and unrelated keys, are private and atomic, and refuse to overwrite a file changed while the dialog was open.

## Privacy and reliability

All recordings, transcripts, models, logs, and configuration remain local under user-controlled paths. Logs never contain transcript text. The app records through a bounded non-blocking capture path, maintains a wall-clock audio timeline, reports capture failures clearly, and does not leave empty meeting folders after failed startup.

## Quality gates and roadmap

1. Validate English and Swedish transcription with Parakeet, Whisper Base, and KB-Whisper Base, plus diarization on a real multi-speaker recording.
2. Complete recording hardening: long recordings, permissions, dropouts, recovery, and durability.
3. Complete release readiness: help/documentation, privacy and reliability audit, signed/notarized package, and fresh-Mac validation.
