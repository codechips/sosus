# Third-party models

Sosus downloads model files on demand. Model weights are not included in the
source repository or application binary.

## Parakeet TDT 0.6B v3 (sherpa-onnx int8 export)

- Built-in alias: `parakeet-tdt-0.6b-v3-int8`
- Source: https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8
- Immutable revision: `2bda32ec70b097a55adaa07d9a7173915b43cc78`
- License: CC BY 4.0
- Attribution: NVIDIA Parakeet TDT 0.6B v3, exported for sherpa-onnx by the k2-fsa project
- Required files: `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`, `tokens.txt`

The exact file sizes, download URLs, redirect allowlist, and SHA-256 digests are
recorded in `models/manifest.toml`. The upstream export does not currently
publish the `bpe.vocab` companion required for contextual hotword biasing, so
sosus does not claim that feature for this model yet.

## Speaker diarization models (sherpa-onnx)

Sosus uses the official sherpa-onnx pairing of Pyannote segmentation 3.0 and
3D-Speaker ERes2Net embeddings. The segmentation export is MIT licensed; the
3D-Speaker model is distributed under Apache-2.0. The exact source revisions,
release asset identifier, sizes, URLs, and SHA-256 digests are pinned in
`models/manifest.toml`.

- Pyannote segmentation 3.0 int8: `pyannote-segmentation-3-0-int8`
- 3D-Speaker ERes2Net base SV: `3dspeaker-eres2net-base-sv-zh-cn`
