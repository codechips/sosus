# sosus

A local-first macOS meeting recorder. sosus records system audio and an optional microphone, transcribes recordings locally, and can optionally diarize speakers. Recordings and exports remain in a portable filesystem archive.

## Name

The name nods to SOSUS, the historical *Sound Surveillance System*. Beginning in the 1950s, it used fixed arrays of underwater microphones to listen passively across long distances, taking advantage of the way low-frequency sound travels through the ocean. It became one of the defining acoustic systems of the Cold War, then evolved into the Integrated Undersea Surveillance System (IUSS). [The Navy's history of SOSUS](https://www.csp.navy.mil/cus/About-IUSS/Origins-of-SOSUS/) is a surprisingly good rabbit hole.

The connection is the idea of patient, local listening—not military surveillance. sosus records only when you start it, on your Mac, for your own meetings. No cloud, no hidden capture, and no database watching in the background.

## Install

Every release ships a signed and notarized arm64 binary. Releases are currently
beta only, so `gh release view` and the `releases/latest` API endpoint do not
resolve; list the releases and take the newest instead.

```sh
tag=$(gh release list -R codechips/sosus --limit 1 --json tagName --jq '.[0].tagName')
gh release download "$tag" -R codechips/sosus -D .
shasum -a 256 -c sosus-*-macos-arm64.zip.sha256
unzip sosus-*-macos-arm64.zip
install -m 755 release/sosus ~/bin/sosus
```

Checksums from releases up to and including `2026.8.26-beta.1` record an
absolute build path and cannot be checked with `shasum -c`; for those, compare
the hash column by hand.

Confirm the signature before first use:

```sh
spctl -a -vvv -t install ~/bin/sosus
```

It should report `source=Notarized Developer ID`.

## Run

Requires macOS on Apple silicon, [Mise](https://mise.jdx.dev/), and the capture permissions macOS requests on first use.

```sh
mise run dev
```

Press `r` to start or stop recording. Use F2 for language, transcription model, diarization, and recording settings.

Press `?` in the TUI to see every shortcut. The essential controls are `r` to
record, `m` to mute the microphone while recording, `s` to cycle the expected
speaker count for the current recording, `t` to process a selected recording
or re-transcribe an existing transcript,
`o` to reveal it in Finder, and `d` to delete it with confirmation.

## CLI examples

Launch the TUI:

```sh
sosus
```

`tui` is an optional explicit subcommand: `sosus tui` is equivalent.

Record system audio and the default microphone until you press Ctrl+C:

```sh
sosus record
```

Transcribe an existing recording, explicitly using Whisper for Swedish:

```sh
sosus transcribe ~/Downloads/meeting.m4a --backend whisper --language sv
```

Diarization assumes two speakers by default. Override that for one invocation
when you know the expected count, or ask it to estimate:

```sh
sosus transcribe ~/Downloads/meeting.m4a --speakers 3
sosus transcribe ~/Downloads/meeting.m4a --speakers auto
```

Import an existing file into the meeting archive, or resume a meeting whose processing was interrupted. Add `--language sv` to `resume` when you need to override the configured language for that one transcription:

```sh
sosus import ~/Downloads/meeting.m4a
sosus resume ~/sosus/recordings/2026-08-22_1430
```

`import` copies the source into its new meeting folder before transcription, so
the archive remains resumable after the original file is moved or deleted.

Open the vocabulary dictionary with your default text editor:

```sh
sosus vocabulary
```

Add conservative corrections as `Canonical: mistaken form, another form`; they
are applied case-insensitively as whole terms when a transcript is exported.

## Storage and privacy

By default, recordings live in `~/sosus/recordings`. Configuration, downloaded
models, and redacted logs use the platform data directories; `--output-dir`,
`--data-dir`, and `--config` override them for a single invocation. All audio,
transcription, and diarization run locally. Model downloads occur only when a
selected model is not already present.

Owned recordings start as resilient PCM WAV files. In Settings, **Compact audio
to M4A** optionally converts a recording to a compact AAC/M4A file after its
transcript has been saved successfully; the original WAV remains if conversion
fails. Both formats can be transcribed again.

## Development

```sh
mise run ci
```

See [the product requirements](docs/prd.md), [release policy](docs/releases.md), and [third-party model notices](docs/third-party-models.md).

## Scope

sosus intentionally focuses on recording, transcription, and optional diarization. It does not include cloud services, summaries, search, chat, a database, or an LLM.
