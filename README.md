# sosus

A local-first macOS meeting recorder. sosus records system audio and an optional microphone, transcribes recordings locally, and can optionally diarize speakers. Recordings and exports remain in a portable filesystem archive.

## Name

The name nods to SOSUS, the historical *Sound Surveillance System*. Beginning in the 1950s, it used fixed arrays of underwater microphones to listen passively across long distances, taking advantage of the way low-frequency sound travels through the ocean. It became one of the defining acoustic systems of the Cold War, then evolved into the Integrated Undersea Surveillance System (IUSS). [The Navy's history of SOSUS](https://www.csp.navy.mil/cus/About-IUSS/Origins-of-SOSUS/) is a surprisingly good rabbit hole.

The connection is the idea of patient, local listening—not military surveillance. sosus records only when you start it, on your Mac, for your own meetings. No cloud, no hidden capture, and no database watching in the background.

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
