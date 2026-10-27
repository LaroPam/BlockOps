//! Retain the last stop result until a new server process starts.
use crate::process::ProcessStatus;
use std::collections::HashMap;
use tokio::sync::watch;

#[derive(Default)]
pub struct StopStates(HashMap<String, StopState>);

enum StopState {
    Stopping(watch::Receiver<ProcessStatus>),
    Finished(ProcessStatus),
}

impl StopStates {
    pub fn begin(&mut self, id: String, status: watch::Receiver<ProcessStatus>) {
        self.0.insert(id, StopState::Stopping(status));
    }

    pub fn finish(&mut self, id: String, status: ProcessStatus) {
        self.0.insert(id, StopState::Finished(status));
    }

    pub fn is_stopping(&self, id: &str) -> bool {
        matches!(self.0.get(id), Some(StopState::Stopping(_)))
    }

    pub fn clear(&mut self, id: &str) {
        self.0.remove(id);
    }

    pub fn status(&self, id: &str) -> Option<ProcessStatus> {
        match self.0.get(id)? {
            StopState::Stopping(status) => {
                let mut snapshot = status.borrow().clone();
                // The stop operation can still be closing its tunnel or saving the world.
                snapshot.phase = "stopping".into();
                Some(snapshot)
            }
            StopState::Finished(status) => Some(status.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_transition_retains_failure_and_log_until_cleared() {
        let mut states = StopStates::default();
        let (tx, rx) = watch::channel(ProcessStatus::default());
        states.begin("server".into(), rx);
        assert!(states.is_stopping("server"));
        assert_eq!(states.status("server").unwrap().phase, "stopping");
        tx.send_modify(|status| {
            status.phase = "failed".into();
            status.error = Some("world save failed".into());
            status.logs.push_back("last log".into());
        });
        let result = tx.borrow().clone();
        // The operation is still marked stopping until it atomically publishes the result.
        assert_eq!(states.status("server").unwrap().phase, "stopping");
        states.finish("server".into(), result);
        let result = states.status("server").unwrap();
        assert!(!states.is_stopping("server"));
        assert_eq!(result.phase, "failed");
        assert_eq!(result.error.as_deref(), Some("world save failed"));
        assert_eq!(result.logs.back().map(String::as_str), Some("last log"));
        assert!(states.status("other").is_none());
        states.clear("server");
        assert!(states.status("server").is_none());
    }

    #[test]
    fn graceful_stop_retains_stopped_state_and_log() {
        let mut states = StopStates::default();
        let status = ProcessStatus {
            phase: "stopped".into(),
            logs: ["saved".into()].into(),
            exit_code: Some(0),
            error: None,
        };
        states.finish("server".into(), status);
        let result = states.status("server").unwrap();
        assert_eq!(result.phase, "stopped");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.logs.back().map(String::as_str), Some("saved"));
    }
}
