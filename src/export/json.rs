//! Machine-readable transcript rendering.

use std::{fs, io, path::Path};

use crate::asr::TranscriptResult;

pub fn write_transcript(path: &Path, transcript: &TranscriptResult) -> io::Result<()> {
    let document = serde_json::json!({
        "language": transcript.language,
        "duration_seconds": transcript.duration_seconds,
        "segments": transcript.segments.iter().map(|segment| serde_json::json!({
            "start_seconds": segment.start_seconds,
            "end_seconds": segment.end_seconds,
            "speaker": segment.speaker,
            "text": segment.text,
            "words": segment.words.iter().map(|word| serde_json::json!({
                "start_seconds": word.start_seconds,
                "end_seconds": word.end_seconds,
                "text": word.text,
                "score": word.score,
                "speaker": word.speaker,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| io::Error::other(format!("could not encode transcript JSON: {error}")))?;
    let partial = path.with_file_name(format!(
        ".{}.partial",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("transcript.json")
    ));
    let _ = fs::remove_file(&partial);
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&partial)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&partial, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::asr::{Segment, Word};

    #[test]
    fn writes_round_trippable_json_without_a_partial_file() {
        let root = std::env::temp_dir().join(format!(
            "sosus-json-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("transcript.json");
        let transcript = TranscriptResult {
            language: "sv".to_owned(),
            duration_seconds: 1.0,
            segments: vec![Segment {
                start_seconds: 0.0,
                end_seconds: 1.0,
                speaker: Some("Speaker 1".to_owned()),
                text: "hej".to_owned(),
                words: vec![Word {
                    start_seconds: 0.0,
                    end_seconds: 1.0,
                    text: "hej".to_owned(),
                    score: 0.8,
                    speaker: Some("Speaker 1".to_owned()),
                }],
            }],
        };
        write_transcript(&path, &transcript).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["language"], "sv");
        assert_eq!(value["segments"][0]["speaker"], "Speaker 1");
        assert!(!path.with_file_name(".transcript.json.partial").exists());
        let _ = fs::remove_dir_all(root);
    }
}
