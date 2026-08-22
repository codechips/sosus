# GitHub releases

Pushing a calendar-version tag such as `v2026.8.22-beta.1` runs the `Release` GitHub Actions workflow. It validates the tag against `Cargo.toml`, runs CI, builds the arm64 binary, signs it, submits it to Apple for notarization, verifies the result, creates the GitHub release, and uploads the ZIP plus SHA-256 checksum.

The workflow does not run from pull requests or `main`. A release tag is deliberate and immutable.

## Required repository secrets

Add these under **Settings → Secrets and variables → Actions** before pushing a release tag:

| Secret | Value |
| --- | --- |
| `MACOS_DEVELOPER_ID_APPLICATION_P12_BASE64` | Base64 encoding of the exported **Developer ID Application** certificate and private key (`.p12`). |
| `MACOS_DEVELOPER_ID_APPLICATION_P12_PASSWORD` | Password used when exporting that `.p12`. |
| `APPLE_NOTARY_KEY_P8_BASE64` | Base64 encoding of an App Store Connect API key (`.p8`) with notarization access. |
| `APPLE_NOTARY_KEY_ID` | App Store Connect API key ID. |
| `APPLE_NOTARY_ISSUER_ID` | App Store Connect issuer ID. |

The certificate and API key are decoded only on the ephemeral macOS runner. The workflow uses the automatically supplied `GITHUB_TOKEN` with `contents: write` solely to create the release and upload its assets.

## Cutting a release

1. Change `Cargo.toml` to the intended calendar version and commit it to `main`.
2. Confirm `mise run ci` passes.
3. Create and push the matching annotated tag, for example `v2026.8.22-beta.1`.
4. Wait for the `Release` workflow. It is the source of the published artifact and checksum.

For local recovery, use `SOSUS_NOTARY_PROFILE=<profile> mise run release`. The local and GitHub paths share the same build, signing, and verification script.
