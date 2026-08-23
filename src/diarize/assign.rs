//! Temporal-overlap speaker assignment.

use std::collections::HashMap;

use crate::asr::{Segment, TranscriptResult};

const MAX_INHERITED_GAP_SECONDS: f64 = 1.0;

#[derive(Clone, Debug, PartialEq)]
pub struct DiarizationTurn {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub cluster_id: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeakerAssignment {
    pub speaker_count: usize,
    pub labelled_segments: usize,
    pub labelled_words: usize,
}

pub fn assign_speakers(
    transcript: &mut TranscriptResult,
    turns: &[DiarizationTurn],
    assign_words: bool,
) -> SpeakerAssignment {
    let turns = valid_sorted_turns(turns);
    let mut segment_clusters = Vec::with_capacity(transcript.segments.len());
    let mut word_clusters = Vec::with_capacity(transcript.segments.len());
    let mut first_appearance = Vec::new();

    for segment in &transcript.segments {
        let cluster = speaker_for_span(segment.start_seconds, segment.end_seconds, &turns);
        remember_cluster(cluster, &mut first_appearance);
        segment_clusters.push(cluster);

        let clusters = if assign_words {
            segment
                .words
                .iter()
                .map(|word| {
                    let cluster = speaker_for_span(word.start_seconds, word.end_seconds, &turns);
                    remember_cluster(cluster, &mut first_appearance);
                    cluster
                })
                .collect()
        } else {
            Vec::new()
        };
        word_clusters.push(clusters);
    }

    let labels = speaker_labels(&turns, &first_appearance);
    let mut assignment = SpeakerAssignment {
        speaker_count: labels.len(),
        ..SpeakerAssignment::default()
    };

    for (index, segment) in transcript.segments.iter_mut().enumerate() {
        segment.speaker = segment_clusters[index].and_then(|cluster| labels.get(&cluster).cloned());
        assignment.labelled_segments += usize::from(segment.speaker.is_some());

        if assign_words {
            for (word, cluster) in segment.words.iter_mut().zip(&word_clusters[index]) {
                word.speaker = cluster
                    .as_ref()
                    .and_then(|cluster| labels.get(cluster).cloned());
                assignment.labelled_words += usize::from(word.speaker.is_some());
            }
        }
    }
    assignment
}

/// Splits timestamped ASR segments wherever the assigned speaker changes.
///
/// Some ASR backends emit long segments that contain several speakers. A
/// segment-level label necessarily hides those changes, so callers that have
/// assigned word labels should normalize the transcript with this function
/// before exporting it.
pub fn split_segments_by_speaker(transcript: &mut TranscriptResult) {
    let original_segments = std::mem::take(&mut transcript.segments);
    let mut segments = Vec::with_capacity(original_segments.len());

    for segment in original_segments {
        if segment.words.is_empty() {
            segments.push(segment);
            continue;
        }

        let fallback_speaker = segment.speaker.clone();
        let mut grouped = Vec::<Segment>::new();
        for word in segment.words {
            let speaker = word.speaker.clone().or_else(|| fallback_speaker.clone());
            if let Some(current) = grouped.last_mut()
                && current.speaker == speaker
            {
                current.end_seconds = word.end_seconds;
                current.text.push_str(&word.text);
                current.words.push(word);
            } else {
                grouped.push(Segment {
                    start_seconds: word.start_seconds,
                    end_seconds: word.end_seconds,
                    text: word.text.clone(),
                    words: vec![word],
                    speaker,
                });
            }
        }

        for segment in &mut grouped {
            segment.text = segment.text.trim().to_owned();
        }
        segments.extend(grouped);
    }

    transcript.segments = segments;
}

fn valid_sorted_turns(turns: &[DiarizationTurn]) -> Vec<&DiarizationTurn> {
    let mut turns = turns
        .iter()
        .filter(|turn| {
            turn.cluster_id >= 0
                && turn.start_seconds.is_finite()
                && turn.end_seconds.is_finite()
                && turn.start_seconds >= 0.0
                && turn.end_seconds > turn.start_seconds
        })
        .collect::<Vec<_>>();
    turns.sort_by(|left, right| {
        left.start_seconds
            .total_cmp(&right.start_seconds)
            .then_with(|| left.end_seconds.total_cmp(&right.end_seconds))
            .then_with(|| left.cluster_id.cmp(&right.cluster_id))
    });
    turns
}

fn remember_cluster(cluster: Option<i32>, first_appearance: &mut Vec<i32>) {
    if let Some(cluster) = cluster
        && !first_appearance.contains(&cluster)
    {
        first_appearance.push(cluster);
    }
}

fn speaker_labels(turns: &[&DiarizationTurn], first_appearance: &[i32]) -> HashMap<i32, String> {
    let mut labels = HashMap::new();
    for cluster in first_appearance.iter().copied().chain(
        turns
            .iter()
            .map(|turn| turn.cluster_id)
            .filter(|cluster| !first_appearance.contains(cluster)),
    ) {
        let next = labels.len() + 1;
        labels
            .entry(cluster)
            .or_insert_with(|| format!("Speaker {next}"));
    }
    labels
}

fn speaker_for_span(start: f64, end: f64, turns: &[&DiarizationTurn]) -> Option<i32> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return None;
    }

    let mut overlap_by_cluster: HashMap<i32, (f64, f64)> = HashMap::new();
    for turn in turns {
        let overlap = overlap_seconds(start, end, turn.start_seconds, turn.end_seconds);
        if overlap > 0.0 {
            let entry = overlap_by_cluster
                .entry(turn.cluster_id)
                .or_insert((0.0, turn.start_seconds));
            entry.0 += overlap;
            entry.1 = entry.1.min(turn.start_seconds);
        }
    }

    let overlapping_cluster = overlap_by_cluster
        .into_iter()
        .max_by(|(left_cluster, left), (right_cluster, right)| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| right.1.total_cmp(&left.1))
                .then_with(|| right_cluster.cmp(left_cluster))
        })
        .map(|(cluster, _)| cluster);
    if let Some(cluster) = overlapping_cluster {
        return Some(cluster);
    }

    turns
        .iter()
        .filter_map(|turn| {
            let gap = span_gap_seconds(start, end, turn.start_seconds, turn.end_seconds);
            (gap <= MAX_INHERITED_GAP_SECONDS).then_some((turn.cluster_id, gap, turn.start_seconds))
        })
        .min_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.2.total_cmp(&right.2))
                .then_with(|| left.0.cmp(&right.0))
        })
        .map(|(cluster, _, _)| cluster)
}

