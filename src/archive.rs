//! Filesystem-backed meeting archive.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Meeting {
    pub path: PathBuf,
    pub name: String,
    pub duration_seconds: Option<f64>,
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
        .filter(|entry| entry.path().join("recording.wav").is_file())
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
    let recording = path.join("recording.wav");
    Ok(Meeting {
        path,
        name,
        duration_seconds: recording_duration_seconds(&recording),
        // Discovery drives the sidebar. Transcript parsing is deferred until this
        // meeting is selected, so a large archive does not block each refresh.
        transcript: Vec::new(),
    })
}

pub fn load_transcript(meeting: &Meeting) -> io::Result<Vec<Segment>> {
    let path = meeting.path.join("transcript.md");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    parse_transcript(&fs::read_to_string(path)?)
}

fn recording_duration_seconds(path: &Path) -> Option<f64> {
    let reader = hound::WavReader::open(path).ok()?;
    let sample_rate = reader.spec().sample_rate;
    (sample_rate > 0).then(|| reader.duration() as f64 / f64::from(sample_rate))
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
    use std::{env, fs};

    use super::*;

    #[test]
    fn ignores_incomplete_meeting_folders() {
        let root = env::temp_dir().join(format!(
            "sosus-archive-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("incomplete")).unwrap();
        let complete = root.join("complete");
        fs::create_dir(&complete).unwrap();
        hound::WavWriter::create(
            complete.join("recording.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap()
        .finalize()
        .unwrap();

        let meetings = discover(&root).unwrap();

        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].name, "complete");
        fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn discovery_defers_transcript_parsing_until_selection() {
        let root = env::temp_dir().join(format!(
            "sosus-archive-lazy-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let meeting = root.join("meeting");
        fs::create_dir(&meeting).unwrap();
        hound::WavWriter::create(
            meeting.join("recording.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap()
        .finalize()
        .unwrap();
        fs::write(
            meeting.join("transcript.md"),
            "## [00:00:00.000 - 00:00:01.000] Unknown speaker\n\nHello\n",
        )
        .unwrap();

        let meetings = discover(&root).unwrap();
        assert!(meetings[0].transcript.is_empty());
        assert_eq!(load_transcript(&meetings[0]).unwrap()[0].text, "Hello");
        fs::remove_dir_all(root).unwrap();
    }
}
