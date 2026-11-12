use std::sync::{
    Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, Ordering},
};

#[cfg(any(windows, test))]
use serde_json::Value;

use super::WorkerErrorKind;
#[cfg(any(windows, test))]
use super::protocol::Outcome;

pub(super) struct SupervisorGate {
    active: Mutex<()>,
    blocked: AtomicBool,
}

impl SupervisorGate {
    pub const fn new() -> Self {
        Self {
            active: Mutex::new(()),
            blocked: AtomicBool::new(false),
        }
    }

    pub fn acquire(&self) -> Result<MutexGuard<'_, ()>, WorkerErrorKind> {
        let guard = match self.active.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(WorkerErrorKind::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(WorkerErrorKind::SupervisionBlocked),
        };
        if self.blocked.load(Ordering::Acquire) {
            return Err(WorkerErrorKind::SupervisionBlocked);
        }
        Ok(guard)
    }

    pub fn block(&self) {
        self.blocked.store(true, Ordering::Release);
    }
}

#[derive(Default)]
#[cfg(any(windows, test))]
pub(super) struct Progress {
    pub send: Option<Result<(), WorkerErrorKind>>,
    pub response: Option<Result<Outcome, WorkerErrorKind>>,
    pub stderr: Option<Result<(), WorkerErrorKind>>,
    pub exit: Option<Result<u32, WorkerErrorKind>>,
}

#[cfg(any(windows, test))]
impl Progress {
    pub fn decide(
        &mut self,
        cancelled: bool,
        deadline_elapsed: bool,
    ) -> Option<Result<Value, WorkerErrorKind>> {
        if cancelled {
            return Some(Err(WorkerErrorKind::Cancelled));
        }
        if deadline_elapsed {
            return Some(Err(WorkerErrorKind::TimedOut));
        }
        if let Some(error) = [
            self.response
                .as_ref()
                .and_then(|result| result.as_ref().err()),
            self.stderr
                .as_ref()
                .and_then(|result| result.as_ref().err()),
            self.send.as_ref().and_then(|result| result.as_ref().err()),
            self.exit.as_ref().and_then(|result| result.as_ref().err()),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Some(Err(*error));
        }
        if self.send.is_none()
            || self.response.is_none()
            || self.stderr.is_none()
            || self.exit.is_none()
        {
            return None;
        }
        let outcome = match self.response.take() {
            Some(Ok(outcome)) => outcome,
            _ => unreachable!("checked above"),
        };
        if matches!(outcome, Outcome::CallbackFailed) {
            return Some(Err(WorkerErrorKind::Callback));
        }
        if matches!(outcome, Outcome::ResponseEncodingFailed) {
            return Some(Err(WorkerErrorKind::FrameLimit));
        }
        if self.exit != Some(Ok(0)) {
            return Some(Err(WorkerErrorKind::WorkerFailed));
        }
        match outcome {
            Outcome::Ok { payload } => Some(Ok(payload)),
            Outcome::CallbackFailed => unreachable!("checked above"),
            Outcome::ResponseEncodingFailed => unreachable!("checked above"),
        }
    }
}
