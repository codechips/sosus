# sosus

A local-first macOS meeting recorder. sosus records system audio and an optional microphone, transcribes recordings locally, and can optionally diarize speakers. Recordings and exports remain in a portable filesystem archive.

## Name

The name nods to SOSUS, the historical *Sound Surveillance System*: a passive acoustic listening network. sosus applies that idea narrowly and locally—it records only when you start it, on your Mac, for your own meetings.

## Run

Requires macOS on Apple silicon, [Mise](https://mise.jdx.dev/), and the capture permissions macOS requests on first use.

```sh
mise run dev
```

Press `r` to start or stop recording. Use F2 for language, transcription model, diarization, and recording settings.

## Development

```sh
mise run ci
```

See [the product requirements](docs/prd.md), [release policy](docs/releases.md), and [third-party model notices](docs/third-party-models.md).

## Scope

sosus intentionally focuses on recording, transcription, and optional diarization. It does not include cloud services, summaries, search, chat, a database, or an LLM.
