//! Conservative post-transcription vocabulary corrections.

use std::{fs, io, path::Path};

use regex::RegexBuilder;

use super::TranscriptResult;

#[derive(Debug, Default)]
pub struct Vocabulary {
    replacements: Vec<Replacement>,
}

#[derive(Debug)]
struct Replacement {
    canonical: String,
    pattern: regex::Regex,
}

impl Vocabulary {
    pub fn load(path: &Path) -> Result<Self, VocabularyError> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(VocabularyError::Read(error)),
        };

        let mut replacements = Vec::new();
        for (line_number, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (canonical, aliases) = line.split_once(':').ok_or_else(|| {
                VocabularyError::InvalidLine(line_number + 1, "expected `Canonical: alias, alias`")
            })?;
            let canonical = canonical.trim();
            if canonical.is_empty() {
                return Err(VocabularyError::InvalidLine(
                    line_number + 1,
                    "canonical term must not be empty",
                ));
            }
            let aliases = aliases
                .split(',')
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .collect::<Vec<_>>();
            if aliases.is_empty() {
                return Err(VocabularyError::InvalidLine(
                    line_number + 1,
                    "provide at least one mistaken form after `:`",
                ));
            }
            for alias in aliases {
                let pattern = RegexBuilder::new(&format!(r"\b{}\b", regex::escape(alias)))
                    .case_insensitive(true)
                    .build()
                    .map_err(|error| VocabularyError::Pattern(line_number + 1, error))?;
                replacements.push(Replacement {
                    canonical: canonical.to_owned(),
                    pattern,
                });
            }
        }
        Ok(Self { replacements })
    }

    pub fn apply(&self, transcript: &mut TranscriptResult) -> usize {
        let mut changes = 0;
        for segment in &mut transcript.segments {
            for replacement in &self.replacements {
                let matches = replacement.pattern.find_iter(&segment.text).count();
                if matches > 0 {
                    segment.text = replacement
                        .pattern
                        .replace_all(&segment.text, replacement.canonical.as_str())
                        .into_owned();
                    changes += matches;
                }
            }
        }
        changes
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VocabularyError {
    #[error("could not read vocabulary file: {0}")]
    Read(#[from] io::Error),
    #[error("invalid vocabulary on line {0}: {1}")]
    InvalidLine(usize, &'static str),
    #[error("invalid vocabulary pattern on line {0}: {1}")]
    Pattern(usize, regex::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::{Segment, TranscriptResult};

    #[test]
    fn replaces_only_configured_whole_terms_case_insensitively() {
        let root =
            std::env::temp_dir().join(format!("sosus-vocabulary-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("vocabulary.txt");
        fs::write(&file, "Asteron: Astaron\nNorthstar: North Star\n").unwrap();
        let vocabulary = Vocabulary::load(&file).unwrap();
        let mut transcript = TranscriptResult {
            language: "en".to_owned(),
            duration_seconds: 1.0,
            provenance: Default::default(),
            segments: vec![Segment {
                start_seconds: 0.0,
                end_seconds: 1.0,
                text: "ASTARON works with North Star, but Astaronic and North Stars.".to_owned(),
                words: Vec::new(),
                speaker: None,
            }],
        };

        assert_eq!(vocabulary.apply(&mut transcript), 2);
        assert_eq!(
            transcript.segments[0].text,
            "Asteron works with Northstar, but Astaronic and North Stars."
        );
        let _ = fs::remove_dir_all(root);
    }
}
