//! Resumable stage orchestration and progress events.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use tokio::sync::mpsc as tokio_mpsc;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Command {
    RunSynthetic { steps: u16, step_delay: Duration },
    Cancel,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppEvent {
    WorkStarted,
    WorkProgress { completed: u16, total: u16 },
    WorkCompleted,
    WorkCancelled,
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
}
