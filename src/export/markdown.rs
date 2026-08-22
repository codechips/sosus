//! Markdown transcript and summary rendering.

use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::Path,
};

use crate::asr::TranscriptResult;

pub fn write_transcript(path: &Path, transcript: &TranscriptResult) -> io::Result<()> {
    let mut markdown = String::from("# Transcript\n\n");
    for segment in &transcript.segments {
        let speaker = segment.speaker.as_deref().unwrap_or("Unknown speaker");
        writeln!(
            markdown,
            "## [{} - {}] {speaker}\n\n{}\n",
            timestamp(segment.start_seconds),
            timestamp(segment.end_seconds),
            segment.text
        )
        .map_err(|_| io::Error::other("could not format transcript"))?;
    }

    let partial = path.with_file_name(format!(
        ".{}.partial",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("transcript.md")
    ));
    let _ = fs::remove_file(&partial);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&partial)?;
        file.write_all(markdown.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&partial, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn timestamp(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1_000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let seconds = (millis / 1_000) % 60;
    let millis = millis % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::asr::{Segment, Word};

    #[test]
    fn writes_atomic_speaker_labelled_markdown() {
        let root = std::env::temp_dir().join(format!(
            "sosus-transcript-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("transcript.md");
        let transcript = TranscriptResult {
            language: "en".to_owned(),
            duration_seconds: 1.25,
            segments: vec![Segment {
                start_seconds: 0.0,
                end_seconds: 1.25,
                text: "hello".to_owned(),
                words: vec![Word {
                    start_seconds: 0.0,
                    end_seconds: 1.0,
                    text: "hello".to_owned(),
                    score: 1.0,
                    speaker: Some("Speaker 1".to_owned()),
                }],
                speaker: Some("Speaker 1".to_owned()),
            }],
        };
        write_transcript(&path, &transcript).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("## [00:00:00.000 - 00:00:01.250] Speaker 1"));
        assert!(content.contains("hello"));
        assert!(!path.with_file_name(".transcript.md.partial").exists());
        let _ = fs::remove_dir_all(root);
    }
}
