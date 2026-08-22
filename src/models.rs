//! Pinned model metadata and verified, atomic model downloads.

#![allow(dead_code)]

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use reqwest::{StatusCode, Url, header::LOCATION};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

const MANIFEST_SOURCE: &str = include_str!("../models/manifest.toml");
const MAX_REDIRECTS: usize = 10;
const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    version: u32,
    models: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub alias: String,
    pub repository: String,
    pub revision: String,
    pub origin_url: String,
    pub license: String,
    pub attribution: String,
    pub redirect_hosts: Vec<String>,
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadProgress<'a> {
    pub model: &'a str,
    pub file: &'a str,
    pub file_bytes: u64,
    pub file_total: u64,
    pub model_bytes: u64,
    pub model_total: u64,
}

pub trait ModelProgressSink: Sync {
    fn report(&self, progress: DownloadProgress<'_>);
}

impl<F> ModelProgressSink for F
where
    F: for<'a> Fn(DownloadProgress<'a>) + Sync,
{
    fn report(&self, progress: DownloadProgress<'_>) {
        self(progress);
    }
}

pub fn manifest() -> Result<ModelManifest, ModelError> {
    let manifest: ModelManifest = toml::from_str(MANIFEST_SOURCE)?;
    manifest.validate()?;
    Ok(manifest)
}