fn overlap_seconds(left_start: f64, left_end: f64, right_start: f64, right_end: f64) -> f64 {
    left_end.min(right_end) - left_start.max(right_start)
}

fn span_gap_seconds(left_start: f64, left_end: f64, right_start: f64, right_end: f64) -> f64 {
    if left_end <= right_start {
        right_start - left_end
    } else if right_end <= left_start {
        left_start - right_end
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use crate::asr::{Segment, TranscriptResult, Word};

    use super::*;

    fn transcript(spans: &[(f64, f64)]) -> TranscriptResult {
        TranscriptResult {
            language: "en".to_owned(),
            duration_seconds: 30.0,
            segments: spans
                .iter()
                .enumerate()
                .map(|(index, &(start_seconds, end_seconds))| Segment {
                    start_seconds,
                    end_seconds,
                    text: format!("segment {index}"),
                    words: vec![Word {
                        start_seconds,
                        end_seconds,
                        text: "word".to_owned(),
                        score: 0.0,
                        speaker: None,
                    }],
                    speaker: None,
                })
                .collect(),
        }
    }

    #[test]
    fn assigns_the_speaker_with_the_largest_total_overlap() {
        let mut transcript = transcript(&[(0.0, 10.0)]);
        let turns = vec![
            DiarizationTurn {
                start_seconds: 0.0,
                end_seconds: 3.0,
                cluster_id: 8,
            },
            DiarizationTurn {
                start_seconds: 3.0,
                end_seconds: 9.0,
                cluster_id: 4,
            },
            DiarizationTurn {
                start_seconds: 9.0,
                end_seconds: 10.0,
                cluster_id: 8,
            },
        ];

        let result = assign_speakers(&mut transcript, &turns, false);

        assert_eq!(result.speaker_count, 2);
        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("Speaker 1"));
        assert!(transcript.segments[0].words[0].speaker.is_none());
    }

    #[test]
    fn overlap_ties_choose_the_turn_that_started_first() {
        let mut transcript = transcript(&[(2.0, 6.0)]);
        let turns = vec![
            DiarizationTurn {
                start_seconds: 0.0,
                end_seconds: 4.0,
                cluster_id: 5,
            },
            DiarizationTurn {
                start_seconds: 4.0,
                end_seconds: 8.0,
                cluster_id: 2,
            },
        ];

        assign_speakers(&mut transcript, &turns, false);

        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn gaps_inherit_only_within_one_second() {
        let mut transcript = transcript(&[(3.5, 4.0), (6.1, 7.0)]);
        let turns = vec![DiarizationTurn {
            start_seconds: 1.0,
            end_seconds: 3.0,
            cluster_id: 9,
        }];

        assign_speakers(&mut transcript, &turns, false);

        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("Speaker 1"));
        assert!(transcript.segments[1].speaker.is_none());
    }

    #[test]
    fn labels_follow_first_appearance_not_cluster_id() {
        let mut transcript = transcript(&[(0.0, 1.0), (2.0, 3.0)]);
        let turns = vec![
            DiarizationTurn {
                start_seconds: 2.0,
                end_seconds: 3.0,
                cluster_id: 1,
            },
            DiarizationTurn {
                start_seconds: 0.0,
                end_seconds: 1.0,
                cluster_id: 99,
            },
        ];

        assign_speakers(&mut transcript, &turns, false);

        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("Speaker 1"));
        assert_eq!(transcript.segments[1].speaker.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn word_assignment_is_explicit_and_uses_the_same_rules() {
        let mut transcript = transcript(&[(0.0, 4.0)]);
        transcript.segments[0].words = vec![
            Word {
                start_seconds: 0.0,
                end_seconds: 1.0,
                text: "one".to_owned(),
                score: 0.0,
                speaker: None,
            },
            Word {
                start_seconds: 3.0,
                end_seconds: 4.0,
                text: "two".to_owned(),
                score: 0.0,
                speaker: None,
            },
        ];
        let turns = vec![
            DiarizationTurn {
                start_seconds: 0.0,
                end_seconds: 2.0,
                cluster_id: 10,
            },
            DiarizationTurn {
                start_seconds: 2.0,
                end_seconds: 4.0,
                cluster_id: 20,
            },
        ];

        let result = assign_speakers(&mut transcript, &turns, true);

        assert_eq!(result.labelled_words, 2);
        assert_eq!(
            transcript.segments[0].words[0].speaker.as_deref(),
            Some("Speaker 1")
        );
        assert_eq!(
            transcript.segments[0].words[1].speaker.as_deref(),
            Some("Speaker 2")
        );
    }

    #[test]
    fn splits_one_asr_segment_when_words_change_speaker() {
        let mut transcript = transcript(&[(0.0, 6.0)]);
        transcript.segments[0].words = vec![
            Word {
                start_seconds: 0.0,
                end_seconds: 2.0,
                text: " Hello".to_owned(),
                score: 0.0,
                speaker: None,
            },
            Word {
                start_seconds: 2.0,
                end_seconds: 3.0,
                text: " there".to_owned(),
                score: 0.0,
                speaker: None,
            },
            Word {
                start_seconds: 3.0,
                end_seconds: 5.0,
                text: " Hi".to_owned(),
                score: 0.0,
                speaker: None,
            },
            Word {
                start_seconds: 5.0,
                end_seconds: 6.0,
                text: " back".to_owned(),
                score: 0.0,
                speaker: None,
            },
        ];
        let turns = vec![
            DiarizationTurn {
                start_seconds: 0.0,
                end_seconds: 3.0,
                cluster_id: 10,
            },
            DiarizationTurn {
                start_seconds: 3.0,
                end_seconds: 6.0,
                cluster_id: 20,
            },
        ];

        assign_speakers(&mut transcript, &turns, true);
        split_segments_by_speaker(&mut transcript);

        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("Speaker 1"));
        assert_eq!(transcript.segments[0].text, "Hello there");
        assert_eq!(transcript.segments[0].start_seconds, 0.0);
        assert_eq!(transcript.segments[0].end_seconds, 3.0);
        assert_eq!(transcript.segments[1].speaker.as_deref(), Some("Speaker 2"));
        assert_eq!(transcript.segments[1].text, "Hi back");
        assert_eq!(transcript.segments[1].start_seconds, 3.0);
        assert_eq!(transcript.segments[1].end_seconds, 6.0);
    }
}
