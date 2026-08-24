//! Filesystem-backed meeting archive.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use symphonia::core::{
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
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
        .filter(|entry| recording_path(&entry.path()).is_some())
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
    let recording =
        recording_path(&path).expect("recording existence was checked during discovery");
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

pub fn recording_path(meeting_dir: &Path) -> Option<PathBuf> {
    [
        "recording.wav",
        "recording.m4a",
        "recording.mp3",
        "recording.flac",
        "recording.ogg",
        "recording.mp4",
        "recording.m4v",
        "recording.mov",
    ]
    .into_iter()
    .map(|name| meeting_dir.join(name))
    .find(|path| path.is_file())
}

fn recording_duration_seconds(path: &Path) -> Option<f64> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
    {
        let reader = hound::WavReader::open(path).ok()?;
        let sample_rate = reader.spec().sample_rate;
        return (sample_rate > 0).then(|| reader.duration() as f64 / f64::from(sample_rate));
    }
    let source = MediaSourceStream::new(Box::new(fs::File::open(path).ok()?), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(path.extension()?.to_str()?);
    let format = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?
        .format;
    let track = format.default_track()?;
    let time = track
        .codec_params
        .time_base?
        .calc_time(track.codec_params.n_frames?);
    Some(time.seconds as f64 + time.frac)
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
    fn discovers_compacted_m4a_recordings() {
        let root = env::temp_dir().join(format!(
            "sosus-archive-m4a-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let meeting = root.join("meeting");
        fs::create_dir_all(&meeting).unwrap();
        fs::write(meeting.join("recording.m4a"), b"placeholder").unwrap();

        let meetings = discover(&root).unwrap();

        assert_eq!(meetings.len(), 1);
        assert_eq!(
            recording_path(&meetings[0].path),
            Some(meeting.join("recording.m4a"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_imported_media_recordings() {
        let root = env::temp_dir().join(format!(
            "sosus-archive-imported-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let meeting = root.join("meeting");
        fs::create_dir_all(&meeting).unwrap();
        fs::write(meeting.join("recording.mp4"), b"placeholder").unwrap();

        assert_eq!(
            recording_path(&meeting),
            Some(meeting.join("recording.mp4"))
        );
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
