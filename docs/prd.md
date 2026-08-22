# sosus — Product Requirements Document

**Status:** Ready for M0; later product decisions must be resolved before their affected milestones
**Version:** 1.7 draft (2026-08-21)
**Deliverable:** A single signed macOS binary providing a first-class CLI and optional terminal UI to record, transcribe, diarize, summarize, search and chat with meetings, entirely on-device.

> **This document is the authoritative specification.** It is written for an implementing agent working in a **fresh repository with no access to any prior implementation**. Do not invent behaviour that is not stated here. Unresolved technical experiments appear in [§16.1](#161-q6-decision-ladder-pre-agreed-do-not-escalate); unresolved product decisions appear in [§16.2](#162-product-decisions-required-before-implementation) and must be answered by the owner before the affected work begins.

> **The name is settled: `sosus`.** After SOSUS, the US Navy's Cold War Sound Surveillance System: a net of seabed hydrophones whose only job was drawing sound out of the ocean and working out who made it. Verified free on crates.io (zero results), free in Homebrew core, and clear of local command and man-page collisions. GitHub holds four abandoned zero-star namesakes and no live project. Bundle identifier `dev.sosus.cli`, config at `~/.config/sosus/`, data at `~/.local/share/sosus/`, meeting folders at `~/sosus/recordings/`. **This question is closed. Do not revisit or "improve" the name.**

---

## Table of contents

1. [Product summary](#1-product-summary)
2. [Scope](#2-scope)
3. [Core user flows](#3-core-user-flows)
4. [Platform and distribution requirements](#4-platform-and-distribution-requirements)
5. [Technical stack (pinned)](#5-technical-stack-pinned)
6. [Architecture](#6-architecture)
7. [Data model](#7-data-model)
8. [Functional requirements](#8-functional-requirements)
9. [Prompt templates](#9-prompt-templates)
10. [Configuration](#10-configuration)
11. [CLI surface](#11-cli-surface)
12. [Non-functional requirements](#12-non-functional-requirements)
13. [Known traps](#13-known-traps-read-before-writing-audio-or-build-code)
14. [Milestones and exit criteria](#14-milestones-and-exit-criteria)
15. [Anti-goals](#15-anti-goals-do-not-build-these)
16. [Open questions](#16-open-questions-escalate-do-not-guess)
17. [References](#17-references)

---

## 1. Product summary

`sosus` is a local-first meeting intelligence tool for macOS, operated through a first-class CLI with an optional terminal UI. It captures system audio and microphone simultaneously, transcribes with speaker labels, produces structured notes, and lets the user ask natural-language questions across their entire meeting archive with citations back to the source audio timestamps.

Nothing leaves the machine. The only network access is downloading model weights on first run.

**Primary user:** a technical individual who is in many meetings, wants searchable notes, and will not send recordings to a cloud service.

**Why a TUI:** the optional interactive view makes recording and archive browsing fast without leaving the keyboard. It is a convenience layer, not a requirement for automation or full product access.

---

## 2. Scope

### 2.1 In scope for v1

| Capability | Summary |
|---|---|
| System audio capture | Core Audio process taps, all-system or per-process |
| Microphone capture | Simultaneous with system audio, mixed, with live mute toggle |
| Transcription | **Two selectable backends:** Whisper via whisper.cpp with Metal, and NVIDIA Parakeet TDT via sherpa-onnx |
| Custom vocabulary | User-managed term list biasing recognition of names, jargon and product terms |
| Speaker diarization | **Required in v1.** Segment-level speaker labels, no HuggingFace token |
| Summarization | Structured notes from a local GGUF model, template-driven |
| Auto-titling | Short generated title used as the meeting's display name |
| Search | Hybrid keyword and semantic retrieval over the whole archive |
| Chat | Conversational Q&A, corpus-wide by default, scopable to one meeting |
| Citations | Every claim traceable to meeting, timestamp and speaker, with verification |
| Import | Ingest existing audio and video files from disk |
| TUI | Optional four-pane interactive front end: Meetings, Transcript, Chat, Recording |
| CLI | First-class non-interactive subcommands and invocation-scoped flags for every durable capability |

### 2.2 Explicitly out of scope for v1

- **Any platform other than macOS on Apple Silicon.** No Intel, no Linux, no Windows. Do not add `cfg` branches or abstraction layers for portability. A future port is easier from clean single-platform code than from premature abstraction.
- Cloud or remote LLM backends. Local inference only.
- Real-time or streaming transcription during recording. Transcription runs after the recording stops.
- Speaker *identification* (naming speakers). v1 produces `Speaker 1`, `Speaker 2`. Persistent voice profiles are a v2 feature.
- Multi-user, sync, sharing, export to third-party services.
- A GUI, menu bar app, web interface or HTTP API.
- Editing transcripts by hand.
- Translation. Transcribe in the source language only.

---

## 3. Core user flows

### 3.1 Record a meeting

User launches `sosus`, presses `r`. Recording begins, capturing system audio plus microphone. The Recording pane shows elapsed time, separate live activity meters for system audio, microphone and the mixed signal being written, and mute state. User presses `m` to mute or unmute the microphone. User presses `r` again or `Ctrl+C` to stop. Recording auto-stops after a configurable silence period.

The pipeline then runs with visible per-stage progress: transcribe, diarize, summarize, index. On completion the meeting appears at the top of the Meetings pane with its generated title, and the Transcript pane shows the result.

### 3.2 Browse and read

Meetings pane lists meetings newest first with date, title, duration and speaker count. Selecting one loads its summary and transcript. Transcript pane supports scrolling, jump-to-timestamp, and filtering by speaker.

### 3.3 Ask across the archive

User focuses the Chat pane and types a question. Default scope is the whole archive. The tool retrieves relevant passages, answers with inline citations, and each citation is selectable to jump the Transcript pane to that moment in that meeting. Unverifiable quotes are visibly marked.

### 3.4 Ask about one meeting

With a meeting selected, the user toggles chat scope to `this meeting`. Retrieval is then restricted to that meeting's passages, and if the whole transcript fits the context window it is passed in full instead of retrieved.

### 3.5 Import existing recordings

`sosus import ~/Recordings` walks the directory, ingests every supported media file as a meeting, and runs the full pipeline on each. Used both for backfilling an archive and for processing recordings made by other tools.

---

## 4. Platform and distribution requirements

| Requirement | Value |
|---|---|
| Minimum macOS | 14.4 (Core Audio process taps are usable and in a stable TCC category from here) |
| Architecture | `aarch64-apple-darwin` only |
| Build prerequisites | Rust stable (1.88+), Xcode Command Line Tools, `clang` for bindgen |
| Distribution artifact | One Mach-O binary, plus an optional notarized `.pkg` |
| Signing | **Mandatory.** Developer ID Application certificate with Team ID |
| Hardened runtime | Enabled (required for notarization) |
| Source license | MIT |
| Initial release process | Manually signed and notarized on the maintainer's Mac |

### 4.1 Signing is a functional requirement, not release polish

TCC keys permission records off the code signing identity. An unsigned or ad-hoc-signed binary compiles against the tap API and then **never receives the permission prompt**, so audio capture silently returns zeros forever. The same applies to ScreenCaptureKit since macOS Sequoia, where `CGPreflightScreenCaptureAccess()` returns false for ad-hoc signed binaries even after the user grants permission in System Settings.

Consequence: **the signing pipeline must exist before audio capture can be tested at all.** It is milestone M0 work, not M4 work.

### 4.2 Required build inputs

**Embedded `Info.plist`** via linker flag `-sectcreate __TEXT __info_plist <path>`:

```xml
<key>CFBundleIdentifier</key>            <string>dev.sosus.cli</string>
<key>CFBundleName</key>                  <string>sosus</string>
<key>NSAudioCaptureUsageDescription</key> <string>sosus records system audio so it can transcribe your meetings on this Mac.</string>
<key>NSMicrophoneUsageDescription</key>   <string>sosus records your microphone so your own speech appears in the transcript.</string>
```

Verify with `otool -s __TEXT __info_plist target/release/sosus | xxd -r -p | plutil -p -`.

**Entitlements file:**

```xml
<key>com.apple.security.device.audio-input</key> <true/>
```

Under hardened runtime this is **not optional**. Without it TCC refuses microphone access even when the usage description is present.

**Signing and notarization:**

```
codesign --force --options runtime --timestamp \
  --entitlements sosus.entitlements \
  --sign "Developer ID Application: <NAME> (<TEAMID>)" target/release/sosus
xcrun notarytool submit sosus.zip --keychain-profile <profile> --wait
```

Initial releases use this manual local process. The Developer ID private key stays in the maintainer's login keychain and notarization credentials stay in a local keychain profile. They are never committed, copied into repository files, printed in logs, or exposed to pull-request workflows. CI may build and test unsigned binaries, but those binaries must be labelled as development artifacts and must not be published as capture-ready releases. Any later automation requires a protected, manually approved release environment whose secrets are unavailable to pull requests and untrusted forks.

### 4.3 Distribution constraints

A notarization ticket **cannot be stapled to a bare Mach-O binary**. Stapling works only for `.app`, `.dmg` and `.pkg`. A notarized bare binary still passes Gatekeeper, but only when the machine is online, where the ticket is fetched and cached. Verify that online ticket with `codesign -vvvv -R="notarized" --check-notarization <binary>`. `spctl --assess` is not a valid acceptance check for a bare command-line Mach-O because it classifies the executable as non-app code; reserve `spctl` for supported containers such as `.app`, `.dmg` and `.pkg`.

Practical consequence: a browser or `curl` download of the bare binary triggers Gatekeeper once. Ship a notarized `.pkg` alongside for a clean offline first run. Do not confuse Gatekeeper acceptance with TCC identity: a Homebrew package can install successfully while still failing to receive audio-capture permission if the installed executable does not retain a suitable signing identity.

Homebrew is a release goal, but the packaging channel is constrained by signing. Official `homebrew/core` formulae for open-source command-line tools build from source, while a cask installs a pre-built upstream artifact. A source-built `sosus` executable will not automatically carry the maintainer's Developer ID signature. The initial safe path is therefore a third-party tap that installs an immutable, SHA-256-verified, signed and notarized upstream artifact. D18 decides whether the later official target is `homebrew/core` or `homebrew/cask`; neither may be promised until a fresh-Mac recording test proves that its installed executable receives both required permissions.

### 4.4 Privacy and data handling requirements

Recording a meeting captures other people's voices, which is personal data. These are hard requirements, not preferences.

- **NFR-PRIV-1** No telemetry, analytics, crash reporting or usage pings. Ever.
- **NFR-PRIV-2** The only permitted outbound network traffic is model weight downloads whose origin and redirect hosts appear in the source-controlled model manifest. User-configured HuggingFace models use the same fixed host allowlist. Any other outbound connection is a defect.
- **NFR-PRIV-3** Disable third-party library telemetry explicitly rather than relying on defaults (for example `HF_HUB_DISABLE_TELEMETRY=1` before any HuggingFace Hub call).
- **NFR-PRIV-4** Audio, transcripts, summaries and the database live only under user-controlled paths. Meeting folders default to `~/sosus/recordings/`; models, the database and logs default to `~/.local/share/sosus/`.
- **NFR-PRIV-5** A retention control must exist: `retention_days` in config, and a `cleanup` command that deletes audio and derived data older than that. Default is unlimited, but the mechanism must ship in v1.
- **NFR-PRIV-6** Deleting a meeting from the TUI must remove its transcript, summary, passages, embeddings, chats and owned artifacts, with no orphan rows. It deletes audio only when `audio_owned = 1`; a user-supplied import is dereferenced but never deleted.
- **NFR-PRIV-7** Directories created by sosus use mode `0700`; files containing meeting data, config, database state or logs use mode `0600`. Do not loosen permissions on an existing user-created output directory.
- **NFR-PRIV-8** Logs must never contain transcript text, chat questions or answers, vocabulary terms, audio samples, prompts, or generated summaries. Log stable IDs, stage names, timings, sizes and redacted error categories only. Rotate at 5 MiB, retain five files, and store them under `~/.local/share/sosus/logs/`.
- **NFR-PRIV-9** Sosus does not provide application-level encryption and does not require FileVault. It relies on macOS user-account isolation and the `0700` directory and `0600` file permissions above. FileVault remains an optional operating-system choice outside the product contract.

#### 4.4.1 Default on-disk layout

For a recording created by sosus, the audio and its portable derived artifacts live together in one stable meeting folder:

```text
~/sosus/
└── recordings/
    └── 2026-08-21_1430/
        ├── recording.wav
        ├── transcript.md
        ├── summary.md
        └── transcript.json   # only when JSON export is enabled
```

The meeting folder is created when capture starts, before a generated title exists, and is never renamed. Its name is the local start time in `YYYY-MM-DD_HHMM` form. If that name already exists, append `_2`, `_3`, and so on. Generated titles are display metadata only and never become filesystem names.

Imported meetings use the same meeting-folder pattern for transcripts and summaries, but their source audio remains at its original path and is not copied or deleted. The database records the actual audio location in `meetings.audio_path` and distinguishes owned recordings from imports with `audio_owned`.

### 4.5 Public repository requirements

The canonical repository is intended to be public. The full PRD may be published, but examples and fixtures must remain fictional or demonstrably redistributable.

- **NFR-OSS-1** Project-authored source and documentation are licensed under MIT. The repository contains `LICENSE`, and `Cargo.toml` declares `license = "MIT"` when the crate is created. Third-party dependencies, model weights and user data are not relicensed by this choice.
- **NFR-OSS-2** No signing keys, certificates, notarization credentials, account identifiers, personal recordings, real transcripts, private vocabulary, local databases, logs or machine-specific configuration may enter Git history. `.gitignore` is a backstop, not a security boundary: inspect staged changes before every publication.
- **NFR-OSS-3** Model weights are downloaded at runtime and never committed or redistributed in release archives. `THIRD_PARTY_MODELS.md` and the model manifest must identify each model's source, immutable revision, license and attribution requirements before the model can become a built-in default.
- **NFR-OSS-4** Public audio fixtures must be synthetic or have an explicit licence permitting redistribution. The representative acceptance corpus in D16 remains outside the repository; only aggregate benchmark results may be published.
- **NFR-OSS-5** Add `SECURITY.md` with a private vulnerability-reporting route before announcing the public repository. Enable repository secret scanning and push protection where the host supports them.
- **NFR-OSS-6** Release tags and source archives are immutable. Published binaries, packages, formulae or casks must pin the matching version and SHA-256; a release process must never fetch build inputs from a moving branch.

---

## 5. Technical stack (pinned)

Use these crates at these versions or later compatible releases. Do not substitute without escalating. Versions are as verified on 2026-08-21.

| Concern | Crate | Version | Notes |
|---|---|---|---|
| TUI | `ratatui` | 0.30.1 | MSRV 1.88, edition 2024 |
| Terminal backend | `crossterm` | matching ratatui | Alternate screen, raw mode |
| Chat input | `tui-textarea` | latest | Multi-line composer with editing |
| Async runtime | `tokio` | 1.x | `rt-multi-thread`, `sync`, `time`, `signal`, `macros` |
| System audio | `objc2-core-audio` | 0.3.2 | Raw Core Audio tap C API. See [§13.1](#131-core-audio-taps) |
| Microphone | `cpal` | 0.18.2 | Input stream only; Core Audio backend |
| Realtime audio queue | `rtrb` | 0.4.0 | Fixed-capacity lock-free SPSC callback boundary |
| Resampling | `rubato` | 0.15+ | Capture rate to 16 kHz for Whisper |
| Media decode | `symphonia` | 0.5+ | WAV, MP4, M4A, FLAC, MP3, OGG. Removes any ffmpeg dependency |
| WAV write | `hound` | 3.x | Archival recording file |
| Transcription (Whisper) | `whisper-rs` | 0.16.0 | Features: `metal`. `coreml` optional. See [§13.4](#134-metal-shader-compilation) |
| Transcription (Parakeet) + diarization | `sherpa-onnx` | 1.13.5 | **Official k2-fsa Rust API.** Static linking is the default. Serves both NeMo TDT ASR and `OfflineSpeakerDiarization` |
| Summarize / chat | `llama-cpp-2` | 0.1.154 | GGUF, Metal automatic |
| Embeddings | `fastembed` | 5.17.4 | Embedding model pending D1; via ONNX Runtime |
| Database | `rusqlite` | 0.32+ | Features: `bundled`, `functions`. FTS5 included in bundled SQLite |
| Vector index | `sqlite-vec` | 0.1.9 | Registered via `sqlite3_auto_extension` |
| Model download | `hf-hub` | latest | With progress callbacks |
| CLI parsing | `clap` | 4.x | `derive` feature |
| Config | `serde` + `toml` | latest | Deserialize with `#[serde(default)]` throughout |
| Format-preserving config edits | `toml_edit` | 0.25.13 | Settings modal writes known keys without discarding comments, layout or unknown keys |
| Unknown config keys | `serde_ignored` | latest | Collect and warn for ignored paths |
| JSON | `serde_json` | latest | CLI and file export |
| Paths | `etcetera` | latest | XDG-style resolution |
| Time | `time` | 0.3+ | RFC3339 timestamps and local offsets |
| Digests | `sha2` | 0.10+ | Model SHA-256 verification |
| CPU topology | `num_cpus` | 1.x | Physical-core default for ASR |
| macOS/POSIX support | `libc` | 0.2+ | File-descriptor-level native log suppression |
| Errors | `thiserror` + `anyhow` | latest | `thiserror` in library modules, `anyhow` at the binary edge |
| Logging | `tracing` + `tracing-subscriber` | latest | File-only sink. See [§13.5](#135-never-write-logs-to-the-terminal) |

`Cargo.lock` is committed and is the reproducible version authority for releases. The table above gives the approved dependency and starting-version surface; “latest” never means silently updating the lockfile during a build. FFI crates (`whisper-rs`, `sherpa-onnx`, `llama-cpp-2`, `sqlite-vec`) are upgraded only as an explicit change with their relevant integration tests rerun.

### 5.1 Stack rationale and rejected alternatives

**Diarization: `sherpa-onnx`, not `sherpa-rs` or `pyannote-rs`.** `sherpa-rs` was archived on 2026-06-06 and is deprecated in favour of the upstream official Rust API. `pyannote-rs` 0.3.4 was last updated 2025-09 and lacks a full clustering stage, so it cannot discover an unknown speaker count reliably. `sherpa-onnx` 1.13.5 shipped 2026-08-12 and exposes `OfflineSpeakerDiarization`, `OfflineSpeakerDiarizationConfig`, `OfflineSpeakerEmbeddingExtractor` and `SpeakerEmbeddingManager`, which is the complete pipeline. **It also needs no HuggingFace token**, unlike pyannote via Python, which removes a significant onboarding obstacle.

**ASR: both `whisper-rs` and `sherpa-onnx`.** These are complementary, not redundant, which is why [§15](#15-anti-goals-do-not-build-these) permits exactly two ASR implementations and no more.

- **Parakeet TDT 0.6B v3** via `sherpa-onnx` is faster and more accurate where it has coverage: 6.34% WER on the Open ASR Leaderboard English set against roughly 7.4% for Whisper large-v3, at a quarter of the size, and around an order of magnitude faster on Apple Silicon. It emits punctuation, capitalisation and **native word-level timestamps** without a second pass. Coverage is 25 European languages with automatic detection, **including Swedish, Danish and Finnish, but not Norwegian**.
- **Whisper** via `whisper-rs` covers 99 languages and is the fallback for anything Parakeet does not handle. Its Metal backend is well proven.
- Parakeet costs **no new dependency**, because `sherpa-onnx` is already required for diarization. This is the single cheapest capability addition in the whole plan.
- The two backends also differ in how they support custom vocabulary, which is a real asymmetry rather than an implementation detail. See [§8.3.3](#833-custom-vocabulary).

**Embeddings: `fastembed`, not `llama-cpp-2` embeddings.** `sherpa-onnx` already links ONNX Runtime, so `fastembed` reuses a runtime that is present regardless. The original BGE-small-en-v1.5 choice is rejected because the product transcribes multilingual meetings while that model is English-only; D1 selects the replacement. This was the right runtime call only because diarization is in scope; without it, ONNX Runtime would have been avoidable.

**Storage: SQLite, not LanceDB or Qdrant.** Single file, no server, statically linked, and FTS5 plus `sqlite-vec` covers hybrid retrieval. LanceDB is built for multimodal lakehouse workloads and is disproportionate for a single-user desktop index.

**LLM: `llama-cpp-2`, not `mistral.rs`.** Closer tracking of llama.cpp, faster on Apple Silicon, and GGUF gives access to a wide model selection. `mistral.rs` is the fallback if C++ build friction becomes intolerable.

### 5.2 Models

| Role | Model | Size | Source |
|---|---|---|---|
| ASR default | `parakeet-tdt-0.6b-v3` ONNX export | ~650 MB | sherpa-onnx pretrained models |
| ASR, Parakeet extras | `bpe.vocab` for the same model | small | **Required for hotword biasing.** See [§13.7](#137-parakeet-hotwords) |
| ASR alternative | `ggml-base.bin` (Whisper) | ~142 MB | `ggerganov/whisper.cpp` |
| ASR, Whisper options | `tiny`, `small`, `medium`, `large-v3` | up to 3 GB | same |
| Diarization segmentation | `sherpa-onnx-pyannote-segmentation-3-0` | ~6 MB | sherpa-onnx pretrained models |
| Speaker embedding | 3D-Speaker or WeSpeaker ONNX export | ~30 MB | sherpa-onnx pretrained models |
| Summarization default | `Phi-4-mini-instruct-Q4_K_M.gguf` | ~2.4 GB | `unsloth/Phi-4-mini-instruct-GGUF` |
| Text embedding | Pending D1; `MultilingualE5Small` recommended | pending | bundled by `fastembed` |

**No model weights in the repository or the binary.** Download on first use to `~/.local/share/sosus/models/`, with a progress indicator in the TUI.

- **FR-MODEL-1** Every model download must be verified against a pinned SHA-256 recorded in the source tree. A mismatch is a hard failure with a clear message, never a warning.
- **FR-MODEL-2** Partial downloads must be cleaned up on failure, never left to be mistaken for a complete file.
- **FR-MODEL-3** `sosus warmup` prefetches all models for the current config without processing audio.
- **FR-MODEL-4** A source-controlled model manifest is the single source of truth for every built-in alias. Each entry contains the immutable repository revision, every required filename, origin URL, permitted redirect hosts, SHA-256, expected byte size and license identifier. The downloader writes to a `.partial` path, verifies size and digest, `fsync`s it, then atomically renames it into place.
- **FR-MODEL-5** A custom remote GGUF reference must be immutable and self-verifying: `hf:owner/repo/file.gguf@<revision>#sha256=<digest>`. Floating revisions and remote files without a digest are config errors. A local GGUF path does not require a manifest entry and causes no network access.
- **FR-MODEL-6** On an unavailable network, a model already present with the correct digest is used normally. A missing model fails with an actionable message suggesting `sosus warmup`; do not repeatedly retry in the background.

---

## 6. Architecture

### 6.1 Threading model

The TUI event loop never blocks. All heavy work runs on dedicated threads and communicates over channels.

```
                    ┌─────────────────────────────────────┐
                    │  main: tokio runtime + ratatui loop │
                    │  tokio::select! over:               │
                    │    - crossterm event stream         │
                    │    - tick interval (100 ms)         │
                    │    - AppEvent receiver              │
                    └───────▲─────────────────────┬───────┘
                            │ AppEvent            │ Command
      ┌─────────────────────┴──────┬──────────────┴──────────────┐
      │                            │                             │
┌─────▼──────────┐   ┌─────────────▼────────┐   ┌────────────────▼─────┐
│ capture thread │   │  pipeline thread     │   │  llm thread          │
│ CoreAudio tap  │   │  whisper-rs          │   │  llama-cpp-2         │
│ + cpal mic     │   │  sherpa-onnx diarize │   │  summarize/title/chat│
│ → mix → WAV    │   │  fastembed           │   │  streaming tokens    │
└────────────────┘   └──────────┬───────────┘   └──────────┬───────────┘
                                │                          │
                          ┌─────▼──────────────────────────▼─────┐
                          │  SQLite (single writer, WAL mode)    │
                          └──────────────────────────────────────┘
```

- **FR-ARCH-1** UI input latency must stay under 50 ms while transcription, diarization or inference is running. Verify by holding a key during a long transcription; there must be no perceptible lag or dropped frames.
- **FR-ARCH-2** All cross-thread communication uses typed enums (`AppEvent`, `Command`). No shared mutable state behind a mutex for pipeline data.
- **FR-ARCH-3** SQLite runs in WAL mode with exactly one writer thread. Readers may be concurrent.
- **FR-ARCH-4** Models load lazily via `OnceCell` or equivalent and stay resident for the process lifetime. Startup must not load any model.
- **FR-ARCH-5** Native model handles must be released deterministically on shutdown, not left to process teardown.

### 6.2 Module layout

Use this layout. Do not reorganise it.

```
src/
  main.rs                  entry, clap dispatch, TUI vs subcommand
  config.rs                Config structs, TOML load, env overrides, validation
  paths.rs                 all filesystem path resolution, single source of truth
  db/
    mod.rs                 connection setup, WAL, sqlite-vec registration
    schema.rs              DDL and versioned migrations
    models.rs              row structs
    queries.rs             all SQL. No SQL anywhere else in the tree
  audio/
    mod.rs                 Recorder trait, mixing, WAV writing
    tap.rs                 Core Audio process tap via objc2-core-audio
    mic.rs                 cpal input
    level.rs               per-source RMS/peak metering and activity state
    permission.rs          TCC status checks and prompt triggering
  asr/
    mod.rs                 Transcriber trait, Segment, Word, TranscriptResult, backend selection
    whisper.rs             whisper-rs implementation (Whisper)
    parakeet.rs            sherpa-onnx implementation (NeMo TDT)
    vocab.rs               vocabulary store, per-backend biasing strategies
    decode.rs              symphonia decode + rubato resample to 16 kHz mono
  diarize/
    mod.rs                 Diarizer trait, SpeakerTurn
    sherpa.rs              sherpa-onnx implementation
    assign.rs              overlap-based speaker assignment to segments/words
  llm/
    mod.rs                 Llm trait, context budgeting, chunking, map-reduce
    llama.rs               llama-cpp-2 implementation
    chunk.rs               transcript splitting with overlap
    prompts.rs             all prompt templates
  index/
    mod.rs                 passage building, embedding, indexing
    embed.rs               fastembed wrapper
    search.rs              hybrid FTS5 + vector retrieval with RRF
    verify.rs              quote verification
  pipeline/
    mod.rs                 stage orchestration, progress events
  tui/
    mod.rs                 App state, event loop, key dispatch
    panes/{meetings,transcript,chat,recording}.rs
    modals/settings.rs     scoped persistent settings editor
    widgets/{progress,audio_monitor,citation}.rs
    theme.rs               all colours and styles, no literals in pane code
  cli/
    mod.rs                 non-interactive subcommands
  export/
    markdown.rs            transcript and summary rendering
    json.rs                machine-readable rendering
```

- **FR-ARCH-6** All SQL lives in `db/queries.rs`. No inline SQL elsewhere.
- **FR-ARCH-7** All filesystem paths resolve through `paths.rs`. No `~` expansion or path joining elsewhere.
- **FR-ARCH-8** All colours and text styles come from `tui/theme.rs`.
- **FR-ARCH-9** `unwrap()` and `expect()` are forbidden outside tests and `main.rs` startup assertions. Use typed errors.

### 6.3 Pipeline state and recovery

- **FR-ARCH-10** Stage order is `transcribe → diarize → summarize → export → index`. A disabled optional stage is persisted as `skipped`. Title generation is part of `summarize`; passage construction and embedding are part of `index`.
- **FR-ARCH-11** Before work begins, atomically mark the stage `running` and increment its attempt. Commit stage output and mark it `completed` in the same SQLite transaction wherever the output lives in SQLite. File artifacts are written to a sibling `.partial` file, flushed and atomically renamed before the completion record is committed.
- **FR-ARCH-12** At startup, any stage left `running` is changed to `failed` with error code `interrupted`. Resume begins at the first non-completed, non-skipped stage whose `input_fingerprint` still matches. If an upstream fingerprint changed, invalidate and rerun all downstream stages.
- **FR-ARCH-13** Cancellation is cooperative. A stage checks its cancellation token at bounded work units, rolls back its open transaction, removes its `.partial` artifacts, and records `cancelled`. Previously completed stages remain valid. Native calls that cannot be interrupted immediately finish their current call before cancellation is acknowledged.
- **FR-ARCH-14** Only one post-recording pipeline runs at a time in v1. Additional imports are queued FIFO. D17 decides whether chat inference is serialized with that pipeline. Recording callbacks always take priority and may not block on pipeline, LLM or database work.
- **FR-ARCH-15** `implementation_id` and `input_fingerprint` include every input that changes stage output: source-audio digest, backend and model digest, relevant config, vocabulary, prompt/template version, and index/embedding model version as applicable. This makes resume and re-index decisions deterministic.

---

## 7. Data model

Concrete DDL. Migrations are versioned and forward-only.

```sql
CREATE TABLE schema_version (version INTEGER NOT NULL, applied_at TEXT NOT NULL);

CREATE TABLE meetings (
  id            INTEGER PRIMARY KEY,
  started_at    TEXT    NOT NULL,          -- RFC3339 local with offset
  ended_at      TEXT,
  title         TEXT,                       -- generated; NULL until summarized
  duration_s    REAL    NOT NULL DEFAULT 0,
  language      TEXT    NOT NULL DEFAULT '',
  audio_path    TEXT,                       -- NULL once audio deleted
  audio_owned   INTEGER NOT NULL,           -- 1 = we recorded it, 0 = user supplied
  source        TEXT    NOT NULL,           -- 'recording' | 'import'
  speaker_count INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT    NOT NULL
);

CREATE TABLE pipeline_stages (
  meeting_id        INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  stage             TEXT    NOT NULL CHECK(stage IN ('transcribe', 'diarize', 'summarize', 'export', 'index')),
  status            TEXT    NOT NULL CHECK(status IN ('pending', 'running', 'completed', 'failed', 'cancelled', 'skipped')),
  attempt           INTEGER NOT NULL DEFAULT 0,
  input_fingerprint TEXT    NOT NULL DEFAULT '',
  implementation_id TEXT    NOT NULL DEFAULT '', -- backend/model/template/index version
  started_at        TEXT,
  completed_at      TEXT,
  error_code        TEXT,                        -- stable category, never sensitive content
  PRIMARY KEY(meeting_id, stage)
);

CREATE TABLE segments (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,
  start_s    REAL    NOT NULL,
  end_s      REAL    NOT NULL,
  speaker    TEXT,                          -- 'Speaker 1', NULL if undiarized
  text       TEXT    NOT NULL
);
CREATE UNIQUE INDEX idx_segments_meeting ON segments(meeting_id, idx);

CREATE TABLE words (                        -- only populated when word timings requested
  id         INTEGER PRIMARY KEY,
  segment_id INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
  start_s    REAL    NOT NULL,
  end_s      REAL    NOT NULL,
  text       TEXT    NOT NULL,
  score      REAL    NOT NULL DEFAULT 0,
  speaker    TEXT
);

CREATE TABLE summaries (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  template   TEXT    NOT NULL,
  body       TEXT    NOT NULL,
  model      TEXT    NOT NULL,
  created_at TEXT    NOT NULL
);

CREATE TABLE passages (                     -- retrieval unit
  id          INTEGER PRIMARY KEY,
  meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  start_s     REAL    NOT NULL,
  end_s       REAL    NOT NULL,
  speakers    TEXT    NOT NULL DEFAULT '',  -- comma-separated speakers present
  text        TEXT    NOT NULL,             -- normalized: no timestamps, no markup
  token_count INTEGER NOT NULL
);
CREATE INDEX idx_passages_meeting ON passages(meeting_id);

CREATE VIRTUAL TABLE passages_fts USING fts5(
  text, content='passages', content_rowid='id', tokenize='unicode61'
);

CREATE VIRTUAL TABLE passage_vec USING vec0(
  passage_id INTEGER PRIMARY KEY,
  meeting_id INTEGER,
  embedding FLOAT[384]
);

CREATE TRIGGER passages_ai AFTER INSERT ON passages BEGIN
  INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER passages_ad AFTER DELETE ON passages BEGIN
  INSERT INTO passages_fts(passages_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
  DELETE FROM passage_vec WHERE passage_id = old.id;
END;

CREATE TRIGGER passages_au AFTER UPDATE OF text ON passages BEGIN
  INSERT INTO passages_fts(passages_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
  INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TABLE chats (
  id         INTEGER PRIMARY KEY,
  scope_meeting_id INTEGER REFERENCES meetings(id) ON DELETE CASCADE, -- NULL = corpus
  created_at TEXT NOT NULL
);

CREATE TABLE chat_turns (
  id         INTEGER PRIMARY KEY,
  chat_id    INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  role       TEXT    NOT NULL,              -- 'user' | 'assistant'
  content    TEXT    NOT NULL,
  created_at TEXT    NOT NULL
);

CREATE TABLE chat_turn_sources (
  chat_turn_id INTEGER NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
  meeting_id   INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  PRIMARY KEY(chat_turn_id, meeting_id)
);

CREATE TABLE citations (
  id           INTEGER PRIMARY KEY,
  chat_turn_id INTEGER NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
  passage_id   INTEGER NOT NULL REFERENCES passages(id) ON DELETE CASCADE,
  quote        TEXT    NOT NULL,
  verified     INTEGER NOT NULL             -- 1 verified, 0 not found in source
);

CREATE TRIGGER meetings_chat_cleanup_bd BEFORE DELETE ON meetings BEGIN
  DELETE FROM chats
  WHERE id IN (
    SELECT DISTINCT ct.chat_id
    FROM chat_turns ct
    JOIN chat_turn_sources src ON src.chat_turn_id = ct.id
    WHERE src.meeting_id = old.id
  );
END;
```

### 7.1 Notes on the schema

- **`meetings.audio_owned` is load-bearing.** It records whether sosus created the audio file or the user supplied it. See [FR-REC-8](#82-recording).
- **`passages.text` is normalized.** No timestamps, no speaker markup, no markdown. This is what retrieval matches against and what quote verification checks. Rendering for display is a separate concern.
- **Meeting folders remain the portable artifact.** The database is the index and source of truth for the application; each sosus-created recording keeps `recording.wav`, `transcript.md` and `summary.md` together under `~/sosus/recordings/<YYYY-MM-DD_HHMM>/` so the archive survives the database and is greppable. Imported source audio remains in place, while its derived artifacts use the same meeting-folder pattern.
- `PRAGMA foreign_keys = ON` on every connection. Without it the `ON DELETE CASCADE` clauses do nothing.
- `pipeline_stages` is operational state, not an audit log. A retry increments `attempt` and updates the row. Sensitive native error strings go only to the redacted file log; `error_code` stores a stable application-defined category.
- The passage triggers are mandatory. They keep the external-content FTS index and vector index consistent when passages are deleted directly or through a meeting cascade.
- `chat_turn_sources` records every meeting whose text was supplied to an assistant turn, whether or not the model cited it. Deleting a meeting deletes any chat containing derived text from that meeting so corpus-scope answers cannot retain supposedly deleted content.

---

## 8. Functional requirements

### 8.1 Permissions

- **FR-PERM-1** On first run and before microphone capture, query microphone TCC status explicitly. Core Audio exposes no separate AudioCapture preflight API, so creating the process tap is the authoritative system-audio permission check; map its permission-denied result to the same actionable permission flow rather than inferring from samples.
- **FR-PERM-2** If a permission is missing, present an actionable message naming the exact System Settings panel, and offer to open it. Never begin a recording that cannot produce audio.
- **FR-PERM-3** Never infer a permission problem from silent audio. Query microphone status directly and use the Core Audio tap-creation result for system audio. Silence detection is a separate, non-fatal quality warning.
- **FR-PERM-4** A recording that produces only silence emits a warning after the fact and still keeps the audio file. It must never exit non-zero or discard data.

### 8.2 Recording

- **FR-REC-1** Capture system audio via Core Audio process taps. Two modes: `all` (everything except sosus itself) and `processes` (an explicit PID allowlist).
- **FR-REC-2** Capture the default microphone simultaneously by default. Configurable off, and a specific device selectable by name.
- **FR-REC-3** Mix system and microphone in 32-bit float on the non-realtime writer worker, then store one mono output stream. The Core Audio tap timeline is canonical. Align the streams from their host timestamps and adaptively resample the microphone to correct device-clock drift before mixing. Apply `system_gain_db` and `mic_gain_db`, each defaulting to `-3.0` dB and valid from `-24.0` through `+12.0` dB. Sum the gained sources and apply a peak limiter with a `-1.0` dBFS ceiling, 5 ms look-ahead, immediate gain reduction and 100 ms release before quantization. The limiter stays at unity below its ceiling, and clean stop flushes its look-ahead tail. Do not apply automatic gain control or normalization. The remaining alignment error after a two-hour synthetic dual-clock test must be under 50 ms.
- **FR-REC-4** Create the stable meeting folder before capture begins and write the mix as signed 16-bit PCM, mono, 48 kHz to `<meeting-folder>/recording.wav` incrementally. This uses approximately 346 MB per hour. Call `WavWriter::flush()` at least once per second so its RIFF and data lengths checkpoint; a `SIGKILL` may lose at most the uncheckpointed tail and must leave a valid, playable file through the last checkpoint.
- **FR-REC-5** Live mute toggle for the microphone only. Mute must not interrupt the system audio stream, and muted regions are written as silence so timeline alignment is preserved.
- **FR-REC-6** Report RMS and peak level independently for post-gain system audio, post-gain microphone and the final post-limiter mix at least 10 times per second. The microphone meter reflects the post-mute signal, so it drops to silence while muted; mute state remains explicitly labelled. Track source clipping, limiter engagement and queue dropouts separately. Audio callbacks may update fixed-size accumulators and send bounded metric messages, but gain, mixing, limiting, rolling history and rendering belong off the callbacks and must not add allocation, locking or filesystem work to them.
- **FR-REC-7** Auto-stop after `silence_timeout` seconds below `silence_threshold_dbfs` on the final mixed signal, with two seconds of hysteresis. Defaults are 300 seconds and `-50.0` dBFS; timeout `0` disables. The threshold is valid from `-90.0` through `-20.0` dBFS. Auto-stop is reported distinctly from a manual stop.
- **FR-REC-8** **Only delete audio where `audio_owned = 1`.** When `keep_recording = false`, delete recordings sosus made. **Never delete a user-supplied file under any configuration.** Enforce this in the type system: the delete function must accept a type that can only be constructed for owned recordings, so deleting imported audio is a compile error rather than an untested path.
- **FR-REC-9** `Ctrl+C` during recording stops cleanly, finalises the WAV, and proceeds to the pipeline. It must not abort the process.
- **FR-REC-10** Native audio callbacks never allocate, wait on a mutex, touch SQLite or perform filesystem I/O. They write into bounded preallocated queues. Queue overflow records a dropout counter and warning while preserving timeline length with silence.
- **FR-REC-11** A microphone device change or stream failure during recording does not stop system capture. Preserve alignment with silence, warn visibly, and continue unless the system-audio stream itself fails.
- **FR-REC-12** Create the Core Audio tap and private aggregate device with process-unique identifiers. Destroy both on normal stop and startup-clean any stale sosus-owned aggregate devices left by a crash; never touch a device not carrying sosus's ownership marker.

### 8.3 Transcription

#### 8.3.1 Backends

Two backends ship in v1, behind a `Transcriber` trait that is designed as a **durable extension seam**: adding a third backend later must not require touching the pipeline, the TUI, or the database. Both v1 backends must produce the same `TranscriptResult` shape so everything downstream is backend-agnostic.

> **The seam is required even if v1 ends up shipping one backend.** If the Q6 decision ladder drops Parakeet, keep the trait, the capability model and the config selector. This is a deliberate exception to the usual rule against abstracting for a single implementation, because more ASR models are expected and the seam is cheap. It is not licence for speculative generality: see [§8.3.1.2](#8312-what-the-seam-must-not-become).

- **FR-ASR-1** Backend selectable via `transcription.backend`: `"parakeet"` (default) or `"whisper"`.
- **FR-ASR-2** **Parakeet:** NVIDIA Parakeet TDT 0.6B v3 through `sherpa-onnx` NeMo transducer support. Provides punctuation, capitalisation and native word-level timestamps. Covers 25 European languages with automatic detection.
- **FR-ASR-3** **Whisper:** `whisper-rs` with the Metal backend. Model configurable, default `base`. Used for languages Parakeet does not cover.
- **FR-ASR-4** If the configured language is set and the selected backend does not support it, refuse at startup with a message naming the languages that backend covers and suggesting the other backend. Do not silently transcribe with the wrong model.
- **FR-ASR-5** Do **not** auto-switch backends based on detected language. Backend choice is the user's, made explicitly in config. Auto-selection is a v2 consideration.

##### 8.3.1.1 The seam contract

Backends differ in ways the pipeline and UI genuinely need to branch on. Express those differences as declared capabilities, so callers query the seam instead of special-casing a backend name.

```rust
pub enum LanguageSupport {
    Universal,                        // Whisper: 99 languages
    Enumerated(&'static [&'static str]),  // Parakeet: 25 ISO codes
}

pub enum WordTimestamps {
    Native,                           // Parakeet: free, always on
    OptIn { experimental: bool },     // Whisper: DTW, costs a pass
    Unsupported,
}

pub enum VocabularyBiasing {
    /// Decoder-level contextual biasing. Strongly enforced.
    ContextGraph { max_terms: Option<usize> },
    /// Prompt priming only. A soft hint, budget-limited.
    PromptPriming { max_prompt_tokens: usize },
    Unsupported,
}

pub struct BackendCapabilities {
    pub id: &'static str,
    pub display_name: &'static str,
    pub languages: LanguageSupport,
    pub word_timestamps: WordTimestamps,
    pub vocabulary: VocabularyBiasing,
    pub emits_punctuation: bool,
}

pub trait Transcriber: Send {
    fn capabilities(&self) -> &BackendCapabilities;
    fn prepare(&mut self, opts: &PrepareOptions) -> Result<(), AsrError>;
    fn transcribe(
        &mut self,
        audio: &Audio16kMono,
        opts: &TranscribeOptions,
        progress: &dyn ProgressSink,
    ) -> Result<TranscriptResult, AsrError>;
}
```

- **FR-ASR-5a** Every backend declares `BackendCapabilities`. Callers must consult it rather than matching on backend identity.
- **FR-ASR-5b** Language validation ([FR-ASR-4](#831-backends)) is implemented once against `LanguageSupport`, not per backend.
- **FR-ASR-5c** The vocabulary UI disclosure ([FR-VOCAB-16](#833-custom-vocabulary)) reads `VocabularyBiasing` to decide what to tell the user. No backend names in UI code.
- **FR-ASR-5d** Word-timestamp policy ([FR-ASR-11](#832-common-requirements)) is decided from `WordTimestamps`, so a future backend with native timings gets the cheap path automatically.
- **FR-ASR-5e** Backend selection is a **closed enum** parsed from config, with an exhaustive `match` at one construction site. An unknown value is a config error listing the valid options.

##### 8.3.1.2 What the seam must NOT become

The seam is a trait, a capability struct and one `match`. Anything more is over-building.

- **No plugin registry, no dynamic registration, no `dlopen`.** Backends are compiled in.
- **No dynamic dispatch beyond one `Box<dyn Transcriber>`** held by the pipeline.
- **No capability negotiation framework, scoring or auto-selection.** [FR-ASR-5](#831-backends) forbids auto-selection in v1.
- **No backend-specific concept in the trait.** `greedy_search`, `modified_beam_search`, `initial_prompt`, `n_ctx`, `bpe.vocab`, ggml types and ONNX session options are all implementation details and must not appear in `asr/mod.rs`. `VocabularyBiasing` exposes the *mechanism class* because the UI must disclose it; the decoder that implements it stays private.
- **No config surface for tuning internals** beyond what [§10](#10-configuration) specifies.

##### 8.3.1.3 Validating the seam against real candidates

Check the trait shape against these concrete candidates, not imagined ones. If a candidate would force a trait change, fix the trait now while it is cheap.

| Candidate | Why it might be added | What it stresses in the seam |
|---|---|---|
| `fluidaudio-rs` (Parakeet or Qwen3-ASR on the Neural Engine) | Fastest option on Apple Silicon | Reintroduces a Swift build dependency, so `prepare()` must tolerate slow first-run model compilation (20 to 30 s) |
| Whisper `large-v3-turbo` | Better accuracy at acceptable speed | Same backend, different weights: model choice must not be a backend choice |
| Qwen3-ASR | Strong CJK coverage | `LanguageSupport::Enumerated` with a different, non-overlapping set |
| Parakeet with a fixed beam search | Upstream resolves #3267 | `VocabularyBiasing::ContextGraph` becomes reliable without any trait change |

#### 8.3.2 Common requirements

- **FR-ASR-6** Decode any supported input to 16 kHz mono `f32` via `symphonia` plus `rubato`. Supported: `.wav .mp3 .m4a .flac .ogg .mp4 .m4v .mov`. **No ffmpeg dependency.**
- **FR-ASR-7** Decode once. The same 16 kHz buffer feeds ASR and diarization.
- **FR-ASR-8** Language auto-detected by default, overridable.
- **FR-ASR-9** Report progress as a fraction to the TUI at least once per second.
- **FR-ASR-10** Thread count defaults to the physical core count, overridable in config.
- **FR-ASR-11** Word-level timestamps: Parakeet provides them natively at no extra cost, so always populate them. For Whisper, enable DTW token-level timestamps **only** when word-level output is required (JSON export or word-level speaker assignment), because it costs time and its precision is experimental.

#### 8.3.3 Custom vocabulary

Meetings are full of names, product names and internal jargon that general ASR models mis-transcribe. A user-managed vocabulary must bias recognition toward these terms.

> **The two backends support this by fundamentally different mechanisms, with different quality.** This is not an abstraction to hide; surface the difference to the user.

**Storage and management**

- **FR-VOCAB-1** Vocabulary is a list of terms, each with an optional boost weight and an optional category label. Stored in config under `[vocabulary]` and additionally loadable from a plain-text file, one term per line, so it can be version-controlled or shared.
- **FR-VOCAB-2** Terms may be multi-word phrases, not just single words.
- **FR-VOCAB-3** Terms are case-preserving. `Asteron` must be capitalised in output, not normalised to `asteron`.
- **FR-VOCAB-4** Support per-meeting vocabulary overrides so a specific recording can add terms without polluting global config.
- **FR-VOCAB-5** `sosus vocab` subcommands manage the list: `list`, `add <term> [--weight N]`, `remove <term>`, `import <file>`.

**Parakeet path: true contextual biasing**

- **FR-VOCAB-6** Pass the vocabulary to `sherpa-onnx` as a hotwords file with per-term scores. It builds a `ContextGraph` prefix tree that boosts those token sequences during decoding. This is genuine contextual biasing and is the higher-quality path.
- **FR-VOCAB-7** Hotwords require `modified_beam_search` decoding. `greedy_search` ignores them entirely. Select the decoder based on whether a vocabulary is active.
- **FR-VOCAB-8** Hotwords with NeMo TDT models require the model's `bpe.vocab` file. Download and verify it alongside the model, and fail with a clear message if it is missing rather than silently producing unbiased output.
- **FR-VOCAB-9** Default hotword score must be configurable. Start at a conservative value: over-boosting causes the model to hallucinate vocabulary terms into unrelated audio.
- **FR-VOCAB-10** **Mitigate the known `modified_beam_search` defect.** See [§13.7](#137-parakeet-hotwords). Detect empty or degenerate output and automatically retry the affected window with `greedy_search`, logging that biasing was skipped for that window. A vocabulary feature must never make transcription worse than having no vocabulary at all.

**Whisper path: prompt priming**

- **FR-VOCAB-11** whisper.cpp has **no hotwords or contextual biasing support.** It is a long-standing open feature request upstream. Do not look for the parameter; it does not exist.
- **FR-VOCAB-12** Bias Whisper by injecting vocabulary terms into `initial_prompt` as a natural-language phrase, which is the only mechanism that works reliably. Construct it as prose, for example `Terms used in this meeting: Asteron, Project Juniper, ZX-41, signal reconciliation.`
- **FR-VOCAB-13** **Whisper's prompt window is limited to 224 tokens.** When the vocabulary exceeds it, prioritise by descending weight then by recency of addition, and truncate. Warn once, naming how many terms were dropped. Never silently truncate.
- **FR-VOCAB-14** Optionally support user-supplied `initial_prompt` free text in addition to the vocabulary list, concatenated within the same token budget.
- **FR-VOCAB-15** Do **not** use whisper.cpp GBNF grammar for vocabulary in v1. It exists, but upstream reports it as unreliable, and constraining the whole output to a grammar is the wrong shape for open-ended meeting speech.
- **FR-VOCAB-16** The TUI must state which biasing mechanism is active, so the user understands that vocabulary is strongly enforced on Parakeet and only a soft hint on Whisper.

### 8.4 Diarization

- **FR-DIA-1** Diarization is **enabled by default** in v1. It requires no token or account.
- **FR-DIA-2** Use `sherpa-onnx` `OfflineSpeakerDiarization` on the same 16 kHz mono buffer used for ASR. Do not decode twice.
- **FR-DIA-3** Honour `min_speakers` and `max_speakers` when set. `0` means auto-detect.
- **FR-DIA-4** **Assign speakers to segments by maximum temporal overlap** between diarization turns and ASR segment spans. This is the primary path and must not depend on word timings.
- **FR-DIA-5** Word-level assignment happens only when word timings exist, and only affects JSON output.
- **FR-DIA-6** Label speakers `Speaker 1`, `Speaker 2`, ordered by first appearance in the timeline, not by internal cluster ID.
- **FR-DIA-7** Report per-stage progress (segmentation, embedding, clustering) as TUI sub-steps.
- **FR-DIA-8** If diarization fails, the transcript must still be saved without speaker labels, and the failure surfaced as a warning. A diarization failure must never lose a transcript.
- **FR-DIA-9** Store `speaker_count` on the meeting row for display in the Meetings pane.
- **FR-DIA-10** Speaker assignment ties resolve to the speaker whose overlapping turn starts first. A segment with no overlap inherits the nearest turn only when the gap is at most 1.0 second; otherwise its speaker remains `NULL`. Overlapping speech remains a single segment-level label in v1, selected by maximum overlap; do not fabricate multi-speaker text from a mono mix.

### 8.5 Summarization

- **FR-SUM-1** Summarize with `llama-cpp-2` using a local GGUF model, default Phi-4-mini Q4_K_M.
- **FR-SUM-2** Build the LLM input from segments as `Speaker N: text`, joining consecutive segments from the same speaker into one line. Fall back to plain concatenated text when undiarized.
- **FR-SUM-3** Context budgeting lives in one place, shared by all LLM operations:
  - Reserve tokens for the completion: `min(1024, max(256, context_size / 4))`.
  - Input budget is `context_size - completion_reserve - tokens(scaffolding)`.
  - Count tokens with the model's own tokenizer, never a character heuristic.
- **FR-SUM-4** When the transcript exceeds the input budget, split into overlapping chunks, summarize each, then reduce the partials into one summary. Split on segment boundaries first, sentence boundaries only if a segment alone is too large, and between words only as a last resort. Overlap is 5% of chunk budget.
- **FR-SUM-5** The reduce step must terminate for any input. If no two partials fit together in one pass, truncate each to half the budget so a pair is guaranteed to fit. Reduce repeatedly until one summary remains, so output does not depend on chunk count.
- **FR-SUM-6** The reduce step runs under the active template's system prompt so a custom template's persona and section structure survive.
- **FR-SUM-7** Built-in templates: `meeting` (default), `lecture`, `brief`. Users may define custom templates in config.
- **FR-SUM-8** Validate custom templates at config load: they must contain `{transcript}`, and any other brace must be escaped. Report a clear error rather than failing at inference time.
- **FR-SUM-9** Generate a 3 to 5 word title from the summary and use it as the meeting's display name. Title generation must never rename the stable timestamp-based meeting folder.
- **FR-SUM-10** Strip reasoning tags such as `<think>...</think>` from all model output before use.

### 8.6 Indexing and search

- **FR-IDX-1** After transcription, build passages of roughly 400 to 600 tokens, split on segment boundaries, with 15% overlap. Record the time span and speakers present.
- **FR-IDX-2** Passage text is normalized: no timestamps, no speaker prefixes, no markdown.
- **FR-IDX-3** Embed each passage with the multilingual model selected in D1 and store it in `passage_vec`. Update the vector dimension in the DDL if D1 does not choose the 384-dimensional recommendation.
- **FR-IDX-4** Index passage text in `passages_fts`.
- **FR-IDX-5** L2-normalize passage and query embeddings before storage/search. Run BM25 over FTS5 and k-NN over the normalized vectors, then fuse with Reciprocal Rank Fusion (`k = 60`). L2 distance over normalized vectors has the same ranking as cosine distance. Return top `n` passages, default 12.
- **FR-IDX-6** Retrieval must support restriction to a single `meeting_id`. Apply the restriction inside the vector query using `passage_vec.meeting_id`, not after taking a corpus-wide top-k.
- **FR-IDX-7** Re-indexing a meeting must be idempotent and transactional: delete its passages (whose triggers remove FTS and vector rows), then rebuild passages, FTS rows and normalized vectors. A failed or cancelled rebuild rolls back to the previous complete index.
- **FR-IDX-8** `sosus reindex` rebuilds the whole index from stored segments without re-transcribing.
- **FR-IDX-9** The query-side instruction/prefix required by the selected embedding model is part of the model manifest and is applied exactly once to queries, never to stored passages unless that model explicitly requires a document prefix.
- **FR-IDX-10** Run the FTS5 `integrity-check` with external-content verification after migrations and whole-corpus reindexing. Treat failure as index corruption and offer `sosus reindex`; do not silently return partial results.

### 8.7 Chat

- **FR-CHAT-1** Default scope is the whole archive. When a meeting is selected the user may toggle scope to that meeting.
- **FR-CHAT-2** In single-meeting scope, if the full transcript fits the input budget, pass all of that meeting's passage blocks in timeline order and skip ranking. Otherwise retrieve within that meeting. Passage IDs remain present in both paths so citations resolve identically.
- **FR-CHAT-3** Every supplied passage is labelled with an opaque stable reference (`P<passage_id>`), meeting title, date, timestamp and speaker. The model cites the opaque reference; the application, not the model, renders human-facing citation metadata.
- **FR-CHAT-4** Stream tokens into the Chat pane as they arrive. Do not wait for a complete response.
- **FR-CHAT-5** Maintain conversation history within a chat session, trimming oldest turns when the budget is exceeded while always preserving the system prompt and the newest user turn.
- **FR-CHAT-6** Parse citations only from the exact marker `[P<integer>]`. Reject markers for passages that were not supplied in the current context. Store valid citations in `citations`, linked directly to that passage; never infer a link from title or timestamp text.
- **FR-CHAT-7** Citations are selectable in the TUI and jump the Transcript pane to that meeting and timestamp.
- **FR-CHAT-8** **Quote verification.** For every quoted span in the response, check it appears in the cited passage's normalized text:
  - Compare against normalized passage text, never rendered markdown. Timestamps and speaker labels in the rendering must not cause false negatives.
  - **Verify quotes of any length.** Do not skip short quotes. A minimum-length threshold silently passes fabricated short quotes, which is worse than no verification.
  - Use case-insensitive, Unicode- and whitespace-normalized exact substring comparison. If a quote contains an ellipsis, every non-empty fragment must appear in order in the same passage. Do not accept a partially matching long quote.
  - Mark unverified quotes visibly in the UI and set `citations.verified = 0`.
- **FR-CHAT-9** If retrieval returns nothing, say so plainly. Never fall back to sending the entire corpus, and never fabricate an answer.
- **FR-CHAT-10** Every factual sentence in the direct answer must end in one or more supplied passage markers. Sentences without a valid marker are visibly marked unsupported even if later evidence bullets contain citations.
- **FR-CHAT-11** For each assistant turn, persist every distinct source meeting represented in the supplied context in `chat_turn_sources` before storing streamed output. This is privacy provenance, not citation provenance.

### 8.8 TUI

- **FR-TUI-1** Four panes: Meetings (list), Transcript (reader), Chat, Recording. Tab and Shift+Tab cycle focus; the focused pane is visually unambiguous.
- **FR-TUI-2** Global keys: `q` quit, `?` help overlay, `F2` Settings modal, `r` start/stop recording, `/` search, `Tab` cycle focus, `Ctrl+C` graceful shutdown. Printable global keys are disabled while a text editor has focus so normal typing is never intercepted; function and control keys remain available.
- **FR-TUI-3** Meetings pane shows date, title, duration and speaker count, newest first, with incremental filtering.
- **FR-TUI-4** Transcript pane shows speaker-grouped, timestamped text with scrolling, jump-to-timestamp and speaker filtering.
- **FR-TUI-5** The Recording pane contains a compact audio activity monitor with independently labelled `System`, `Microphone` and `Recording` rows. At normal widths each row shows a horizontal RMS bar, peak marker, numeric peak in dBFS, and approximately five seconds of rolling level history. At constrained widths retain the labels, states and live RMS bars; the numeric value and history may be hidden. This is a signal-presence diagnostic, not a frequency-spectrum analyser.
- **FR-TUI-6** A pipeline progress display shows every stage (transcribe, diarize with sub-steps, summarize, index) with determinate bars where a fraction is known and spinners where it is not.
- **FR-TUI-7** Long-running work must be cancellable. `Esc` cancels the active pipeline stage and leaves the database consistent.
- **FR-TUI-8** Handle terminal resize without corruption or panic.
- **FR-TUI-9** Restore the terminal on every exit path including panic. Install a panic hook that leaves the alternate screen and disables raw mode before printing.
- **FR-TUI-10** Minimum usable size 80x24. Below that, show a single clear message instead of a broken layout.
- **FR-TUI-11** Errors appear in the UI as dismissable, readable messages. Never print to stdout or stderr while the TUI is active.
- **FR-TUI-12** The activity monitor distinguishes `ACTIVE`, `SILENT`, `MUTED`, `OFF` and `LOST` states without relying on colour alone. Show a latched `CLIP` indicator until acknowledged or recording stops, and show the cumulative dropout count whenever it is non-zero. A quiet source is informational, not a permission error; permission and stream failures use the explicit states above.
- **FR-TUI-13** `F2` opens a centred, scrollable Settings modal. It edits only common persistent settings: meeting-folder root, output format, keep-recording policy, retention days, capture mode, microphone enabled/device, system and microphone gain, silence timeout and threshold, transcription backend/built-in model/language, diarization enabled, summarization enabled and summary template. Process selection, vocabulary, custom prompts, custom model paths, thread counts, speaker bounds and retrieval tuning remain in their dedicated flows, CLI commands or `config.toml`.
- **FR-TUI-14** Settings use `Tab` and `Shift+Tab` to move between controls, arrow keys to change enumerated values, `Enter` to edit or select, `Ctrl+S` to validate and save, and `Esc` to close. If values have changed, `Esc` asks whether to discard them. The modal must be usable at 80x24 and scroll instead of clipping.
- **FR-TUI-15** Saving settings never changes an active recording or pipeline. The modal states that changes apply to the next operation. After a successful save, the in-memory configuration is reloaded for future work and the TUI shows a brief confirmation; a validation or write failure leaves the modal open with the relevant field and error visible.

### 8.9 Export

- **FR-EXP-1** Write `transcript.md` and `summary.md` into the meeting's stable `<output-dir>/<YYYY-MM-DD_HHMM>/` folder. For a sosus-created recording, this is the same folder that already contains `recording.wav`.
- **FR-EXP-2** JSON export includes segments, optional words with timings and scores, language, duration and speakers.
- **FR-EXP-3** Any path reported to the user must be the final, stable path. Never print a path that does not exist.

---

## 9. Prompt templates

Ship these verbatim. They are part of the specification.

### 9.1 `meeting` (default)

System:
```
You are a meeting notes assistant. You produce clear, structured summaries of meeting transcripts.
```

User:
```
Summarize the following meeting transcript into structured meeting notes.

Include these sections:
## Summary
A brief 2-3 sentence overview of what the meeting was about.

## Key Points
Bullet points of the main topics discussed and decisions made.

## Action Items
Bullet points of any tasks, assignments, or follow-ups mentioned. Include who is responsible if mentioned.

## Decisions
Bullet points of any explicit decisions that were made.

---

Transcript:
{transcript}
```

### 9.2 `lecture`

System:
```
You are an academic note-taking assistant. You produce clear, structured notes from lecture and seminar transcripts.
```

User: same shape as `meeting` with sections `## Summary`, `## Key Concepts`, `## Key Takeaways`.

### 9.3 `brief`

System:
```
You are a concise summarization assistant. You produce short, scannable summaries.
```

User:
```
Summarize the following transcript into 3-5 concise bullet points.
Return only the bullet points, no headers or additional structure.

Transcript:
{transcript}
```

### 9.4 Reduce (merging chunk summaries)

Appended to the active template's system prompt:
```
You are given several partial sets of notes, each covering a consecutive part of the same long transcript. You consolidate them into one final set of notes that keeps the exact structure the partial notes already use.
```

User:
```
The notes below were produced from consecutive parts of a single long transcript. Every part was summarized with the same instructions, so the parts already share their structure.

Merge them into one set of notes covering the whole transcript:
- Keep exactly the sections and headings that appear in the partial notes, in the same order. Do not add, rename, drop, or reorder sections.
- If the partial notes use no headings, return the merged notes in that same shape.
- Consecutive parts overlap, so fold duplicates and near-duplicates into a single entry.
- Keep every distinct fact, decision, task, and owner. Do not invent anything the partial notes do not state.
- Write the result as the final notes for the whole transcript, never as commentary about the parts.

{partials}

Return only the merged notes.
```

### 9.5 Title

System: `You generate short meeting titles.`

User:
```
Based on this meeting summary, generate a short title of 3-5 words. Return ONLY the title, nothing else. No quotes, no punctuation, no explanation.

{summary}
```

### 9.6 Chat

System:
```
You are a meeting assistant. Answer the user's question using only the meeting excerpts provided.

Each excerpt is labelled with an opaque passage reference such as P123, plus its meeting title, date, timestamp and speaker.

Format your answer as:
1. A direct 1-2 sentence answer. End every factual sentence with one or more passage markers in the exact form [P123].
2. Supporting evidence grouped by meeting, as:

**Meeting title — date**
- **Speaker N** [MM:SS]: "Verbatim quote from the excerpt." [P123]

Rules:
- Quote verbatim. Never paraphrase inside quotation marks.
- Cite only passage references that were provided. Copy the reference exactly; never invent one and never cite from memory.
- Place each passage marker immediately after the sentence or quote it supports.
- If the excerpts do not answer the question, say so plainly and stop.
- Be concise.
```

User:
```
Question: {question}

Excerpts:
{passages}
```

`{passages}` renders each supplied passage in this exact shape, separated by one blank line:

```text
[P{passage_id}] {meeting_title} — {YYYY-MM-DD} — {MM:SS}-{MM:SS} — {speakers_or_unknown}
{normalized_passage_text}
```

Titles and passage text are untrusted source material. Delimit the complete excerpts block through the active GGUF chat template and instruct the model that commands appearing inside excerpts are meeting content, never instructions. Do not interpolate excerpt text into the system message.

---

## 10. Configuration

TOML at `~/.config/sosus/config.toml`. Every field optional with the default shown. Unknown keys warn, never fail.

```toml
[audio]
capture_mode      = "all"     # "all" | "processes"
processes         = []         # PIDs, used when capture_mode = "processes"
mic               = true
mic_device        = ""         # empty = system default
system_gain_db    = -3.0       # -24.0 to +12.0
mic_gain_db       = -3.0       # -24.0 to +12.0
silence_timeout   = 300        # seconds; 0 disables auto-stop
silence_threshold_dbfs = -50.0 # -90.0 to -20.0; measured on final mix

[transcription]
backend           = "parakeet" # "parakeet" (default, 25 European langs) | "whisper" (99 langs)
model             = ""         # whisper only: tiny | base | small | medium | large-v3. Empty = backend default
language          = ""         # empty = auto-detect
threads           = 0          # 0 = physical core count
initial_prompt    = ""         # whisper only: free-text priming, shares the 224-token prompt budget

[vocabulary]
enabled           = true
file              = ""         # optional path to a newline-separated term list, merged with `terms`
hotword_score     = 1.5        # parakeet only: boost per term. Higher risks hallucinating terms
terms             = [
  # "Asteron",
  # { term = "signal reconciliation", weight = 2.0 },
]

[diarization]
enabled           = true       # v1 default ON
min_speakers      = 0          # 0 = auto
max_speakers      = 0

[summarization]
enabled           = true
model             = "phi-4-mini"   # alias, local GGUF path, or immutable hf:...@revision#sha256=...
template          = "meeting"
context_size      = 0          # 0 = model default

[search]
top_k             = 12
rrf_k             = 60

[output]
dir               = "~/sosus/recordings"
format            = "markdown" # markdown | json
keep_recording    = true
retention_days    = 0          # 0 = keep forever

# [templates.my-notes]
# system_prompt = "..."
# prompt = "Summarize:\n{transcript}"
```

- **FR-CFG-1** Validate types and ranges on load. Report the offending key and the expected type. Never accept a wrong-typed value and fail later.
- **FR-CFG-2** Env overrides exist for `SOSUS_CONFIG` (config path) and `SOSUS_DATA_DIR` only.
- **FR-CFG-3** `sosus config` opens the file in `$EDITOR`, creating it with commented defaults if absent.
- **FR-CFG-4** Effective-setting precedence is CLI flag, then environment override, then config file, then built-in default. Invocation flags never rewrite the config file.
- **FR-CFG-5** The Settings modal edits only the config-file layer. If an environment or invocation override shadows an editable value, display both the saved and effective values and label the override; never imply that saving will change the active override.
- **FR-CFG-6** Use `toml_edit` to modify only the known keys changed in the modal. Preserve comments, whitespace, relative ordering and unknown keys; never serialize the full typed config over the user's document. A newly created file uses the documented, commented defaults.
- **FR-CFG-7** On opening the modal, fingerprint the config file's bytes. Before saving, re-read and compare the fingerprint. If another process changed the file, refuse to overwrite it and offer to reload the modal from disk; v1 does not attempt a three-way merge.
- **FR-CFG-8** Validate the complete candidate document through the same typed loader used at startup before writing. Save through a sibling temporary file with mode `0600`, flush and `fsync` it, then atomically rename it over `config.toml`; `fsync` the parent directory. A failed save must leave the previous file intact.

---

## 11. CLI surface

`sosus` is deliberately dual-mode: the TUI and CLI are two front ends over the same application services and database queries. Core behavior must not be implemented separately in `tui/` and `cli/`.

With no subcommand, launch the TUI only when stdin and stdout are terminals. `sosus tui` always requests the TUI and fails clearly if no interactive terminal is available. Bare `sosus` in a pipe or non-interactive process prints plain help and exits successfully; it never emits terminal control sequences.

| Command | Behaviour |
|---|---|
| `sosus` | Launch the TUI when interactive; otherwise print help |
| `sosus tui` | Explicitly launch the TUI |
| `sosus record` | Record, process, print result paths. No TUI |
| `sosus transcribe <file>` | Transcribe and diarize a file, write outputs alongside it |
| `sosus summarize <file>` | Summarize an existing transcript |
| `sosus ask <question>` | One-shot query, print answer with citations |
| `sosus import <dir>` | Ingest every supported media file in a directory |
| `sosus meetings` | List archived meetings newest first |
| `sosus show <meeting>` | Show one meeting's metadata, summary and transcript path |
| `sosus search <query>` | Return ranked passages with meeting, speaker and timestamp |
| `sosus export <meeting>` | Regenerate portable artifacts from stored data |
| `sosus delete <meeting>` | Preview and delete one meeting according to ownership rules |
| `sosus resume [meeting]` | Resume interrupted or failed pipeline work |
| `sosus reindex` | Rebuild search index from stored segments |
| `sosus warmup` | Prefetch all models for the current config |
| `sosus vocab list\|add\|remove\|import` | Manage the custom vocabulary |
| `sosus devices` | List audio input devices |
| `sosus apps` | List running processes with PIDs for `capture_mode = "processes"` |
| `sosus config` | Open config in `$EDITOR` |
| `sosus cleanup` | Apply retention policy, or remove data with explicit flags |

`<meeting>` accepts a numeric database ID or an exact meeting-directory name such as `2026-08-21_1430_2`. A missing name is an error that prints matching candidates without modifying anything.

### 11.1 Global flags

| Flag | Behaviour |
|---|---|
| `--config <path>` | Use this config file for the invocation |
| `--data-dir <path>` | Override the model/database data directory |
| `--output-dir <path>` | Override the meeting-folder root for portable artifacts and sosus-created recordings |
| `--json` | Emit one machine-readable result document and no human prose on stdout |
| `--no-color` | Disable colour even on an interactive terminal |
| `-q`, `--quiet` | Suppress non-error human progress and warnings; does not change file logging |

### 11.2 Processing flags

These override config for one invocation and appear only on commands where they are meaningful.

| Flag | Commands | Behaviour |
|---|---|---|
| `--backend <parakeet\|whisper>` | `record`, `transcribe`, `import` | Select ASR backend |
| `--asr-model <name-or-path>` | `record`, `transcribe`, `import` | Select ASR weights without confusing them with the LLM model |
| `--language <iso-code>` | `record`, `transcribe`, `import` | Override language detection |
| `--threads <n>` | processing commands | Override physical-core default; must be at least 1 |
| `--term <text>` | `record`, `transcribe`, `import` | Add one per-meeting vocabulary term; repeatable |
| `--vocab-file <path>` | `record`, `transcribe`, `import` | Merge a per-meeting vocabulary file |
| `--no-vocabulary` | `record`, `transcribe`, `import` | Disable global and per-meeting vocabulary |
| `--no-diarize` | `record`, `transcribe`, `import` | Skip diarization |
| `--min-speakers <n>` / `--max-speakers <n>` | `record`, `transcribe`, `import` | Override diarization bounds |
| `--no-summarize` | `record`, `import` | Skip summary and title generation |
| `--template <name>` | `record`, `summarize`, `import` | Select summary template |
| `--llm-model <alias-or-path>` | `record`, `summarize`, `import`, `ask` | Override summarization/chat model |
| `--format <markdown\|json\|both>` | commands that write artifacts | Override artifact format for the invocation; final default semantics depend on D10 |

### 11.3 Recording, archive and query flags

| Flag | Commands | Behaviour |
|---|---|---|
| `--capture-mode <all\|processes>` | `record` | Select tap mode |
| `--app <bundle-id>` | `record` | Include an application; repeatable and contingent on D5 |
| `--pid <pid>` | `record` | Include a current process; repeatable |
| `--no-mic` / `--mic-device <name>` | `record` | Disable or select microphone |
| `--system-gain-db <db>` / `--mic-gain-db <db>` | `record` | Override source gain for this recording; each accepts `-24.0` through `+12.0` |
| `--silence-timeout <seconds>` | `record` | Override auto-stop; `0` disables |
| `--silence-threshold-dbfs <db>` | `record` | Override the final-mix silence threshold; accepts `-90.0` through `-20.0` |
| `--keep-recording` / `--discard-recording` | `record` | Override owned-audio retention; mutually exclusive |
| `--meeting <meeting>` | `search`, `ask` | Restrict retrieval to one meeting |
| `--limit <n>` | `meetings`, `search` | Limit returned rows; must be at least 1 |
| `--recursive` / `--no-recursive` | `import` | Explicitly control traversal; D15 chooses the default |
| `--stage <stage>` | `resume` | Resume from this stage only after validating/invalidation of downstream state |
| `--yes` | `delete`, `cleanup` | Skip confirmation after resolving and previewing the exact target set |
| `--older-than <days>` | `cleanup` | Override configured retention age for this run |
| `--audio-only` | `cleanup` | Delete eligible owned audio but preserve derived data |

- **FR-CLI-1** `--json` writes exactly one JSON document to stdout on success. On failure it writes one JSON error document to stderr and nothing to stdout. Progress is suppressed in JSON mode.
- **FR-CLI-2** In human mode, result data and final artifact paths go to stdout; progress and warnings go to stderr. ANSI output is used only when that stream is a terminal.
- **FR-CLI-3** Exit codes are: `0` success including warnings/no speech; `1` runtime failure; `2` invalid CLI or configuration; `3` partial batch failure; `130` cancellation by signal. The first `Ctrl+C` while recording remains the clean stop specified by FR-REC-9; a second `Ctrl+C` cancels and exits 130.
- **FR-CLI-4** `delete` and `cleanup` require confirmation unless `--yes`, and print the resolved files and sizes before asking. In `--json` mode they require `--yes`; never prompt on a non-terminal.
- **FR-CLI-5** Every durable meeting capability has a non-interactive CLI path. The TUI may provide more convenient navigation, but it is never required for recording, processing, retrieval, export, deletion or recovery; invocation flags cover temporary configuration overrides.

---

## 12. Non-functional requirements

| ID | Requirement | Target |
|---|---|---|
| NFR-PERF-1 | Startup to interactive TUI | < 300 ms, no model loading |
| NFR-PERF-2 | 60-minute meeting, full pipeline on M-series, Parakeet backend | < 6 min wall clock |
| NFR-PERF-2b | Same, Whisper `base` backend | < 10 min wall clock |
| NFR-PERF-3 | Chat first token, corpus scope, 500 meetings | < 3 s |
| NFR-PERF-4 | UI input latency under load | < 50 ms |
| NFR-PERF-5 | Peak RSS during pipeline, `base` + Phi-4-mini Q4 | < 6 GB |
| NFR-SIZE-1 | Release binary, stripped | < 120 MB |
| NFR-REL-1 | Crash during recording must not lose captured audio | Valid partial WAV |
| NFR-REL-2 | Interrupted pipeline is resumable from the last completed stage | Yes |
| NFR-REL-3 | Database corruption cannot be caused by normal shutdown or `Ctrl+C` | WAL + single writer |

- **NFR-SIZE-2** Build ONNX Runtime with the `minimal-build` profile where `sherpa-onnx` and `fastembed` allow it, to keep NFR-SIZE-1 achievable.
- **NFR-TEST-1** Unit tests for all pure logic: chunking, context budgeting, RRF fusion, speaker overlap assignment including gaps and ties, timestamp-based meeting-directory naming and collision suffixes, citation-marker parsing, quote verification, config parsing, format-preserving settings edits, concurrent config-change detection, vocabulary prompt construction and its 224-token truncation, stage invalidation and model-manifest validation.
- **NFR-TEST-2** Integration tests using short synthetic audio fixtures for the pipeline, with models behind a feature flag so CI can run without downloading gigabytes.
- **NFR-TEST-3** CI runs `cargo clippy -- -D warnings` and `cargo fmt --check`. **Formatting is enforced, not documented.**
- **NFR-TEST-4** Database integration tests cover foreign-key cascades, passage/FTS/vector consistency, rollback of a cancelled reindex, startup recovery from every interrupted stage, and `integrity-check` after reindex.
- **NFR-TEST-5** Audio integration tests cover two simulated device clocks with drift, queue overflow, microphone loss, mute alignment, gain bounds and application, limiter ceiling and engagement reporting, 16-bit mono/48 kHz output, independent system/microphone/mixed level reporting, activity-state transitions, periodic WAV checkpoint recovery and aggregate-device cleanup.

---

## 13. Known traps (read before writing audio or build code)

These are documented failures that cost other projects days. They are specification, not advice.

### 13.1 Core Audio taps

1. **The `exclusive` flag on `CATapDescription` sets direction, not locking.** Setting it inverts the tap from "exclude these processes" to "include only these". Misreading it yields a permanently silent stream that looks like a permission failure.
2. **`AVAudioEngine` cannot be retargeted to a tap-backed aggregate device.** It returns success and keeps reading system defaults. Use `AudioDeviceCreateIOProcIDWithBlock` on the aggregate device directly.
3. **The aggregate device's main sub-device must be a real output device**, with the tap attached as a sub-tap and `kAudioAggregateDeviceTapAutoStartKey: true`. A tap-as-main configuration produces zero samples with no error.
4. **Taps apply an ingestion-side reconstruction filter that cannot be disabled.** Irrelevant for speech, but taps are not bit-perfect.
5. **Reference implementations to read:** `insidegui/AudioCap` and `makeusabrew/audiotee`. Both are Swift; the aggregate-device sequence is what matters, not the language.

### 13.2 TCC and signing

6. **An unsigned or ad-hoc-signed binary never receives the audio capture prompt.** Not a bug to debug: sign the binary. See [§4.1](#41-signing-is-a-functional-requirement-not-release-polish).
7. **Do not rely on inheriting a terminal's permission grant.** It works, but it means the permission belongs to the terminal and covers everything that terminal ever launches. The tool must hold its own grant.
8. **`tccutil reset SystemAudioCaptureRequests dev.sosus.cli`** resets the grant during testing. Expect to need it often.

### 13.3 Hardened runtime

9. **`com.apple.security.device.audio-input` is mandatory** for microphone access under hardened runtime, even with the usage description present. Its absence presents as a permission denial with no useful diagnostic.

### 13.4 Metal shader compilation

10. **Building a `.metallib` requires full Xcode.** `xcrun --find metal` fails on a Command Line Tools install. Build with `GGML_METAL_EMBED_LIBRARY` so the `.metal` source is embedded and compiled at runtime, costing roughly one second on first model load. If runtime compilation proves incompatible with hardened runtime, precompile the `.metallib` in CI, where GitHub's macOS runners carry full Xcode. See [§16](#16-open-questions-escalate-do-not-guess).

### 13.5 Never write logs to the terminal

11. **`tracing` must write to a file only.** Any library that prints to stdout or stderr will corrupt the TUI. Capture or suppress output from `whisper-rs`, `llama-cpp-2` and `sherpa-onnx`, all of which are chatty by default. `llama.cpp` in particular writes to file descriptor 2 from C, which Rust-level redirection does not catch; suppress at the file descriptor level.
12. **Never emit ANSI cursor movement when the terminal is not interactive.** Check `IsTerminal` before any escape sequence. Piping output must produce clean plain text.

### 13.6 Data integrity

13. **`PRAGMA foreign_keys = ON` on every connection.** SQLite defaults it off, silently disabling every `ON DELETE CASCADE` in [§7](#7-data-model).
14. **Two recordings can start in the same minute.** Directory names must be unique. Add a numeric suffix on collision rather than writing into an existing directory.

### 13.7 Parakeet hotwords

15. **`modified_beam_search` with NeMo TDT is reported to hallucinate or return empty text roughly 20% of the time.** Tracked upstream as [sherpa-onnx #3267](https://github.com/k2-fsa/sherpa-onnx/issues/3267), **open** as of 2026-08-21, with a related unresolved PR #3657. Support for `modified_beam_search` on NeMo transducers only merged in February 2026 (PR #3077), so it is a young code path.

    **Scope this defect correctly before reacting to it:**
    - It affects `modified_beam_search` **only**. The same audio transcribes correctly 100% of the time under `greedy_search`. **Parakeet transcription itself has no known defect.**
    - The confirmed reproduction was on v1.12.25. We target 1.13.5 (2026-08-11). **Re-measure; do not assume the reported rate.**
    - It intersects custom vocabulary because hotwords require `modified_beam_search`. It does not affect any other feature.

    [FR-VOCAB-10](#833-custom-vocabulary) specifies the mitigation, and [§16.1](#161-q6-decision-ladder-pre-agreed-do-not-escalate) specifies exactly what to do at each measured failure rate. Follow the ladder; do not improvise a scope cut.

16. **Hotwords silently do nothing under `greedy_search`.** If a vocabulary is configured but the decoder is greedy, that is a bug, not a fallback. Assert the pairing.

17. **NeMo TDT hotwords need the model's `bpe.vocab` file**, which is a separate download from the ONNX weights. Its absence produces unbiased output with no error.

18. **Over-boosting is a real failure mode.** A high hotword score makes the model insert vocabulary terms into audio that does not contain them. Keep the default conservative and make it tunable.

### 13.8 Whisper vocabulary

19. **whisper.cpp has no hotwords parameter.** Requests for it are long-standing and open upstream. If you find a `hotwords` field while porting patterns from other tools, it came from faster-whisper or CTranslate2, which are different engines. whisper.cpp offers `initial_prompt` and GBNF grammar only.

20. **The Whisper prompt window is 224 tokens.** A long vocabulary will not fit. Prioritise and truncate explicitly, with a warning, per [FR-VOCAB-13](#833-custom-vocabulary).

21. **whisper.cpp GBNF grammar is reported unreliable** in upstream discussions. Do not build the vocabulary feature on it.

---

## 14. Milestones and exit criteria

Each milestone must fully satisfy its exit criteria before the next begins.

### M0 — Skeleton, signing, storage

Repository scaffold per [§6.2](#62-module-layout). ratatui shell with four panes and focus cycling. SQLite schema and migrations. Config load and validation. **In parallel from day one:** create the Developer ID certificate and push a hello-world binary through `codesign` and `notarytool`.

> **Do the notarization dry run first.** Newer individual Apple Developer enrollments have been hitting `Error 7000: Team is not yet configured for notarization`, taking days to clear. Discover that now, not at M3.

**Exit:** TUI launches and quits cleanly, restoring the terminal including on panic. Database created and migrated. A signed, notarized hello-world binary has passed `codesign -vvvv -R="notarized" --check-notarization`.

### M0.5 — Core recording vertical slice

Build the product's essential loop before the processing stack: the signed app captures all system audio except itself plus the default microphone, mixes them into one mono stream, writes a private 48 kHz signed 16-bit PCM `recording.wav`, and exposes start and clean stop through both CLI and TUI. This slice intentionally uses all-system capture and default settings so it is not blocked on per-process identity decisions or advanced controls.

Advanced gains, limiting, adaptive clock-drift correction, source meters, mute, auto-stop, per-process selection, exhaustive failure injection and long-duration tests remain in M3. The core slice must nevertheless use the same durable meeting folder and bounded callback-to-writer boundary so it can be hardened without replacement.

**Exit:** A signed build records a real five-minute meeting with system audio and the default microphone both audible in a valid mono 48 kHz signed 16-bit PCM WAV. CLI and TUI start and stop cleanly, the owned meeting folder and database row exist, and a checkpointed file remains readable if capture is interrupted.

### M1 — Transcription and diarization on existing files

`sosus transcribe <file>` and `sosus import <dir>`. symphonia decode, rubato resample, **both ASR backends** behind the `Transcriber` trait, sherpa-onnx diarization, overlap-based speaker assignment, custom vocabulary on both paths. Model download with SHA-256 verification and TUI progress. Markdown and JSON export.

This milestone carries the main technical risk: three FFI dependencies must build, link and run on Metal under a signed binary, and the Parakeet vocabulary path has a known upstream defect to characterise.

Order within the milestone: get Parakeet transcribing with `greedy_search` first, then diarization, then Whisper as the second backend, then vocabulary last. Vocabulary depends on knowing the answer to Q6, so measure before building the UI around it.

**Exit:** The complete source-controlled model manifest exists and every built-in download passes digest verification. A 30-minute meeting transcribes with correct speaker labels on both backends. The Q6 measurement is complete on the locked sherpa-onnx release across at least 20 real recordings, the matching rung of the [§16.1](#161-q6-decision-ladder-pre-agreed-do-not-escalate) ladder is implemented, and the measured rate is recorded in the repository. A vocabulary of at least 20 domain terms measurably improves recognition of those terms on whichever path the ladder selected. **Report Q1, Q3 and Q7 before starting M2.**

### M2 — Summarization, indexing, search and chat

llama-cpp-2 with context budgeting, chunking and map-reduce. Templates and display-title generation. Passage building, fastembed embeddings, FTS5 and sqlite-vec. Hybrid retrieval with RRF. Chat pane with streaming, citations and quote verification.

**Exit:** Chat answers a question about a meeting from three months ago with a valid passage marker that renders as a citation and jumps the Transcript pane to the right timestamp. A fabricated marker, a marker for a passage outside the supplied context, a partially fabricated long quote and an uncited factual sentence are all rejected or visibly marked. `reindex` rebuilds cleanly and passes FTS external-content integrity verification.

### M3 — Recording hardening

Harden the core recording slice with per-process Core Audio tap capture, adaptive source alignment, adjustable gains and peak limiting, source-specific activity monitoring, mute, silence auto-stop, failure recovery and exhaustive real-hardware coverage.

**Exit:** A real meeting records end to end with both sides audible in a valid 16-bit mono/48 kHz WAV. Changing each source gain measurably changes only that source in the next recording, and full-scale simultaneous test signals never exceed the `-1.0` dBFS limiter ceiling. System-only and microphone-only test signals animate only their respective source rows, while the `Recording` row reflects the written mix. Mute changes the microphone row to `MUTED` and removes it from the mixed signal; source clipping, limiter engagement, dropouts and microphone loss are visibly distinct. Auto-stop fires, and a `SIGKILL` at arbitrary points leaves a playable WAV through the most recent one-second checkpoint. A two-hour dual-clock fixture stays within 50 ms alignment. Permission revocation and microphone loss produce clear messages, never silent failure, and no sosus-owned aggregate device remains after the recovery test.

### M4 — Release engineering

Manually signed and notarized release binary and `.pkg`, immutable release tag and checksums, third-party Homebrew tap installing the signed upstream binary, retention policy and cleanup, Settings modal, help overlay, README, `SECURITY.md`, and complete third-party model notices.

**Exit:** A fresh Mac installs from the `.pkg`, grants permissions when prompted, changes a common setting in the TUI, and records a meeting without editing a file or changing a terminal setting. The saved TOML retains pre-existing comments and unknown keys, and an externally modified config is never overwritten. On a second fresh test account, `brew install <tap>/sosus` installs the same release and the installed executable passes signature verification, receives both required permissions, and records both system and microphone audio. Official Homebrew submission is a later distribution step, not an M4 exit criterion controlled by this project.

---

## 15. Anti-goals (do not build these)

Agents reliably over-build. These are prohibited in v1.

- **One diarizer, one LLM, no trait machinery for either.** These have a single implementation and no expectation of a second.
- **The `Transcriber` seam is the deliberate exception.** More ASR models are expected, so the trait, `BackendCapabilities` and the config selector stay even if v1 ships a single backend. Its boundaries are specified in [§8.3.1.2](#8312-what-the-seam-must-not-become) and they are tight: no registry, no dynamic loading, no auto-selection, no backend-specific concepts in `asr/mod.rs`. **Do not add a third backend in v1.**
- **No plugin system, scripting hooks or extension points.**
- **No cross-platform `cfg` branches.** macOS arm64 only. See [§2.2](#22-explicitly-out-of-scope-for-v1).
- **No cloud or remote backends**, including "just in case" HTTP client scaffolding.
- **No telemetry, analytics or crash reporting.**
- **No web UI, HTTP server or IPC surface.**
- **No streaming or real-time transcription.**
- **No model weights committed** to the repository, and none embedded in the binary.
- **No `unwrap()` or `expect()`** outside tests and `main.rs` startup assertions.
- **No new dependencies** beyond [§5](#5-technical-stack-pinned) without escalating first. Adding a crate is a decision, not an implementation detail.
- **No git commits, pushes or PRs** without explicit instruction from the human owner each time.
- **No silent scope reduction.** If a requirement cannot be met, say so explicitly with the reason. Do not quietly ship a narrower version.

---

## 16. Open questions (escalate, do not guess)

| # | Question | Resolve by | Fallback if it fails |
|---|---|---|---|
| Q1 | Does ggml's runtime Metal shader compilation via `newLibraryWithSource:` work under hardened runtime, or does it need `com.apple.security.cs.allow-jit`? No reports of a problem were found, and compilation happens in Apple's Metal compiler service rather than as in-process JIT, so it is expected to work. | End of M1 | Precompile `.metallib` in CI on a full-Xcode runner |
| Q2 | Which `sherpa-onnx` speaker embedding model gives the best accuracy-to-size ratio for meeting audio: a 3D-Speaker or a WeSpeaker ONNX export? | During M1 | Ship the smallest that clusters two speakers reliably, make it configurable |
| Q3 | Are Whisper's DTW token-level timestamps accurate enough for word-level speaker assignment? Moot on the Parakeet path, which emits word timings natively, so this only affects the Whisper fallback. Segment-level assignment ([FR-DIA-4](#84-diarization)) is unaffected either way. | During M1 | Derive word speakers from the containing segment |
| Q4 | Does `sherpa-onnx` static linking plus `fastembed` produce two copies of ONNX Runtime in the binary? If so, NFR-SIZE-1 needs revisiting. | During M2 | Take embeddings from `llama-cpp-2` instead and drop `fastembed` |
| Q5 | Is the default Phi-4-mini Q4 still the right summarization default in late 2026, or has a better small instruct model shipped in GGUF? | Before M2 | Keep Phi-4-mini, make it configurable, which it already is |
| Q6 | **What is the real failure rate of `modified_beam_search` on Parakeet TDT with our audio, on sherpa-onnx 1.13.5?** See the decision ladder in [§16.1](#161-q6-decision-ladder-pre-agreed-do-not-escalate) rather than escalating. | **During M1, before building the vocabulary UI** | Per ladder |
| Q7 | Does the Parakeet ONNX export in sherpa-onnx's model zoo match NVIDIA's published `parakeet-tdt-0.6b-v3` quality, and is a `bpe.vocab` published alongside it? The hotword feature depends on the latter. | During M1 | Use Whisper as the default backend until a suitable export exists |

### 16.1 Q6 decision ladder (pre-agreed, do not escalate)

**First, scope the defect correctly.** Upstream issue #3267 is open, but it affects `modified_beam_search` **only**. The reporter's failing files transcribe correctly 100% of the time under `greedy_search`. **Parakeet transcription itself has no known defect.** The confirmed reproduction was on sherpa-onnx v1.12.25; we target 1.13.5, released 2026-08-11, so re-measure rather than assuming the reported rate.

Measure on at least 20 real meeting recordings, at least 10 minutes each, comparing greedy against beam search with a 20-term vocabulary. Record the degenerate-output rate. Then take the matching rung, implement it, and note the choice in the repository. Do not stop and ask.

| Measured MBS degenerate rate | Decision | Result for the user |
|---|---|---|
| **Under 2%** | Parakeet default. Vocabulary via `ContextGraph`, with the [FR-VOCAB-10](#833-custom-vocabulary) retry mitigation retained as a safety net. | Everything as specified. |
| **2% to 10%** | Parakeet default. Vocabulary via `ContextGraph` **behind an explicit opt-in** (`vocabulary.strong_biasing = true`, default off) with the measured rate stated in the config comment and the TUI. Prompt-priming is not available on Parakeet, so vocabulary-off is the default experience. | Fast, accurate transcription. Strong vocabulary available with a documented caveat. |
| **Over 10%** | Parakeet default, `greedy_search` only. **Vocabulary supported on the Whisper backend only.** The TUI states that switching to Whisper enables vocabulary, and why. | Best ASR by default; users who need vocabulary switch backend, which config already supports. |
| **Parakeet unreliable under `greedy_search` too** (not currently indicated by any evidence) | Whisper becomes the default backend. Parakeet stays selectable and clearly marked experimental. | Vocabulary works out of the box; Parakeet available for speed. |

**Only if Parakeet proves unusable in greedy search** should it be removed from v1 entirely. In that case: **keep the `Transcriber` trait, `BackendCapabilities`, and the config selector.** Remove `asr/parakeet.rs` and its model entries, leave the seam and its tests, and record why in the repository so the next attempt starts from the measurement rather than repeating it.

Note what is *not* on this ladder: dropping Parakeet because beam search is buggy. That would trade the fastest and most accurate backend, its native word timestamps, and Swedish coverage, to work around a defect in one optional feature's decoder.

### 16.2 Product decisions required before implementation

These are owner decisions, not invitations for the implementing agent to choose. The recommendation is listed first. Once answered, replace each item with the selected normative requirement and return the document status to `Ready for implementation handoff`.

**D1 — Multilingual embedding model (before M2)**

- **A — `MultilingualE5Small` (recommended):** 384 dimensions, already supported by fastembed, keeps the current schema and is the smallest appropriate match for multilingual transcripts.
- **B — `MultilingualE5Base`:** 768 dimensions and a larger model for a likely retrieval-quality gain.
- **C — `BGE-M3`:** 1024 dimensions and the broadest retrieval capability, with materially greater download, memory and indexing cost.

**D2 — Archive search interaction (before M2)**

- **A — Global search overlay (recommended):** `/` opens hybrid passage search; results temporarily occupy the Meetings pane and selecting one opens its meeting at the timestamp. Typing while the Meetings pane is focused continues to filter meeting titles.
- **B — Separate keys:** `/` filters the focused pane and `s` opens archive passage search.
- **C — Chat only:** remove direct passage search and treat Chat as the only corpus-search surface. This narrows the stated Search capability.

**D3 — Recording WAV encoding (resolved)**

Store signed 16-bit PCM, mono, 48 kHz WAV. Mixing remains 32-bit float internally; quantize only after gain and limiting. The stored recording uses approximately 346 MB per hour.

**D4 — Mixing and silence controls (resolved)**

Use adjustable `system_gain_db` and `mic_gain_db`, both defaulting to `-3.0` dB, followed by a fixed `-1.0` dBFS peak limiter. Expose both gains in config and the Settings modal. Silence detection uses configurable `silence_threshold_dbfs`, default `-50.0`, on the final mix with two seconds of hysteresis. Do not add automatic gain control.

**D5 — Per-process capture identity (before M3)**

- **A — Bundle IDs plus one-shot PIDs (recommended):** persist application bundle identifiers, resolve their current processes and audio helpers when recording starts, and accept `--pid` for scripts.
- **B — Raw PIDs only:** retain the current config, accepting that values expire and may be reused.
- **C — TUI selection only:** choose from currently running applications for each recording and persist nothing.

**D6 — Meeting deletion interaction (before M4)**

- **A — `d` plus confirmation (recommended):** show the exact owned and derived files and total size; imported source audio is explicitly labelled “preserved”.
- **B — `D` plus confirmation:** harder to trigger accidentally, but less discoverable.
- **C — CLI-only deletion:** remove TUI deletion from v1 and use `sosus delete <meeting>`; this changes NFR-PRIV-6.

**D7 — Chat scope and citation controls (before M2)**

- **A — Explicit keys (recommended):** `S` toggles corpus/meeting scope, `[` and `]` select citations, and `Enter` jumps to the selected citation.
- **B — Contextual controls:** scope is a selectable header control; `Tab` reaches it and citations, with `Enter` activating them.
- **C — Automatic scope:** selected meeting always means meeting scope. Simpler, but makes corpus chat less predictable.

**D8 — Per-meeting vocabulary surface (before M1)**

- **A — Preflight editor plus CLI flags (recommended):** `v` in the Recording pane edits terms for the next recording; CLI commands accept repeatable `--term` and `--vocab-file`. Persist the effective vocabulary fingerprint with pipeline state, not the terms in logs.
- **B — CLI flags only:** per-meeting vocabulary works for scripted recording/import but not from the TUI.
- **C — Sidecar files:** automatically read `<media>.vocab.txt` on import, plus global vocabulary in the TUI.

**D9 — Standalone CLI persistence (before M1)**

- **A — Explicit separation (recommended):** `transcribe` and `summarize` are standalone and never modify the archive; `import` and `record` create meetings and run the configured full pipeline.
- **B — Archive by default:** every processing command creates or updates a meeting; add `--standalone` to avoid persistence.
- **C — Standalone by default with `--import`:** all file commands avoid the database unless explicitly requested.

**D10 — Portable artifact formats (before M1)**

- **A — Markdown always, JSON optional (recommended):** every archived meeting gets `transcript.md` and `summary.md`; replace `output.format` with `output.json = false` to optionally add `transcript.json`.
- **B — Exclusive format:** `output.format` selects Markdown or JSON and FR-EXP-1 no longer guarantees Markdown.
- **C — Always both:** remove the setting and write Markdown plus JSON for every meeting.

**D11 — Source-audio playback from citations (before M2)**

- **A — Explicitly out of scope (recommended):** citations jump to transcript timestamps and display the audio path when retained; revise “source audio” wording so it does not promise playback.
- **B — Built-in playback:** add play/pause from the cited timestamp when retained audio exists, accepting an additional native audio dependency and interaction work.
- **C — Open externally:** invoke the default media application and copy/display the timestamp; exact seeking is not guaranteed.

**D12 — Encryption at rest (resolved)**

Sosus adds no application-level encryption and does not require FileVault. It relies on macOS user-account isolation and restrictive filesystem permissions as specified by NFR-PRIV-7 and NFR-PRIV-9.

**D13 — Performance reference hardware (before M1 benchmarking)**

- **A — Current development Mac (recommended for reproducibility):** pin the exact Mac model, chip, core count, RAM and macOS build used for acceptance; report other Macs as observations, not promises.
- **B — Minimum baseline:** define targets against an M1 with 16 GB RAM, which is more meaningful commercially but requires access to that hardware.
- **C — Two tiers:** publish baseline and current-development-Mac results, doubling formal performance testing.

**D14 — LLM sampling policy (before M2)**

- **A — Fixed conservative sampling (recommended):** greedy title generation; temperature `0.2`, top-p `0.9`, top-k `40`, repeat penalty `1.1` for summary/reduce/chat; use the GGUF chat template and fail clearly if absent.
- **B — Fully greedy:** maximally repeatable but usually more brittle and repetitive for summaries.
- **C — User-configurable:** expose sampler controls in config, contrary to the current goal of avoiding inference-tuning surface.

**D15 — Directory import policy (before M1)**

- **A — Recursive and resilient (recommended):** recurse, do not follow symlinks, deduplicate by canonical path plus content digest, continue after per-file failures, print a final success/failure summary, and exit non-zero if any file failed.
- **B — Shallow:** process only direct children; otherwise the same behavior.
- **C — Fail fast:** stop on the first bad file, which is simpler but poor for archive backfills.

**D16 — ASR and diarization acceptance thresholds (before M1 exit)**

- **A — Corpus-relative gates (recommended):** establish a private representative corpus, record WER, domain-term recall and DER for the first accepted build, and reject later regressions greater than agreed tolerances; keep the raw recordings outside the repository.
- **B — Fixed universal thresholds:** choose absolute WER/DER numbers now, despite their strong dependence on language, speakers and audio quality.
- **C — Manual acceptance only:** retain “correct speaker labels” and “measurably improves” as human judgments, accepting that the milestone is not reproducible.

**D17 — Chat during pipeline inference (before M2)**

- **A — Serialize heavyweight inference (recommended):** retrieval and browsing remain available, but a chat question queues until the active ASR/diarization/summarization inference call completes. Lowest memory and FFI risk.
- **B — Run chat concurrently:** dedicate independent workers and enforce the RSS target with admission control. Faster interaction, more memory pressure and scheduling complexity.
- **C — User-configurable:** add a concurrency setting, which exposes an implementation concern most users should not need to manage.

**D18 — Official Homebrew channel (before submitting upstream; does not block M4)**

- **A — Third-party tap first, then evaluate official channels (recommended):** publish the signed and notarized upstream artifact through a project-owned tap. After release, test whether a source-built formula can establish a TCC identity that receives audio permissions; submit to `homebrew/core` only if it can.
- **B — `homebrew/core` is the hard target:** design and test a source build from the start. This is the normal location for an open-source CLI, but it is blocked unless a Homebrew-built executable can satisfy sosus's signing-dependent audio capture requirement.
- **C — `homebrew/cask` is the hard target:** install the signed upstream artifact, preserving its Developer ID identity. This better matches the technical requirement, but Homebrew normally expects open-source command-line-only software in `homebrew/core`, so cask acceptance is uncertain.

---

## 17. References

**macOS audio and permissions**
- [Capturing system audio with Core Audio taps (Apple)](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)
- [NSAudioCaptureUsageDescription (Apple)](https://developer.apple.com/documentation/bundleresources/information-property-list/nsaudiocaptureusagedescription)
- [Capturing System Audio on macOS in 2026 (DGR Labs)](https://dgrlabs.co/blog/2026-04-25-capturing-system-audio-on-macos-in-2026.html)
- [insidegui/AudioCap](https://github.com/insidegui/AudioCap) and [makeusabrew/audiotee](https://github.com/makeusabrew/audiotee)
- [Cap #1722: ScreenCaptureKit rejects ad-hoc signed binaries on Sequoia](https://github.com/CapSoftware/Cap/issues/1722)

**Signing and distribution**
- [Developer ID certificates (Apple)](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)
- [Notarizing macOS software (Apple)](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Notarization: the hardened runtime (Eclectic Light)](https://eclecticlight.co/2021/01/07/notarization-the-hardened-runtime/)
- [Building and notarizing command tools (Eclectic Light)](https://eclecticlight.co/2020/08/27/building-and-notarizing-command-tools-as-universal-binaries/)
- [Adding software to Homebrew](https://docs.brew.sh/Adding-Software-to-Homebrew)
- [Acceptable formulae](https://docs.brew.sh/Acceptable-Formulae) and [acceptable casks](https://docs.brew.sh/Acceptable-Casks)
- [Third-party Homebrew taps](https://docs.brew.sh/Taps)

**Crates**
- [ratatui](https://ratatui.rs/) and [async events tutorial](https://ratatui.rs/tutorials/counter-async-app/full-async-events/)
- [`toml_edit`](https://docs.rs/toml_edit/latest/toml_edit/)
- [whisper-rs](https://docs.rs/whisper-rs) · [llama-cpp-2](https://docs.rs/llama-cpp-2)
- [sherpa-onnx (official Rust API)](https://docs.rs/sherpa-onnx/latest/sherpa_onnx/) · [k2-fsa/sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)

**ASR backends and vocabulary**
- [nvidia/parakeet-tdt-0.6b-v3 (model card, language list)](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- [sherpa-onnx hotwords / contextual biasing](https://k2-fsa.github.io/sherpa/onnx/hotwords/index.html)
- [sherpa-onnx #3267: modified_beam_search with NeMo TDT hallucinates or returns empty ~20%](https://github.com/k2-fsa/sherpa-onnx/issues/3267)
- [sherpa-onnx #2541: why Parakeet cannot support hotwords (context on the bpe.vocab requirement)](https://github.com/k2-fsa/sherpa-onnx/issues/2541)
- [whisper.cpp #1979: using hotwords to bias transcription (open feature request)](https://github.com/ggml-org/whisper.cpp/issues/1979)
- [whisper.cpp #2003: flaky behaviour with grammars](https://github.com/ggml-org/whisper.cpp/discussions/2003)
- [objc2-core-audio](https://docs.rs/objc2-core-audio/latest/objc2_core_audio/)
- [sqlite-vec in Rust](https://alexgarcia.xyz/sqlite-vec/rust.html) · [fastembed-rs](https://github.com/Anush008/fastembed-rs)
- [ort linking guide](https://ort.pyke.io/setup/linking)
