# Test fixture policy

Public fixtures must be synthetic or have an explicit licence that permits redistribution. Real user recordings, private meeting content, the representative acceptance corpus, and model weights never belong in this repository.

Every media fixture must live in this directory and have an adjacent `<filename>.license.toml` file containing:

```toml
source = "How and where the fixture was produced"
license = "SPDX identifier or explicit redistribution terms"
sha256 = "64 lowercase hexadecimal characters"
redistributable = true
contains_private_data = false
synthetic = true
```

Prefer generating deterministic PCM fixtures at test runtime instead of storing media. Set `synthetic = false` only when the source and licence independently establish redistribution rights. Review the audio itself for private or identifying content before adding it. Keep fixtures short and purpose-specific, and update `sha256` whenever the file changes.

Tests that load model weights must be guarded by Cargo's `model-tests` feature. Default CI runs with `--no-default-features`, does not invoke model download code, and must remain useful without network access beyond fetching Rust dependencies and tools.
