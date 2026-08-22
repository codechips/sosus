//! Filesystem-backed meeting archive.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Meeting {
    pub path: PathBuf,
    pub name: String,
    pub transcript: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start_s: f64,
    pub end_s: f64,
    pub speaker: Option<String>,
    pub text: String,
}

pub fn discover(root: &Path) -> io::Result<Vec<Meeting>> {
    let mut meetings = fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| load_meeting(entry.path()).ok())
        .collect::<Vec<_>>();
    meetings.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(meetings)
}

fn load_meeting(path: PathBuf) -> io::Result<Meeting> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("meeting")
        .to_owned();
    let transcript = path.join("transcript.md");
    let segments = if transcript.is_file() {
        parse_transcript(&fs::read_to_string(transcript)?)?
    } else {
        Vec::new()
    };
    Ok(Meeting {
        path,
        name,
        transcript: segments,
    })
}

fn parse_transcript(markdown: &str) -> io::Result<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut current: Option<Segment> = None;
    for line in markdown.lines() {
        if let Some(header) = line.strip_prefix("## [") {
            if let Some(segment) = current.take() {
                segments.push(segment);
            }
            let Some((range, speaker)) = header.split_once("] ") else {
                continue;
            };
            let Some((start, end)) = range.split_once(" - ") else {
                continue;
            };
            current = Some(Segment {
                start_s: parse_timestamp(start)?,
                end_s: parse_timestamp(end)?,
                speaker: (speaker != "Unknown speaker").then(|| speaker.to_owned()),
                text: String::new(),
            });
        } else if let Some(segment) = &mut current {
            let text = line.trim();
            if !text.is_empty() {
                if !segment.text.is_empty() {
                    segment.text.push(' ');
                }
                segment.text.push_str(text);
            }
        }
    }
    if let Some(segment) = current {
        segments.push(segment);
    }
    Ok(segments)
}

fn parse_timestamp(value: &str) -> io::Result<f64> {
    let mut parts = value.split(':');
    let hours = parts
        .next()
        .and_then(|part| part.parse::<f64>().ok())
        .ok_or_else(|| io::Error::other("invalid transcript timestamp"))?;
    let minutes = parts
        .next()
        .and_then(|part| part.parse::<f64>().ok())
        .ok_or_else(|| io::Error::other("invalid transcript timestamp"))?;
    let seconds = parts
        .next()
        .and_then(|part| part.parse::<f64>().ok())
        .ok_or_else(|| io::Error::other("invalid transcript timestamp"))?;
    Ok(hours * 3600.0 + minutes * 60.0 + seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exported_transcript_segments() {
        let transcript =
            "# Transcript\n\n## [00:00:01.000 - 00:00:02.500] Speaker 1\n\nHello there\n";
        let segments = parse_transcript(transcript).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start_s, 1.0);
        assert_eq!(segments[0].end_s, 2.5);
        assert_eq!(segments[0].speaker.as_deref(), Some("Speaker 1"));
        assert_eq!(segments[0].text, "Hello there");
    }
}
