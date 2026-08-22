//! Database row models and typed write inputs.

#[derive(Clone, Debug, PartialEq)]
pub struct Meeting {
    pub id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub duration_s: f64,
    pub language: String,
    pub audio_path: Option<String>,
    pub audio_owned: bool,
    pub source: String,
    pub speaker_count: i64,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewMeeting {
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub duration_s: f64,
    pub language: String,
    pub audio_path: Option<String>,
    pub audio_owned: bool,
    pub source: String,
    pub speaker_count: i64,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineStage {
    pub meeting_id: i64,
    pub stage: String,
    pub status: String,
    pub attempt: i64,
    pub input_fingerprint: String,
    pub implementation_id: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineStageUpdate {
    pub meeting_id: i64,
    pub stage: String,
    pub status: String,
    pub attempt: i64,
    pub input_fingerprint: String,
    pub implementation_id: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub id: i64,
    pub meeting_id: i64,
    pub idx: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub id: i64,
    pub segment_id: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
    pub score: f64,
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: i64,
    pub meeting_id: i64,
    pub template: String,
    pub body: String,
    pub model: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Passage {
    pub id: i64,
    pub meeting_id: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub speakers: String,
    pub text: String,
    pub token_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewPassage {
    pub meeting_id: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub speakers: String,
    pub text: String,
    pub token_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub id: i64,
    pub scope_meeting_id: Option<i64>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatTurn {
    pub id: i64,
    pub chat_id: i64,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatTurnSource {
    pub chat_turn_id: i64,
    pub meeting_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Citation {
    pub id: i64,
    pub chat_turn_id: i64,
    pub passage_id: i64,
    pub quote: String,
    pub verified: bool,
}
