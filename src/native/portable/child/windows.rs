use std::{
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{ChildErrorKind, MAX_STDOUT_BYTES, block_after_unconfirmed_cleanup};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CHILD_CLEANUP_WAIT: Duration = Duration::from_secs(2);
const READER_CLEANUP_WAIT: Duration = Duration::from_secs(1);

enum ReaderResult {
    Bytes(Vec<u8>),
    OutputLimit,
    Transport,
}

pub(super) fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, ChildErrorKind> {
    let deadline = Instant::now() + timeout;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = command.spawn().map_err(|_| ChildErrorKind::Spawn)?;
    let mut guard = DirectGuard {
        child,
        status: None,
        reader: None,
        receiver: None,
        reader_result: None,
        cleanup_complete: false,
    };
    let result = if let Some(stdout) = guard.child.stdout.take() {
        let (sender, receiver) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("diagnostic-stdout".to_owned())
            .spawn(move || {
                let mut bytes = Vec::new();
                let result = match stdout
                    .take((MAX_STDOUT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                {
                    Ok(_) if bytes.len() > MAX_STDOUT_BYTES => ReaderResult::OutputLimit,
                    Ok(_) => ReaderResult::Bytes(bytes),
                    Err(_) => ReaderResult::Transport,
                };
                let _ = sender.send(result);
            }) {
            Ok(reader) => {
                guard.reader = Some(reader);
                guard.receiver = Some(receiver);
                capture_loop(&mut guard, deadline)
            }
            Err(_) => Err(ChildErrorKind::Transport),
        }
    } else {
        Err(ChildErrorKind::Transport)
    };
    if guard.finish() {
        result
    } else {
        block_after_unconfirmed_cleanup();
        Err(ChildErrorKind::CleanupUnconfirmed)
    }
}

fn capture_loop(guard: &mut DirectGuard, deadline: Instant) -> Result<Vec<u8>, ChildErrorKind> {
    loop {
        if Instant::now() >= deadline {
            return Err(ChildErrorKind::TimedOut);
        }
        guard.poll_child().map_err(|_| ChildErrorKind::Transport)?;
        guard.poll_reader();
        match guard.reader_result.as_ref() {
            Some(ReaderResult::OutputLimit) => return Err(ChildErrorKind::OutputLimit),
            Some(ReaderResult::Transport) => return Err(ChildErrorKind::Transport),
            _ => {}
        }
        if let (Some(status), Some(ReaderResult::Bytes(_)), None) =
            (guard.status, guard.reader_result.as_ref(), &guard.reader)
        {
            if !status.success() {
                return Err(ChildErrorKind::Failed);
            }
            if let Some(ReaderResult::Bytes(bytes)) = guard.reader_result.take() {
                return Ok(bytes);
            }
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

struct DirectGuard {
    child: Child,
    status: Option<ExitStatus>,
    reader: Option<JoinHandle<()>>,
    receiver: Option<Receiver<ReaderResult>>,
    reader_result: Option<ReaderResult>,
    cleanup_complete: bool,
}

impl DirectGuard {
    fn poll_child(&mut self) -> io::Result<()> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(())
    }

    fn poll_reader(&mut self) {
        if self.reader.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(reader) = self.reader.take()
            && reader.join().is_err()
        {
            self.reader_result = Some(ReaderResult::Transport);
        }
        if self.reader_result.is_none()
            && let Some(receiver) = &self.receiver
        {
            match receiver.try_recv() {
                Ok(result) => self.reader_result = Some(result),
                Err(TryRecvError::Disconnected) => {
                    self.reader_result = Some(ReaderResult::Transport);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.reader.is_none() {
            self.receiver = None;
        }
    }

    fn finish(&mut self) -> bool {
        if self.cleanup_complete {
            return true;
        }
        let _ = self.poll_child();
        if self.status.is_none() {
            // A racing natural exit is resolved by the following wait.
            let _ = self.child.kill();
            let until = Instant::now() + CHILD_CLEANUP_WAIT;
            while self.status.is_none() {
                if self.poll_child().is_err() || Instant::now() >= until {
                    break;
                }
                thread::sleep(POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())));
            }
        }
        let until = Instant::now() + READER_CLEANUP_WAIT;
        while self.reader.is_some() {
            self.poll_reader();
            if self.reader.is_none() || Instant::now() >= until {
                break;
            }
            thread::sleep(POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())));
        }
        self.cleanup_complete = self.status.is_some() && self.reader.is_none();
        self.cleanup_complete
    }
}

impl Drop for DirectGuard {
    fn drop(&mut self) {
        if !self.cleanup_complete && !self.finish() {
            block_after_unconfirmed_cleanup();
        }
    }
}
