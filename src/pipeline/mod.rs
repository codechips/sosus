//! Resumable stage orchestration and progress events.

// The state machine is introduced before the database adapter consumes it;
// keep its public transition surface intact while that adapter is added.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs, io,
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

const STAGE_ORDER: [Stage; 5] = [
    Stage::Transcribe,
    Stage::Diarize,
    Stage::Summarize,
    Stage::Export,
    Stage::Index,
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Transcribe,
    Diarize,
    Summarize,
    Export,
    Index,
}

impl Stage {
    pub const fn all() -> &'static [Self; 5] {
        &STAGE_ORDER
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StageState {
    pub stage: Stage,
    pub status: StageStatus,
    pub attempt: u32,
    pub input_fingerprint: String,
    pub implementation_id: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PipelineState {
    stages: Vec<StageState>,
}

impl PipelineState {
    pub const STATE_FILE: &'static str = ".pipeline-state.json";

    pub fn load(path: &Path) -> Result<Self, PipelinePersistenceError> {
        let bytes = fs::read(path.join(Self::STATE_FILE))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), PipelinePersistenceError> {
        let destination = path.join(Self::STATE_FILE);
        let temporary = path.join(format!(".pipeline-state.{}.partial", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, destination)?;
        Ok(())
    }
    pub fn new(skipped: &[Stage]) -> Self {
        Self {
            stages: STAGE_ORDER
                .into_iter()
                .map(|stage| StageState {
                    stage,
                    status: if skipped.contains(&stage) {
                        StageStatus::Skipped
                    } else {
                        StageStatus::Pending
                    },
                    attempt: 0,
                    input_fingerprint: String::new(),
                    implementation_id: String::new(),
                    started_at: None,
                    completed_at: None,
                    error_code: None,
                })
                .collect(),
        }
    }

    pub fn stage(&self, stage: Stage) -> Option<&StageState> {
        self.stages.iter().find(|state| state.stage == stage)
    }

    pub fn stages(&self) -> &[StageState] {
        &self.stages
    }

    pub fn recover_interrupted(&mut self) -> usize {
        let mut recovered = 0;
        for state in &mut self.stages {
            if state.status == StageStatus::Running {
                state.status = StageStatus::Failed;
                state.error_code = Some("interrupted".to_owned());
                state.completed_at = None;
                recovered += 1;
            }
        }
        recovered
    }

    pub fn first_resumable(&self) -> Option<Stage> {
        self.stages
            .iter()
            .find(|state| {
                matches!(
                    state.status,
                    StageStatus::Pending | StageStatus::Failed | StageStatus::Cancelled
                )
            })
            .map(|state| state.stage)
    }

    pub fn begin(
        &mut self,
        stage: Stage,
        input_fingerprint: &str,
        implementation_id: &str,
        started_at: &str,
    ) -> Result<(), PipelineError> {
        let index = stage_index(stage);
        if self.stages[..index]
            .iter()
            .any(|state| !matches!(state.status, StageStatus::Completed | StageStatus::Skipped))
        {
            return Err(PipelineError::UpstreamIncomplete { stage });
        }
        if self.stages[index].status == StageStatus::Running {
            return Err(PipelineError::AlreadyRunning { stage });
        }
        if !self.stages[index].input_fingerprint.is_empty()
            && self.stages[index].input_fingerprint != input_fingerprint
        {
            self.invalidate_downstream(stage);
        }
        let state = &mut self.stages[index];
        state.status = StageStatus::Running;
        state.attempt = state.attempt.saturating_add(1);
        state.input_fingerprint = input_fingerprint.to_owned();
        state.implementation_id = implementation_id.to_owned();
        state.started_at = Some(started_at.to_owned());
        state.completed_at = None;
        state.error_code = None;
        Ok(())
    }

    pub fn complete(&mut self, stage: Stage, completed_at: &str) -> Result<(), PipelineError> {
        let state = &mut self.stages[stage_index(stage)];
        if state.status != StageStatus::Running {
            return Err(PipelineError::NotRunning { stage });
        }
        state.status = StageStatus::Completed;
        state.completed_at = Some(completed_at.to_owned());
        Ok(())
    }

    pub fn skip(&mut self, stage: Stage) -> Result<(), PipelineError> {
        let state = &mut self.stages[stage_index(stage)];
        if state.status != StageStatus::Running {
            return Err(PipelineError::NotRunning { stage });
        }
        state.status = StageStatus::Skipped;
        state.completed_at = None;
        state.error_code = None;
        Ok(())
    }

    pub fn fail(&mut self, stage: Stage, error_code: &str) -> Result<(), PipelineError> {
        let state = &mut self.stages[stage_index(stage)];
        if state.status != StageStatus::Running {
            return Err(PipelineError::NotRunning { stage });
        }
        state.status = StageStatus::Failed;
        state.error_code = Some(error_code.to_owned());
        Ok(())
    }

    pub fn cancel(&mut self, stage: Stage) -> Result<(), PipelineError> {
        let state = &mut self.stages[stage_index(stage)];
        if state.status != StageStatus::Running {
            return Err(PipelineError::NotRunning { stage });
        }
        state.status = StageStatus::Cancelled;
        state.error_code = Some("cancelled".to_owned());
        Ok(())
    }

    pub fn invalidate_downstream(&mut self, stage: Stage) {
        let index = stage_index(stage);
        for state in &mut self.stages[index + 1..] {
            if state.status != StageStatus::Skipped {
                state.status = StageStatus::Pending;
                state.input_fingerprint.clear();
                state.implementation_id.clear();
                state.started_at = None;
                state.completed_at = None;
                state.error_code = None;
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipelinePersistenceError {
    #[error("could not access pipeline state: {0}")]
    Io(#[from] io::Error),
    #[error("could not parse pipeline state: {0}")]
    Json(#[from] serde_json::Error),
}

fn stage_index(stage: Stage) -> usize {
    match stage {
        Stage::Transcribe => 0,
        Stage::Diarize => 1,
        Stage::Summarize => 2,
        Stage::Export => 3,
        Stage::Index => 4,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PipelineError {
    #[error("cannot start {stage:?}: an upstream stage is incomplete")]
    UpstreamIncomplete { stage: Stage },
    #[error("pipeline stage {stage:?} is already running")]
    AlreadyRunning { stage: Stage },
    #[error("pipeline stage {stage:?} is not running")]
    NotRunning { stage: Stage },
}

#[derive(Debug)]
pub struct PipelineQueue<T> {
    pending: VecDeque<T>,
    active: bool,
}

impl<T> Default for PipelineQueue<T> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            active: false,
        }
    }
}

impl<T> PipelineQueue<T> {
    pub fn submit(&mut self, item: T) {
        self.pending.push_back(item);
    }

    pub fn start_next(&mut self) -> Option<T> {
        if self.active {
            return None;
        }
        let item = self.pending.pop_front()?;
        self.active = true;
        Some(item)
    }

    pub fn finish_active(&mut self) -> Result<(), PipelineQueueError> {
        if !self.active {
            return Err(PipelineQueueError::NothingActive);
        }
        self.active = false;
        Ok(())
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PipelineQueueError {
    #[error("pipeline queue has no active item")]
    NothingActive,
}

use tokio::sync::mpsc as tokio_mpsc;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Command {
    RunSynthetic { steps: u16, step_delay: Duration },
    Cancel,
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEvent {
    WorkStarted,
    Stage(String),
    WorkProgress { completed: u16, total: u16 },
    WorkCompleted,
    WorkCancelled,
    WorkFailed(String),
    WorkerStopped,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("failed to spawn pipeline worker: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("pipeline worker panicked during shutdown")]
    Join,
}

pub struct Worker {
    commands: Sender<Command>,
    events: tokio_mpsc::UnboundedReceiver<AppEvent>,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn() -> Result<Self, WorkerError> {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = tokio_mpsc::unbounded_channel();
        let join = thread::Builder::new()
            .name("sosus-pipeline".to_owned())
            .spawn(move || worker_loop(&command_rx, &event_tx))
            .map_err(WorkerError::Spawn)?;

        Ok(Self {
            commands,
            events,
            join: Some(join),
        })
    }

    pub async fn recv(&mut self) -> Option<AppEvent> {
        self.events.recv().await
    }

    pub fn shutdown(mut self) -> Result<(), WorkerError> {
        self.stop()
    }

    #[cfg(test)]
    fn send(&self, command: Command) -> Result<(), mpsc::SendError<Command>> {
        self.commands.send(command)
    }

    fn stop(&mut self) -> Result<(), WorkerError> {
        if self.join.is_none() {
            return Ok(());
        }

        let join = self.join.take().ok_or(WorkerError::Join)?;
        let _ = self.commands.send(Command::Shutdown);
        join.join().map_err(|_| WorkerError::Join)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn worker_loop(commands: &Receiver<Command>, events: &tokio_mpsc::UnboundedSender<AppEvent>) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::RunSynthetic { steps, step_delay } => {
                if run_synthetic_work(commands, events, steps, step_delay) {
                    break;
                }
            }
            Command::Cancel => {}
            Command::Shutdown => break,
        }
    }
    let _ = events.send(AppEvent::WorkerStopped);
}

fn run_synthetic_work(
    commands: &Receiver<Command>,
    events: &tokio_mpsc::UnboundedSender<AppEvent>,
    steps: u16,
    step_delay: Duration,
) -> bool {
    let _ = events.send(AppEvent::WorkStarted);

    for completed in 1..=steps {
        match commands.recv_timeout(step_delay) {
            Ok(Command::Cancel) => {
                let _ = events.send(AppEvent::WorkCancelled);
                return false;
            }
            Ok(Command::Shutdown) => return true,
            Ok(Command::RunSynthetic { .. }) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return true,
        }
        let _ = events.send(AppEvent::WorkProgress {
            completed,
            total: steps,
        });
    }

    let _ = events.send(AppEvent::WorkCompleted);
    false
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn cancellation_is_observed_during_long_work() {
        let mut worker = Worker::spawn().expect("worker should spawn");
        worker
            .send(Command::RunSynthetic {
                steps: 1_000,
                step_delay: Duration::from_millis(10),
            })
            .expect("work command should send");
        assert_eq!(worker.recv().await, Some(AppEvent::WorkStarted));

        let started = Instant::now();
        worker.send(Command::Cancel).expect("cancel should send");
        assert_eq!(worker.recv().await, Some(AppEvent::WorkCancelled));
        assert!(started.elapsed() < Duration::from_millis(100));

        worker.shutdown().expect("worker should join");
    }

    #[test]
    fn shutdown_joins_an_idle_worker() {
        Worker::spawn()
            .expect("worker should spawn")
            .shutdown()
            .expect("worker should join");
    }

    #[test]
    fn stage_state_recovers_running_work_and_retries_with_incremented_attempt() {
        let mut state = PipelineState::new(&[Stage::Summarize, Stage::Export, Stage::Index]);
        state
            .begin(Stage::Transcribe, "audio-a", "parakeet-a", "t0")
            .unwrap();
        assert_eq!(state.recover_interrupted(), 1);
        assert_eq!(
            state.stage(Stage::Transcribe).unwrap().status,
            StageStatus::Failed
        );
        assert_eq!(
            state
                .stage(Stage::Transcribe)
                .unwrap()
                .error_code
                .as_deref(),
            Some("interrupted")
        );
        state
            .begin(Stage::Transcribe, "audio-a", "parakeet-a", "t1")
            .unwrap();
        assert_eq!(state.stage(Stage::Transcribe).unwrap().attempt, 2);
        state.complete(Stage::Transcribe, "t2").unwrap();
        assert_eq!(state.first_resumable(), Some(Stage::Diarize));
    }

    #[test]
    fn changed_fingerprint_invalidates_only_downstream_completed_stages() {
        let mut state = PipelineState::new(&[]);
        for stage in [Stage::Transcribe, Stage::Diarize, Stage::Summarize] {
            state.begin(stage, "v1", "impl", "start").unwrap();
            state.complete(stage, "done").unwrap();
        }
        state.begin(Stage::Diarize, "v2", "impl", "retry").unwrap();
        assert_eq!(
            state.stage(Stage::Transcribe).unwrap().status,
            StageStatus::Completed
        );
        assert_eq!(
            state.stage(Stage::Diarize).unwrap().status,
            StageStatus::Running
        );
        assert_eq!(
            state.stage(Stage::Summarize).unwrap().status,
            StageStatus::Pending
        );
        assert_eq!(state.stage(Stage::Summarize).unwrap().input_fingerprint, "");
    }

    #[test]
    fn cancellation_and_skips_leave_a_resumable_next_stage() {
        let mut state = PipelineState::new(&[Stage::Summarize, Stage::Export, Stage::Index]);
        state
            .begin(Stage::Transcribe, "audio", "impl", "start")
            .unwrap();
        state.cancel(Stage::Transcribe).unwrap();
        assert_eq!(state.first_resumable(), Some(Stage::Transcribe));
        state
            .begin(Stage::Transcribe, "audio", "impl", "retry")
            .unwrap();
        state.complete(Stage::Transcribe, "done").unwrap();
        assert_eq!(state.first_resumable(), Some(Stage::Diarize));
        assert_eq!(
            state.stage(Stage::Summarize).unwrap().status,
            StageStatus::Skipped
        );
    }

    #[test]
    fn persisted_state_round_trips_and_recovers_interrupted_work() {
        let root = std::env::temp_dir().join(format!(
            "sosus-pipeline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut state = PipelineState::new(&[Stage::Summarize, Stage::Index]);
        state
            .begin(Stage::Transcribe, "audio", "asr", "start")
            .unwrap();
        state.save(&root).unwrap();
        let mut loaded = PipelineState::load(&root).unwrap();
        assert_eq!(loaded.recover_interrupted(), 1);
        assert_eq!(
            loaded.stage(Stage::Transcribe).unwrap().status,
            StageStatus::Failed
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn queue_is_fifo_and_allows_only_one_active_pipeline() {
        let mut queue = PipelineQueue::default();
        queue.submit("first");
        queue.submit("second");
        assert_eq!(queue.start_next(), Some("first"));
        assert_eq!(queue.start_next(), None);
        assert_eq!(queue.pending_len(), 1);
        queue.finish_active().unwrap();
        assert_eq!(queue.start_next(), Some("second"));
        assert!(matches!(queue.finish_active(), Ok(())));
        assert!(matches!(
            queue.finish_active(),
            Err(PipelineQueueError::NothingActive)
        ));
    }
}