impl ModelManifest {
    pub fn model(&self, alias: &str) -> Result<&ModelEntry, ModelError> {
        self.models
            .iter()
            .find(|model| model.alias == alias)
            .ok_or_else(|| ModelError::UnknownAlias {
                alias: alias.to_owned(),
                known: self
                    .models
                    .iter()
                    .map(|model| model.alias.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.version != 1 {
            return Err(ModelError::InvalidManifest(format!(
                "unsupported manifest version {}; expected 1",
                self.version
            )));
        }
        if self.models.is_empty() {
            return Err(ModelError::InvalidManifest(
                "manifest contains no models".to_owned(),
            ));
        }

        let mut aliases = HashSet::new();
        for model in &self.models {
            model.validate()?;
            if !aliases.insert(model.alias.as_str()) {
                return Err(ModelError::InvalidManifest(format!(
                    "duplicate model alias `{}`",
                    model.alias
                )));
            }
        }
        Ok(())
    }
}

impl ModelEntry {
    fn validate(&self) -> Result<(), ModelError> {
        if self.alias.is_empty()
            || !self.alias.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
            })
        {
            return Err(ModelError::InvalidManifest(format!(
                "invalid model alias `{}`",
                self.alias
            )));
        }
        if self.repository.split('/').count() != 2 {
            return Err(ModelError::InvalidManifest(format!(
                "model `{}` has invalid Hugging Face repository `{}`",
                self.alias, self.repository
            )));
        }
        if !is_lower_hex(&self.revision, 40) {
            return Err(ModelError::InvalidManifest(format!(
                "model `{}` revision is not an immutable 40-character commit",
                self.alias
            )));
        }
        if self.license.trim().is_empty() || self.attribution.trim().is_empty() {
            return Err(ModelError::InvalidManifest(format!(
                "model `{}` is missing license or attribution",
                self.alias
            )));
        }
        validate_https_url(&self.origin_url, &self.redirect_hosts)?;
        if self.redirect_hosts.is_empty() {
            return Err(ModelError::InvalidManifest(format!(
                "model `{}` has no permitted download hosts",
                self.alias
            )));
        }
        let mut hosts = HashSet::new();
        for host in &self.redirect_hosts {
            if host.is_empty()
                || host.bytes().any(|byte| {
                    !(byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'
                        || byte == b'.')
                })
                || !hosts.insert(host.as_str())
            {
                return Err(ModelError::InvalidManifest(format!(
                    "model `{}` has invalid or duplicate permitted host `{host}`",
                    self.alias
                )));
            }
        }
        if self.files.is_empty() {
            return Err(ModelError::InvalidManifest(format!(
                "model `{}` contains no files",
                self.alias
            )));
        }

        let mut filenames = HashSet::new();
        for file in &self.files {
            if Path::new(&file.filename).components().count() != 1
                || !matches!(
                    Path::new(&file.filename).components().next(),
                    Some(Component::Normal(_))
                )
            {
                return Err(ModelError::InvalidManifest(format!(
                    "model `{}` has unsafe filename `{}`",
                    self.alias, file.filename
                )));
            }
            if !filenames.insert(file.filename.as_str()) {
                return Err(ModelError::InvalidManifest(format!(
                    "model `{}` repeats filename `{}`",
                    self.alias, file.filename
                )));
            }
            if file.bytes == 0 || !is_lower_hex(&file.sha256, 64) {
                return Err(ModelError::InvalidManifest(format!(
                    "model `{}` file `{}` has invalid size or SHA-256",
                    self.alias, file.filename
                )));
            }
            let url = validate_https_url(&file.url, &self.redirect_hosts)?;
            if !url.path().contains(&self.revision) {
                return Err(ModelError::InvalidManifest(format!(
                    "model `{}` file `{}` URL does not contain its immutable revision",
                    self.alias, file.filename
                )));
            }
        }
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }
}

pub async fn ensure_model(
    alias: &str,
    model_root: &Path,
    progress: &dyn ModelProgressSink,
) -> Result<PathBuf, ModelError> {
    let manifest = manifest()?;
    let model = manifest.model(alias)?;
    let model_directory = model_root.join(&model.alias);
    fs::create_dir_all(&model_directory)
        .await
        .map_err(|source| ModelError::CreateDirectory {
            path: model_directory.clone(),
            source,
        })?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("sosus/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let model_total = model.total_bytes();
    let mut completed_bytes = 0;

    for file in &model.files {
        let destination = model_directory.join(&file.filename);
        if verify_file(&destination, file).await? {
            completed_bytes += file.bytes;
            progress.report(DownloadProgress {
                model: &model.alias,
                file: &file.filename,
                file_bytes: file.bytes,
                file_total: file.bytes,
                model_bytes: completed_bytes,
                model_total,
            });
            continue;
        }

        download_file(
            &client,
            model,
            file,
            &destination,
            completed_bytes,
            model_total,
            progress,
        )
        .await?;
        completed_bytes += file.bytes;
    }

    Ok(model_directory)
}

async fn download_file(
    client: &reqwest::Client,
    model: &ModelEntry,
    file: &ModelFile,
    destination: &Path,
    completed_bytes: u64,
    model_total: u64,
    progress: &dyn ModelProgressSink,
) -> Result<(), ModelError> {
    let partial = destination.with_file_name(format!("{}.partial", file.filename));
    remove_partial(&partial).await?;
    let result = download_to_partial(
        client,
        model,
        file,
        &partial,
        completed_bytes,
        model_total,
        progress,
    )
    .await;
    if let Err(error) = result {
        let _ = fs::remove_file(&partial).await;
        return Err(error);
    }

    fs::rename(&partial, destination)
        .await
        .map_err(|source| ModelError::Publish {
            from: partial,
            to: destination.to_path_buf(),
            source,
        })
}

async fn download_to_partial(
    client: &reqwest::Client,
    model: &ModelEntry,
    file: &ModelFile,
    partial: &Path,
    completed_bytes: u64,
    model_total: u64,
    progress: &dyn ModelProgressSink,
) -> Result<(), ModelError> {
    let mut url = validate_https_url(&file.url, &model.redirect_hosts)?;
    let mut response = None;
    for redirect_count in 0..=MAX_REDIRECTS {
        let candidate = client.get(url.clone()).send().await?;
        if candidate.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(ModelError::TooManyRedirects {
                    file: file.filename.clone(),
                });
            }
            let location = candidate
                .headers()
                .get(LOCATION)
                .ok_or_else(|| ModelError::RedirectWithoutLocation {
                    url: url.to_string(),
                })?
                .to_str()
                .map_err(|_| ModelError::InvalidRedirect {
                    location: "<non-UTF-8>".to_owned(),
                })?;
            url = url
                .join(location)
                .map_err(|_| ModelError::InvalidRedirect {
                    location: location.to_owned(),
                })?;
            validate_download_url(&url, &model.redirect_hosts)?;
            continue;
        }
        if candidate.status() != StatusCode::OK {
            return Err(ModelError::HttpStatus {
                url: url.to_string(),
                status: candidate.status(),
            });
        }
        response = Some(candidate);
        break;
    }
    let mut response = response.ok_or_else(|| ModelError::TooManyRedirects {
        file: file.filename.clone(),
    })?;
    if let Some(length) = response.content_length()
        && length != file.bytes
    {
        return Err(ModelError::SizeMismatch {
            file: file.filename.clone(),
            expected: file.bytes,
            actual: length,
        });
    }

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options
        .open(partial)
        .await
        .map_err(|source| ModelError::CreatePartial {
            path: partial.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut downloaded = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > file.bytes {
            return Err(ModelError::SizeMismatch {
                file: file.filename.clone(),
                expected: file.bytes,
                actual: downloaded,
            });
        }
        digest.update(&chunk);
        output
            .write_all(&chunk)
            .await
            .map_err(|source| ModelError::WritePartial {
                path: partial.to_path_buf(),
                source,
            })?;
        progress.report(DownloadProgress {
            model: &model.alias,
            file: &file.filename,
            file_bytes: downloaded,
            file_total: file.bytes,
            model_bytes: completed_bytes.saturating_add(downloaded),
            model_total,
        });
    }
    output
        .sync_all()
        .await
        .map_err(|source| ModelError::SyncPartial {
            path: partial.to_path_buf(),
            source,
        })?;
    drop(output);
    verify_size_and_digest(file, downloaded, digest.finalize().as_slice())
}

async fn verify_file(path: &Path, expected: &ModelFile) -> Result<bool, ModelError> {
    let metadata = match fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(ModelError::InspectFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() || metadata.len() != expected.bytes {
        return Ok(false);
    }

    let mut input = File::open(path)
        .await
        .map_err(|source| ModelError::InspectFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; DOWNLOAD_BUFFER_BYTES];
    loop {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|source| ModelError::InspectFile {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()) == expected.sha256)
}

async fn remove_partial(path: &Path) -> Result<(), ModelError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ModelError::RemovePartial {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn verify_size_and_digest(
    file: &ModelFile,
    actual_size: u64,
    actual_digest: &[u8],
) -> Result<(), ModelError> {
    if actual_size != file.bytes {
        return Err(ModelError::SizeMismatch {
            file: file.filename.clone(),
            expected: file.bytes,
            actual: actual_size,
        });
    }
    let actual = actual_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != file.sha256 {
        return Err(ModelError::DigestMismatch {
            file: file.filename.clone(),
            expected: file.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

fn validate_https_url(url: &str, hosts: &[String]) -> Result<Url, ModelError> {
    let url = Url::parse(url)
        .map_err(|_| ModelError::InvalidManifest(format!("invalid model URL `{url}`")))?;
    validate_download_url(&url, hosts)?;
    Ok(url)
}

fn validate_download_url(url: &Url, hosts: &[String]) -> Result<(), ModelError> {
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https" || !hosts.iter().any(|allowed| allowed == host) {
        return Err(ModelError::DisallowedHost {
            url: url.to_string(),
            allowed: hosts.join(", "),
        });
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("could not parse the embedded model manifest")]
    ParseManifest(#[from] toml::de::Error),
    #[error("invalid model manifest: {0}")]
    InvalidManifest(String),
    #[error("unknown model alias `{alias}`; known aliases: {known}")]
    UnknownAlias { alias: String, known: String },
    #[error(
        "model download URL `{url}` is not HTTPS or its host is not allowed; allowed hosts: {allowed}"
    )]
    DisallowedHost { url: String, allowed: String },
    #[error("model redirect from `{url}` did not provide a Location header")]
    RedirectWithoutLocation { url: String },
    #[error("invalid model redirect `{location}`")]
    InvalidRedirect { location: String },
    #[error("too many redirects while downloading `{file}`")]
    TooManyRedirects { file: String },
    #[error("model download from `{url}` returned HTTP {status}")]
    HttpStatus { url: String, status: StatusCode },
    #[error("model network request failed")]
    Request(#[from] reqwest::Error),
    #[error("could not create model directory {path}", path = .path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not inspect model file {path}", path = .path.display())]
    InspectFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not remove stale partial download {path}", path = .path.display())]
    RemovePartial {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not create partial download {path}", path = .path.display())]
    CreatePartial {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write partial download {path}", path = .path.display())]
    WritePartial {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not sync partial download {path}", path = .path.display())]
    SyncPartial {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model file `{file}` has {actual} bytes; expected {expected}")]
    SizeMismatch {
        file: String,
        expected: u64,
        actual: u64,
    },
    #[error("model file `{file}` has SHA-256 {actual}; expected {expected}")]
    DigestMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("could not atomically publish {from} as {to}", from = .from.display(), to = .to.display())]
    Publish {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_is_complete_and_pinned_for_parakeet() {
        let manifest = manifest().unwrap();
        let model = manifest.model("parakeet-tdt-0.6b-v3-int8").unwrap();
        assert_eq!(model.revision.len(), 40);
        assert_eq!(model.files.len(), 4);
        assert_eq!(model.total_bytes(), 670_478_772);
        assert!(model.files.iter().any(|file| file.filename == "tokens.txt"));
    }

    #[test]
    fn rejects_redirects_outside_the_exact_allowlist() {
        let hosts = vec!["huggingface.co".to_owned()];
        let error = validate_download_url(
            &Url::parse("https://attacker.example/model.onnx").unwrap(),
            &hosts,
        )
        .unwrap_err();
        assert!(matches!(error, ModelError::DisallowedHost { .. }));
        assert!(
            validate_download_url(
                &Url::parse("https://huggingface.co/model.onnx").unwrap(),
                &hosts
            )
            .is_ok()
        );
    }

    #[test]
    fn digest_and_size_mismatches_are_hard_failures() {
        let file = ModelFile {
            filename: "weights.onnx".to_owned(),
            url: "https://huggingface.co/weights.onnx".to_owned(),
            sha256: "00".repeat(32),
            bytes: 3,
        };
        assert!(matches!(
            verify_size_and_digest(&file, 2, &[0; 32]),
            Err(ModelError::SizeMismatch { .. })
        ));
        assert!(matches!(
            verify_size_and_digest(&file, 3, &[1; 32]),
            Err(ModelError::DigestMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn stale_partial_is_removed_without_touching_complete_file() {
        let root = std::env::temp_dir().join(format!(
            "sosus-model-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir(&root).await.unwrap();
        let complete = root.join("weights.onnx");
        let partial = root.join("weights.onnx.partial");
        fs::write(&complete, b"complete").await.unwrap();
        fs::write(&partial, b"partial").await.unwrap();

        remove_partial(&partial).await.unwrap();

        assert!(!partial.exists());
        assert_eq!(fs::read(&complete).await.unwrap(), b"complete");
        fs::remove_dir_all(root).await.unwrap();
    }
}
