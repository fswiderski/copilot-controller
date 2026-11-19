use serde::{Deserialize, Serialize};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use serde_json::Value;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::collections::VecDeque;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::ffi::{CStr, CString, c_char, c_void};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::io::{BufRead, BufReader, Write};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::ptr::{null, null_mut};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::sync::mpsc;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::thread;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::thread::sleep;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::time::{Duration, Instant};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use copilot_controller::devices::micro::{
    enumeration::{APPLICATION_PID, APPLICATION_VID, REPORT_SIZE, USAGE, USAGE_PAGE},
    probe::verify_micro_descriptor,
    wire::{Decoder, encode_json},
};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use copilot_controller::native::hid::{HidBus, HidDescriptor, HidError, HidIdentity, HidPath};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use serde_json::json;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("micro_hid_contention_lab is macOS ARM64-only");
    std::process::exit(2);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() {
    if let Err(error) = run_main(std::env::args().collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const IPC_MAX_MESSAGE_BYTES: usize = 8 * 1024;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const IPC_RECV_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const HELPER_MAX_LIFETIME: Duration = Duration::from_secs(30);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const HELPER_IDLE_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const LABEL_MAX_BYTES: usize = 64;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const QUERY_TIMEOUT_MS_MAX: u64 = 10_000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CHILD_WAIT_AFTER_CLOSE: Duration = Duration::from_secs(2);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const FIRST_LAB_REQUEST_ID: u64 = 10_001;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_main(args: Vec<String>) -> Result<(), String> {
    match args.as_slice() {
        [_] => run_parent(),
        [_, mode, open] if mode == "--helper" => run_helper(open),
        [_, mode, case] if mode == "--fixture" => run_fixture(case),
        _ => Err(
            "usage: micro_hid_contention_lab [--helper <normal|seize>] [--fixture <name>]".into(),
        ),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_parent() -> Result<(), String> {
    let mut request_ids = RequestIdAllocator::new(FIRST_LAB_REQUEST_ID);
    let report = MatrixReport {
        scenario_existing_client_then_seize: run_existing_then_seize(&mut request_ids)?,
        scenario_seize_then_new_client: run_seize_then_new(&mut request_ids)?,
        limitations: "HID-only lab signal; no raw-USB challenge, no persistent keymap/config/firmware writes.",
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "serialization failed")?
    );
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_existing_then_seize(request_ids: &mut RequestIdAllocator) -> Result<ScenarioResult, String> {
    let mut scenario = ScenarioResult {
        name: "existing_client_then_seize".into(),
        ..Default::default()
    };
    let mut b = ChildConn::spawn("normal")?;
    scenario.b_open = Some(b.opened.clone());
    if !b.opened.acquired {
        return Ok(scenario);
    }
    let b_pre_id = request_ids.next()?;
    scenario.b_pre_query = Some(b.query_with_known_id("b_pre", b_pre_id, 2_000)?);
    let mut a = ChildConn::spawn("seize")?;
    scenario.a_open = Some(a.opened.clone());
    if !a.opened.acquired {
        b.close()?;
        return Ok(scenario);
    }
    let during_id = request_ids.next()?;
    a.start_observe("a_observe_b_during", during_id, 2_000)?;
    scenario.b_during_query = Some(b.query_with_known_id("b_during", during_id, 2_000)?);
    scenario.a_during_query = Some(a.finish_observe("a_observe_b_during", during_id)?);
    scenario.a_close = Some(a.close()?);
    let b_after_id = request_ids.next()?;
    scenario.b_after_query = Some(b.query_with_known_id("b_after", b_after_id, 2_000)?);
    scenario.b_close = Some(b.close()?);
    Ok(scenario)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_seize_then_new(request_ids: &mut RequestIdAllocator) -> Result<ScenarioResult, String> {
    let mut scenario = ScenarioResult {
        name: "seize_then_new_client".into(),
        ..Default::default()
    };
    let mut a = ChildConn::spawn("seize")?;
    scenario.a_open = Some(a.opened.clone());
    if !a.opened.acquired {
        return Ok(scenario);
    }
    let mut b = ChildConn::spawn("normal")?;
    scenario.b_open = Some(b.opened.clone());
    if b.opened.acquired {
        let b_during_id = request_ids.next()?;
        scenario.b_during_query = Some(b.query_with_known_id("b_during", b_during_id, 2_000)?);
        scenario.b_close = Some(b.close()?);
    }
    scenario.a_close = Some(a.close()?);
    let mut after = ChildConn::spawn("normal")?;
    scenario.b_after_open = Some(after.opened.clone());
    if after.opened.acquired {
        let b_after_id = request_ids.next()?;
        scenario.b_after_query = Some(after.query_with_known_id("b_after", b_after_id, 2_000)?);
        scenario.b_after_close = Some(after.close()?);
    }
    Ok(scenario)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_fixture(case: &str) -> Result<(), String> {
    match case {
        "init_hang" => {
            sleep(Duration::from_secs(60));
            Ok(())
        }
        "close_ack_hang" => {
            println!(
                "{}",
                serde_json::to_string(&ToParent::Opened(Opened {
                    mode: "fixture".into(),
                    acquired: true,
                    open_code: 0,
                    close_code: None,
                    error: None,
                }))
                .map_err(|_| "serialize failed")?
            );
            std::io::stdout().flush().map_err(|_| "flush failed")?;
            let mut reader = BufReader::new(std::io::stdin().lock());
            let Some(line) = read_bounded_line(&mut reader, IPC_MAX_MESSAGE_BYTES)? else {
                return Ok(());
            };
            let command: FromParent =
                serde_json::from_str(&line).map_err(|_| "invalid fixture command")?;
            if matches!(command, FromParent::Close) {
                println!(
                    "{}",
                    serde_json::to_string(&ToParent::Closed(CloseResult { close_code: 0 }))
                        .map_err(|_| "serialize failed")?
                );
                std::io::stdout().flush().map_err(|_| "flush failed")?;
                sleep(Duration::from_secs(60));
            }
            Ok(())
        }
        "watchdog_idle" => {
            start_watchdog(HELPER_MAX_LIFETIME);
            sleep(Duration::from_secs(60));
            Ok(())
        }
        "eof_exit" => {
            let mut reader = BufReader::new(std::io::stdin().lock());
            while read_bounded_line(&mut reader, IPC_MAX_MESSAGE_BYTES)?.is_some() {}
            Ok(())
        }
        _ if case.starts_with("watchdog_idle_ms:") => {
            let millis = case
                .split_once(':')
                .and_then(|(_, value)| value.parse::<u64>().ok())
                .filter(|value| *value > 0 && *value <= 2_000)
                .ok_or("invalid watchdog fixture timeout")?;
            start_watchdog(Duration::from_millis(millis));
            sleep(Duration::from_secs(60));
            Ok(())
        }
        _ => Err("unknown fixture".into()),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct RequestIdAllocator {
    next: u64,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl RequestIdAllocator {
    fn new(first: u64) -> Self {
        Self { next: first }
    }

    fn next(&mut self) -> Result<u64, String> {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("request id sequence exhausted")?;
        Ok(id)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_helper(open_mode: &str) -> Result<(), String> {
    start_watchdog(HELPER_MAX_LIFETIME);
    let mode = match open_mode {
        "normal" => OpenMode::Normal,
        "seize" => OpenMode::Seize,
        _ => return Err("helper mode must be normal|seize".into()),
    };
    let mut session = match HidSession::open(mode) {
        Ok(session) => session,
        Err(error) => {
            println!(
                "{}",
                serde_json::to_string(&ToParent::Opened(Opened {
                    mode: mode.name().into(),
                    acquired: false,
                    open_code: error.open_code.unwrap_or(-1),
                    close_code: None,
                    error: Some(error.message),
                }))
                .map_err(|_| "serialize failed")?
            );
            return Ok(());
        }
    };
    println!(
        "{}",
        serde_json::to_string(&ToParent::Opened(Opened {
            mode: mode.name().into(),
            acquired: true,
            open_code: 0,
            close_code: None,
            error: None,
        }))
        .map_err(|_| "serialize failed")?
    );
    std::io::stdout().flush().map_err(|_| "flush failed")?;
    let (tx, rx) = mpsc::channel::<Result<Option<String>, String>>();
    thread::spawn(move || {
        let input = std::io::stdin();
        let mut reader = BufReader::new(input.lock());
        loop {
            match read_bounded_line(&mut reader, IPC_MAX_MESSAGE_BYTES) {
                Ok(Some(line)) => {
                    if tx.send(Ok(Some(line))).is_err() {
                        break;
                    }
                }

                Ok(None) => {
                    let _ = tx.send(Ok(None));
                    break;
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        }
    });
    loop {
        let line = match rx.recv_timeout(HELPER_IDLE_TIMEOUT) {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let command: FromParent = serde_json::from_str(&line).map_err(|_| "invalid command")?;
        match command {
            FromParent::Query {
                label,
                request_id,
                timeout_ms,
            } => {
                validate_query_command(&label, timeout_ms)?;
                let result =
                    session.query_version(&label, request_id, Duration::from_millis(timeout_ms));
                println!(
                    "{}",
                    serde_json::to_string(&ToParent::Query(result))
                        .map_err(|_| "serialize failed")?
                );
                std::io::stdout().flush().map_err(|_| "flush failed")?;
            }
            FromParent::Observe {
                label,
                timeout_ms,
                expected_request_id,
            } => {
                validate_query_command(&label, timeout_ms)?;
                let result = session.observe_response(
                    &label,
                    expected_request_id,
                    Duration::from_millis(timeout_ms),
                );
                println!(
                    "{}",
                    serde_json::to_string(&ToParent::Query(result))
                        .map_err(|_| "serialize failed")?
                );
                std::io::stdout().flush().map_err(|_| "flush failed")?;
            }
            FromParent::Close => {
                let close_code = session.close();
                println!(
                    "{}",
                    serde_json::to_string(&ToParent::Closed(CloseResult { close_code }))
                        .map_err(|_| "serialize failed")?
                );
                std::io::stdout().flush().map_err(|_| "flush failed")?;
                return Ok(());
            }
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn start_watchdog(timeout: Duration) {
    thread::spawn(move || {
        thread::sleep(timeout);
        std::process::exit(124);
    });
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Default, Serialize)]
struct MatrixReport {
    scenario_existing_client_then_seize: ScenarioResult,
    scenario_seize_then_new_client: ScenarioResult,
    limitations: &'static str,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Default, Serialize)]
struct ScenarioResult {
    name: String,
    a_open: Option<Opened>,
    b_open: Option<Opened>,
    b_pre_query: Option<QueryResult>,
    b_during_query: Option<QueryResult>,
    a_during_query: Option<QueryResult>,
    a_close: Option<CloseResult>,
    b_close: Option<CloseResult>,
    b_after_open: Option<Opened>,
    b_after_query: Option<QueryResult>,
    b_after_close: Option<CloseResult>,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Opened {
    mode: String,
    acquired: bool,
    open_code: i64,
    close_code: Option<i64>,
    error: Option<String>,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryResult {
    label: String,
    request_id: Option<u64>,
    native_write_code: i64,
    native_write_ok: bool,
    write_attempted: bool,
    correlated_response: bool,
    version: Option<String>,
    error: Option<String>,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CloseResult {
    close_code: i64,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum FromParent {
    Query {
        label: String,
        request_id: u64,
        timeout_ms: u64,
    },
    Observe {
        label: String,
        timeout_ms: u64,
        expected_request_id: u64,
    },
    Close,
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum ToParent {
    Opened(Opened),
    Query(QueryResult),
    Closed(CloseResult),
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct ChildConn {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<ReaderEvent>,
    opened: Opened,
    seen_query_ids: Vec<u64>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl ChildConn {
    fn spawn(mode: &str) -> Result<Self, String> {
        Self::spawn_with_args(&["--helper", mode], IPC_RECV_TIMEOUT)
    }

    #[cfg(test)]
    fn spawn_fixture(case: &str, open_timeout: Duration) -> Result<Self, String> {
        let mut cmd = Self::fixture_test_command(case)?;
        let child = cmd.spawn().map_err(|_| "spawn failed")?;
        let mut guard = ChildInitGuard::new(child);
        let stdin = guard
            .child_mut()?
            .stdin
            .take()
            .ok_or("child stdin missing")?;
        let stdout = guard
            .child_mut()?
            .stdout
            .take()
            .ok_or("child stdout missing")?;
        let rx = Self::spawn_reader(stdout, true);
        let opened = match recv_reader_event(&rx, open_timeout) {
            Ok(ToParent::Opened(opened)) => opened,
            Ok(ToParent::Query(_) | ToParent::Closed(_)) => {
                return guard.fail("fixture sent unexpected event before open");
            }
            Err(error) => return guard.fail(&error),
        };
        let child = guard.disarm();
        Ok(Self {
            child,
            stdin,
            rx,
            opened,
            seen_query_ids: Vec::new(),
        })
    }

    fn spawn_with_args(args: &[&str], open_timeout: Duration) -> Result<Self, String> {
        let child = Command::new(std::env::current_exe().map_err(|_| "current exe failed")?)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| "spawn failed")?;
        let mut guard = ChildInitGuard::new(child);
        let stdin = guard
            .child_mut()?
            .stdin
            .take()
            .ok_or("child stdin missing")?;
        let stdout = guard
            .child_mut()?
            .stdout
            .take()
            .ok_or("child stdout missing")?;
        let rx = Self::spawn_reader(stdout, false);
        let opened = match recv_reader_event(&rx, open_timeout) {
            Ok(ToParent::Opened(opened)) => opened,
            Ok(ToParent::Query(_) | ToParent::Closed(_)) => {
                return guard.fail("helper sent unexpected event before open");
            }
            Err(error) => return guard.fail(&error),
        };
        let child = guard.disarm();
        Ok(Self {
            child,
            stdin,
            rx,
            opened,
            seen_query_ids: Vec::new(),
        })
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64", test))]
    fn fixture_test_command(case: &str) -> Result<Command, String> {
        let mut cmd = Command::new(std::env::current_exe().map_err(|_| "current exe failed")?);
        cmd.arg("--exact")
            .arg("tests::fixture_process_entry")
            .arg("--nocapture")
            .env("MICRO_HID_TEST_FIXTURE", case)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    }

    fn query_with_known_id(
        &mut self,
        label: &str,
        expected_request_id: u64,
        timeout_ms: u64,
    ) -> Result<QueryResult, String> {
        let command = FromParent::Query {
            label: label.into(),
            request_id: expected_request_id,
            timeout_ms,
        };
        self.send(&command)?;
        self.recv_query(label, Some(expected_request_id))
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn spawn_reader(
        stdout: impl std::io::Read + Send + 'static,
        allow_non_json_prefix: bool,
    ) -> mpsc::Receiver<ReaderEvent> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut seen_json = false;
            loop {
                let line = match read_bounded_line(&mut reader, IPC_MAX_MESSAGE_BYTES) {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        let _ = tx.send(ReaderEvent::Eof);
                        break;
                    }
                    Err(error) => {
                        let _ = tx.send(ReaderEvent::Error(error));
                        break;
                    }
                };
                let Ok(event) = serde_json::from_str::<ToParent>(&line) else {
                    if allow_non_json_prefix && !seen_json {
                        continue;
                    }
                    let _ = tx.send(ReaderEvent::Error("helper output parse error".into()));
                    break;
                };
                seen_json = true;
                if tx.send(ReaderEvent::Event(event)).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn start_observe(
        &mut self,
        label: &str,
        expected_request_id: u64,
        timeout_ms: u64,
    ) -> Result<(), String> {
        let command = FromParent::Observe {
            label: label.into(),
            timeout_ms,
            expected_request_id,
        };
        self.send(&command)
    }

    fn finish_observe(
        &mut self,
        label: &str,
        expected_request_id: u64,
    ) -> Result<QueryResult, String> {
        self.recv_query(label, Some(expected_request_id))
    }

    fn close(&mut self) -> Result<CloseResult, String> {
        self.send(&FromParent::Close)?;
        let result = match recv_reader_event(&self.rx, IPC_RECV_TIMEOUT)? {
            ToParent::Closed(result) => result,
            ToParent::Opened(_) | ToParent::Query(_) => {
                return Err("helper sent non-close event while closing".into());
            }
        };
        if !wait_child_exit(&mut self.child, CHILD_WAIT_AFTER_CLOSE)? {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        Ok(result)
    }

    fn recv_query(
        &mut self,
        expected_label: &str,
        expected_request_id: Option<u64>,
    ) -> Result<QueryResult, String> {
        let event = recv_reader_event(&self.rx, IPC_RECV_TIMEOUT)?;
        let result = match event {
            ToParent::Query(result) => result,
            ToParent::Opened(_) => return Err("helper repeated open event".into()),
            ToParent::Closed(_) => return Err("helper closed before query response".into()),
        };
        validate_query_result(
            &result,
            expected_label,
            expected_request_id,
            &mut self.seen_query_ids,
        )?;
        Ok(result)
    }

    fn send(&mut self, command: &FromParent) -> Result<(), String> {
        let encoded = serde_json::to_string(command).map_err(|_| "encode command failed")?;
        if encoded.len() + 1 > IPC_MAX_MESSAGE_BYTES {
            return Err("command too large".into());
        }
        self.stdin
            .write_all(encoded.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|_| "command send failed".to_string())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
enum ReaderEvent {
    Event(ToParent),
    Eof,
    Error(String),
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn recv_reader_event(
    rx: &mpsc::Receiver<ReaderEvent>,
    timeout: Duration,
) -> Result<ToParent, String> {
    match rx.recv_timeout(timeout) {
        Ok(ReaderEvent::Event(event)) => Ok(event),
        Ok(ReaderEvent::Eof) => Err("helper stdout EOF".into()),
        Ok(ReaderEvent::Error(error)) => Err(error),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("helper response timeout".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("helper stdout disconnected".into()),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn validate_query_result(
    result: &QueryResult,
    expected_label: &str,
    expected_request_id: Option<u64>,
    seen_ids: &mut Vec<u64>,
) -> Result<(), String> {
    if result.label != expected_label {
        return Err(format!(
            "helper response label mismatch: expected {expected_label}, got {}",
            result.label
        ));
    }
    if let Some(expected) = expected_request_id
        && result.request_id != Some(expected)
    {
        return Err(format!(
            "helper response request_id mismatch: expected {expected}, got {:?}",
            result.request_id
        ));
    }
    if let Some(id) = result.request_id {
        if seen_ids.contains(&id) {
            return Err(format!("helper response duplicated request_id {id}"));
        }
        seen_ids.push(id);
    }
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn wait_child_exit(child: &mut Child, timeout: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .map_err(|_| "child try_wait failed".to_string())?
            .is_some()
        {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_millis(20));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for ChildConn {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct ChildInitGuard {
    child: Option<Child>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl ChildInitGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> Result<&mut Child, String> {
        self.child.as_mut().ok_or("child missing".into())
    }

    fn disarm(mut self) -> Child {
        self.child.take().expect("child exists")
    }

    fn cleanup(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            return child.wait().is_ok();
        }
        false
    }

    fn fail<T>(&mut self, message: &str) -> Result<T, String> {
        let reaped = self.cleanup();
        Err(format!("{message}; cleanup_reaped={reaped}"))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for ChildInitGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy)]
enum OpenMode {
    Normal,
    Seize,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl OpenMode {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Seize => "seize",
        }
    }

    fn option(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::Seize => 1,
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct OpenError {
    message: String,
    open_code: Option<i64>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct HidSession {
    device: Cf,
    run_loop: CFTypeRef,
    state: Box<InputState>,
    observer: Decoder,
    opened: bool,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl HidSession {
    fn open(mode: OpenMode) -> Result<Self, OpenError> {
        let candidate = select_candidate().map_err(OpenError::from)?;
        let device = candidate.device;
        let open = unsafe { IOHIDDeviceOpen(device.0, mode.option()) };
        if open != 0 {
            return Err(OpenError {
                message: format!("IOHIDDeviceOpen failed: {open}"),
                open_code: Some(i64::from(open)),
            });
        }
        let reopened = describe_device(device.0).ok_or(OpenError {
            message: "descriptor unavailable after open".into(),
            open_code: None,
        })?;
        if !is_qualified_micro_descriptor(&reopened) {
            let _ = unsafe { IOHIDDeviceClose(device.0, 0) };
            return Err(OpenError {
                message: "identity changed after open".into(),
                open_code: None,
            });
        }
        let mut state = Box::new(InputState {
            buf: vec![0u8; REPORT_SIZE + 1],
            queue: VecDeque::new(),
        });
        let ctx = (&raw mut *state).cast::<c_void>();
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                device.0,
                state.buf.as_mut_ptr(),
                (REPORT_SIZE + 1) as isize,
                Some(on_input),
                ctx,
            );
            IOHIDDeviceScheduleWithRunLoop(device.0, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        }
        Ok(Self {
            device,
            run_loop: unsafe { CFRunLoopGetCurrent() },
            state,
            observer: Decoder::new(),
            opened: true,
        })
    }

    fn query_version(&mut self, label: &str, request_id: u64, timeout: Duration) -> QueryResult {
        let request = json!({"method":"sys.version","params":{},"id":request_id});
        let reports = match encode_json(&request) {
            Ok(reports) => reports,
            Err(error) => {
                return QueryResult {
                    label: label.into(),
                    request_id: Some(request_id),
                    native_write_code: -1,
                    native_write_ok: false,
                    write_attempted: false,
                    correlated_response: false,
                    version: None,
                    error: Some(error.to_string()),
                };
            }
        };
        let mut decoder = Decoder::new();
        let started = Instant::now();
        for report in &reports {
            let write = write_report(self.device.0, report);
            if write != 0 {
                return QueryResult {
                    label: label.into(),
                    request_id: Some(request_id),
                    native_write_code: i64::from(write),
                    native_write_ok: false,
                    write_attempted: true,
                    correlated_response: false,
                    version: None,
                    error: Some(format!("IOHIDDeviceSetReport failed: {write}")),
                };
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(report) = self.state.queue.pop_front() {
                match decode_observed_correlation(
                    &mut decoder,
                    &report,
                    request_id,
                    started.elapsed(),
                ) {
                    Ok(Some(ObservedCorrelation::MatchedResult { version })) => {
                        return QueryResult {
                            label: label.into(),
                            request_id: Some(request_id),
                            native_write_code: 0,
                            native_write_ok: true,
                            write_attempted: true,
                            correlated_response: true,
                            version,
                            error: None,
                        };
                    }
                    Ok(Some(ObservedCorrelation::MatchedError { error })) => {
                        return QueryResult {
                            label: label.into(),
                            request_id: Some(request_id),
                            native_write_code: 0,
                            native_write_ok: true,
                            write_attempted: true,
                            correlated_response: true,
                            version: None,
                            error: Some(error),
                        };
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return QueryResult {
                            label: label.into(),
                            request_id: Some(request_id),
                            native_write_code: 0,
                            native_write_ok: true,
                            write_attempted: true,
                            correlated_response: false,
                            version: None,
                            error: Some(error),
                        };
                    }
                }
            }
            if Instant::now() >= deadline {
                return QueryResult {
                    label: label.into(),
                    request_id: Some(request_id),
                    native_write_code: 0,
                    native_write_ok: true,
                    write_attempted: true,
                    correlated_response: false,
                    version: None,
                    error: Some("query timeout".into()),
                };
            }
            let _ = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 1) };
        }
    }

    fn observe_response(
        &mut self,
        label: &str,
        expected_request_id: u64,
        timeout: Duration,
    ) -> QueryResult {
        observe_with_pump(
            &mut self.observer,
            label,
            expected_request_id,
            timeout,
            || self.state.queue.pop_front(),
            || {
                let _ = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 1) };
            },
            |_unused| 0,
        )
    }

    fn close(&mut self) -> i64 {
        if !self.opened {
            return 0;
        }
        self.opened = false;
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                self.device.0,
                self.state.buf.as_mut_ptr(),
                (REPORT_SIZE + 1) as isize,
                None,
                null_mut(),
            );
            IOHIDDeviceUnscheduleFromRunLoop(self.device.0, self.run_loop, kCFRunLoopDefaultMode);
        }
        i64::from(unsafe { IOHIDDeviceClose(self.device.0, 0) })
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn observe_with_pump<FRead, FIdle, FWrite>(
    decoder: &mut Decoder,
    label: &str,
    expected_request_id: u64,
    timeout: Duration,
    mut next_report: FRead,
    mut idle: FIdle,
    mut write_fn: FWrite,
) -> QueryResult
where
    FRead: FnMut() -> Option<Vec<u8>>,
    FIdle: FnMut(),
    FWrite: FnMut(&[u8; REPORT_SIZE]) -> i32,
{
    let _ = &mut write_fn;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(report) = next_report() {
            match decode_observed_correlation(
                decoder,
                &report,
                expected_request_id,
                Duration::from_secs(0),
            ) {
                Ok(Some(ObservedCorrelation::MatchedResult { version })) => {
                    return QueryResult {
                        label: label.into(),
                        request_id: Some(expected_request_id),
                        native_write_code: 0,
                        native_write_ok: true,
                        write_attempted: false,
                        correlated_response: true,
                        version,
                        error: None,
                    };
                }
                Ok(Some(ObservedCorrelation::MatchedError { error })) => {
                    return QueryResult {
                        label: label.into(),
                        request_id: Some(expected_request_id),
                        native_write_code: 0,
                        native_write_ok: true,
                        write_attempted: false,
                        correlated_response: true,
                        version: None,
                        error: Some(error),
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return QueryResult {
                        label: label.into(),
                        request_id: Some(expected_request_id),
                        native_write_code: 0,
                        native_write_ok: true,
                        write_attempted: false,
                        correlated_response: false,
                        version: None,
                        error: Some(error),
                    };
                }
            }
        }
        if Instant::now() >= deadline {
            return QueryResult {
                label: label.into(),
                request_id: Some(expected_request_id),
                native_write_code: 0,
                native_write_ok: true,
                write_attempted: false,
                correlated_response: false,
                version: None,
                error: Some("observe timeout".into()),
            };
        }
        idle();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for HidSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_report(device: CFTypeRef, report: &[u8; REPORT_SIZE]) -> i32 {
    let id = report[0];
    let bytes = if id == 0 { &report[1..] } else { &report[..] };
    unsafe {
        IOHIDDeviceSetReport(
            device,
            1,
            i64::from(id),
            bytes.as_ptr(),
            bytes.len() as isize,
        )
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct Candidate {
    device: Cf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn select_candidate() -> Result<Candidate, HidError> {
    let manager =
        Cf::new(unsafe { IOHIDManagerCreate(kCFAllocatorDefault, 0) }).ok_or(HidError::NotFound)?;
    let matching = matching(APPLICATION_VID, APPLICATION_PID).ok_or(HidError::NotFound)?;
    unsafe { IOHIDManagerSetDeviceMatching(manager.0, matching.0) };
    let devices =
        Cf::new(unsafe { IOHIDManagerCopyDevices(manager.0) }).ok_or(HidError::NotFound)?;
    let count = unsafe { CFSetGetCount(devices.0) };
    if count <= 0 || count > 64 {
        return Err(HidError::NotFound);
    }
    let mut values = vec![null(); count as usize];
    unsafe { CFSetGetValues(devices.0, values.as_mut_ptr()) };
    let mut matched = Vec::new();
    for device in values {
        let Some(descriptor) = describe_device(device) else {
            continue;
        };
        if !is_qualified_micro_descriptor(&descriptor) {
            continue;
        }
        let report = original_report_descriptor(device).ok_or(HidError::EvidenceUnavailable(
            copilot_controller::native::hid::EvidenceKind::RawReportDescriptor,
        ))?;
        verify_micro_descriptor(&descriptor, &report)?;
        let retained = unsafe { CFRetain(device) };
        matched.push(Candidate {
            device: Cf(retained),
        });
    }
    if matched.len() != 1 {
        return Err(HidError::NotFound);
    }
    Ok(matched.remove(0))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_qualified_micro_descriptor(descriptor: &HidDescriptor) -> bool {
    descriptor.identity
        == HidIdentity {
            vendor_id: APPLICATION_VID,
            product_id: APPLICATION_PID,
        }
        && descriptor.usage_page == USAGE_PAGE
        && descriptor.usage == USAGE
        && descriptor.interface_number == Some(2)
        && descriptor.bus == Some(HidBus::Usb)
        && descriptor.max_input_report_len == REPORT_SIZE
        && descriptor.max_output_report_len == REPORT_SIZE
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservedCorrelation {
    MatchedResult { version: Option<String> },
    MatchedError { error: String },
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn decode_observed_correlation(
    decoder: &mut Decoder,
    report: &[u8],
    expected_request_id: u64,
    now: Duration,
) -> Result<Option<ObservedCorrelation>, String> {
    match decoder
        .feed(report, now)
        .map_err(|error| error.to_string())?
    {
        Some(object) => Ok(classify_observed_message(
            object.as_map(),
            expected_request_id,
        )),
        None => Ok(None),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn classify_observed_message(
    object: &serde_json::Map<String, Value>,
    expected_request_id: u64,
) -> Option<ObservedCorrelation> {
    if !object
        .get("id")
        .and_then(value_to_u64)
        .is_some_and(|id| id == expected_request_id)
    {
        return None;
    }
    if let Some(value) = object.get("result") {
        let version = value
            .as_object()
            .and_then(|map| map.get("version"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Some(ObservedCorrelation::MatchedResult { version });
    }
    if object.contains_key("error") {
        return Some(ObservedCorrelation::MatchedError {
            error: "device rejected".into(),
        });
    }
    None
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn value_to_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .filter(|id| *id >= 0)
            .and_then(|id| u64::try_from(id).ok())
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn validate_query_command(label: &str, timeout_ms: u64) -> Result<(), String> {
    if label.is_empty() || label.len() > LABEL_MAX_BYTES {
        return Err("invalid query label".into());
    }
    if timeout_ms == 0 || timeout_ms > QUERY_TIMEOUT_MS_MAX {
        return Err("invalid timeout_ms".into());
    }
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    loop {
        let chunk = reader
            .fill_buf()
            .map_err(|_| "stdin/stdout read failed".to_string())?;
        if chunk.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        let remaining = max_bytes.saturating_sub(buf.len());
        if let Some(pos) = chunk.iter().position(|byte| *byte == b'\n') {
            let take = pos + 1;
            if take > remaining {
                return Err("ipc line too long".into());
            }
            buf.extend_from_slice(&chunk[..take]);
            reader.consume(take);
            break;
        }
        if chunk.len() > remaining {
            return Err("ipc line too long".into());
        }
        buf.extend_from_slice(chunk);
        let consumed = chunk.len();
        reader.consume(consumed);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| "ipc line not utf8".into())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn describe_device(device: CFTypeRef) -> Option<HidDescriptor> {
    let vendor = prop_i32(device, "VendorID")?;
    let product = prop_i32(device, "ProductID")?;
    let usage_page =
        prop_i32(device, "PrimaryUsagePage").or_else(|| prop_i32(device, "DeviceUsagePage"))?;
    let usage = prop_i32(device, "PrimaryUsage").or_else(|| prop_i32(device, "DeviceUsage"))?;
    let input_len = prop_i32(device, "MaxInputReportSize").unwrap_or(0).max(0) as usize;
    let output_len = prop_i32(device, "MaxOutputReportSize").unwrap_or(0).max(0) as usize;
    let registry = entry_id(device)?;
    Some(HidDescriptor {
        identity: HidIdentity {
            vendor_id: u16::try_from(vendor).ok()?,
            product_id: u16::try_from(product).ok()?,
        },
        usage_page: u16::try_from(usage_page).ok()?,
        usage: u16::try_from(usage).ok()?,
        product: prop_string(device, "Product"),
        serial: prop_string(device, "SerialNumber"),
        max_input_report_len: input_len,
        max_output_report_len: output_len,
        interface_number: interface_from_ancestry(device, APPLICATION_VID, APPLICATION_PID),
        bus: bus_from_property(device),
        location_id: None,
        path: HidPath::new(format!("registry:{registry}")).ok()?,
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn bus_from_property(device: CFTypeRef) -> Option<HidBus> {
    let transport = prop_string(device, "Transport")?;
    if transport.eq_ignore_ascii_case("usb") {
        Some(HidBus::Usb)
    } else if transport.eq_ignore_ascii_case("bluetooth") {
        Some(HidBus::Bluetooth)
    } else {
        Some(HidBus::Unknown)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn interface_from_ancestry(device: CFTypeRef, vendor: u16, product: u16) -> Option<i32> {
    let service = unsafe { IOHIDDeviceGetService(device) };
    if service == 0 {
        return None;
    }
    let mut current = IoOwned::new(service)?;
    let class = c"IOUSBHostInterface";
    let mut seen = Vec::new();
    for _ in 0..16 {
        let id = entry_registry_id(current.0)?;
        if seen.contains(&id) {
            return None;
        }
        seen.push(id);
        if unsafe { IOObjectConformsTo(current.0, class.as_ptr()) } != 0 {
            let interface = entry_property_i32(current.0, "bInterfaceNumber")?;
            if !(0..=255).contains(&interface) {
                return None;
            }
            let vid = entry_property_i32(current.0, "idVendor")?;
            let pid = entry_property_i32(current.0, "idProduct")?;
            if vid == i32::from(vendor) && pid == i32::from(product) {
                return Some(interface);
            }
            return None;
        }
        current = parent_entry(current.0)?;
    }
    None
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn original_report_descriptor(device: CFTypeRef) -> Option<Vec<u8>> {
    let key = cfstr("ReportDescriptor")?;
    let data = unsafe { IOHIDDeviceGetProperty(device, key.0) };
    if data.is_null() || unsafe { CFGetTypeID(data) != CFDataGetTypeID() } {
        return None;
    }
    let len = unsafe { CFDataGetLength(data) };
    if !(1..=1024).contains(&len) {
        return None;
    }
    let ptr = unsafe { CFDataGetBytePtr(data) };
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn matching(vendor: u16, product: u16) -> Option<Cf> {
    let kv = cfstr("VendorID")?;
    let kp = cfstr("ProductID")?;
    let vv = cfnum(i32::from(vendor))?;
    let vp = cfnum(i32::from(product))?;
    let keys = [kv.0, kp.0];
    let values = [vv.0, vp.0];
    Cf::new(unsafe {
        CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            2,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_i32(device: CFTypeRef, key: &str) -> Option<i32> {
    let key = cfstr(key)?;
    let value = unsafe { IOHIDDeviceGetProperty(device, key.0) };
    if value.is_null() || unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i32;
    (unsafe { CFNumberGetValue(value, 3, (&raw mut out).cast()) } != 0).then_some(out)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_string(device: CFTypeRef, key: &str) -> Option<String> {
    let key = cfstr(key)?;
    let value = unsafe { IOHIDDeviceGetProperty(device, key.0) };
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    if unsafe { CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, 0x0800_0100) } == 0
    {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn entry_id(device: CFTypeRef) -> Option<u64> {
    let service = unsafe { IOHIDDeviceGetService(device) };
    entry_registry_id(service)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn entry_registry_id(entry: u32) -> Option<u64> {
    if entry == 0 {
        return None;
    }
    let mut id = 0u64;
    (unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) } == 0).then_some(id)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn parent_entry(entry: u32) -> Option<IoOwned> {
    let plane = c"IOService";
    let mut parent = 0u32;
    (unsafe { IORegistryEntryGetParentEntry(entry, plane.as_ptr(), &mut parent) == 0 })
        .then(|| IoOwned::new(parent))
        .flatten()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn entry_property_i32(entry: u32, key: &str) -> Option<i32> {
    let key = cfstr(key)?;
    let value =
        Cf::new(unsafe { IORegistryEntryCreateCFProperty(entry, key.0, kCFAllocatorDefault, 0) })?;
    if unsafe { CFGetTypeID(value.0) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i32;
    (unsafe { CFNumberGetValue(value.0, 3, (&raw mut out).cast()) } != 0).then_some(out)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn cfstr(value: &str) -> Option<Cf> {
    let c = CString::new(value).ok()?;
    Cf::new(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), 0x0800_0100) })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn cfnum(value: i32) -> Option<Cf> {
    Cf::new(unsafe { CFNumberCreate(kCFAllocatorDefault, 3, (&raw const value).cast()) })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct InputState {
    buf: Vec<u8>,
    queue: VecDeque<Vec<u8>>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
extern "C" fn on_input(
    context: *mut c_void,
    result: i32,
    _sender: *mut c_void,
    _ty: u32,
    id: u32,
    report: *mut u8,
    len: isize,
) {
    if result != 0
        || context.is_null()
        || report.is_null()
        || len <= 0
        || len as usize > REPORT_SIZE + 1
    {
        return;
    }
    let (state, data) = unsafe {
        (
            &mut *context.cast::<InputState>(),
            std::slice::from_raw_parts(report, len as usize),
        )
    };
    if state.queue.len() >= 64 {
        return;
    }
    let mut framed = Vec::with_capacity(REPORT_SIZE);
    if data.first().copied().map(u32::from) != Some(id) || id == 0 {
        framed.push(id as u8);
    }
    framed.extend_from_slice(data);
    if framed.len() == REPORT_SIZE {
        state.queue.push_back(framed);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct Cf(CFTypeRef);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Cf {
    fn new(value: CFTypeRef) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct IoOwned(u32);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl IoOwned {
    fn new(value: u32) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for IoOwned {
    fn drop(&mut self) {
        let _ = unsafe { IOObjectRelease(self.0) };
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl From<HidError> for OpenError {
    fn from(value: HidError) -> Self {
        Self {
            message: value.to_string(),
            open_code: None,
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
type CFTypeRef = *const c_void;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    static kCFRunLoopDefaultMode: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: isize, enc: u32) -> u8;
    fn CFNumberGetValue(number: CFTypeRef, ty: isize, out: *mut c_void) -> u8;
    fn CFNumberCreate(a: CFTypeRef, ty: isize, value: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithCString(a: CFTypeRef, text: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFDictionaryCreate(
        a: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: isize,
        kcb: *const u8,
        vcb: *const u8,
    ) -> CFTypeRef;
    fn CFSetGetCount(set: CFTypeRef) -> isize;
    fn CFSetGetValues(set: CFTypeRef, values: *mut CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFTypeRef;
    fn CFRunLoopRunInMode(mode: CFTypeRef, secs: f64, return_after_source_handled: u8) -> i32;
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(a: CFTypeRef, options: u32) -> CFTypeRef;
    fn IOHIDManagerSetDeviceMatching(manager: CFTypeRef, matching: CFTypeRef);
    fn IOHIDManagerCopyDevices(manager: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetProperty(device: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetService(device: CFTypeRef) -> u32;
    fn IOHIDDeviceOpen(device: CFTypeRef, options: u32) -> i32;
    fn IOHIDDeviceClose(device: CFTypeRef, options: u32) -> i32;
    fn IOHIDDeviceSetReport(
        device: CFTypeRef,
        ty: u32,
        id: i64,
        report: *const u8,
        len: isize,
    ) -> i32;
    fn IOHIDDeviceRegisterInputReportCallback(
        device: CFTypeRef,
        report: *mut u8,
        len: isize,
        callback: Option<extern "C" fn(*mut c_void, i32, *mut c_void, u32, u32, *mut u8, isize)>,
        context: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(device: CFTypeRef, run_loop: CFTypeRef, mode: CFTypeRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(device: CFTypeRef, run_loop: CFTypeRef, mode: CFTypeRef);
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFTypeRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IOObjectConformsTo(object: u32, class_name: *const c_char) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use serde_json::json;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::env;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::io::Cursor;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::sync::mpsc;

    #[test]
    fn scenario_defaults_are_bounded_and_empty() {
        let scenario = ScenarioResult::default();
        assert!(scenario.name.is_empty());
        assert!(scenario.a_open.is_none());
        assert!(scenario.b_after_query.is_none());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn fixture_process_entry() {
        let Ok(case) = env::var("MICRO_HID_TEST_FIXTURE") else {
            return;
        };
        match run_fixture(&case) {
            Ok(()) => std::process::exit(0),
            Err(_) => std::process::exit(101),
        }
    }

    #[test]
    fn helper_command_schema_rejects_ambiguous_inputs() {
        assert!(
            serde_json::from_str::<FromParent>(
                r#"{"cmd":"query","label":"x","request_id":2001,"timeout_ms":1}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<FromParent>(
                r#"{"cmd":"observe","label":"x","timeout_ms":1,"expected_request_id":101}"#
            )
            .is_ok()
        );
        assert!(serde_json::from_str::<FromParent>(r#"{"cmd":"close"}"#).is_ok());
        assert!(serde_json::from_str::<FromParent>(r#"{"cmd":"query","timeout_ms":1}"#).is_err());
        assert!(
            serde_json::from_str::<FromParent>(r#"{"cmd":"query","label":"x","timeout_ms":"1"}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<FromParent>(r#"{"cmd":"unknown"}"#).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn version_query_uses_single_vendor_frame() {
        let request = json!({"method":"sys.version","params":{},"id":3001});
        let frames = encode_json(&request).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], 6);
        assert_eq!(frames[0][1], 2);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn observed_correlation_matches_only_expected_id() {
        let matched = classify_observed_message(
            json!({"id": 501, "result": {"version":"fw0.6.3"}})
                .as_object()
                .unwrap(),
            501,
        );
        assert_eq!(
            matched,
            Some(ObservedCorrelation::MatchedResult {
                version: Some("fw0.6.3".into())
            })
        );

        let matched_error = classify_observed_message(
            json!({"id": 501, "error": {"code": -1}})
                .as_object()
                .unwrap(),
            501,
        );
        assert_eq!(
            matched_error,
            Some(ObservedCorrelation::MatchedError {
                error: "device rejected".into()
            })
        );

        assert!(
            classify_observed_message(
                json!({"id": 500, "result": {"version":"old"}})
                    .as_object()
                    .unwrap(),
                501
            )
            .is_none()
        );
        assert!(
            classify_observed_message(json!({"m":"v.oai.hid"}).as_object().unwrap(), 501).is_none()
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn bounded_line_reader_enforces_limits() {
        let mut ok = Cursor::new(b"{\"x\":1}\n".to_vec());
        let line = read_bounded_line(&mut ok, 16).unwrap().unwrap();
        assert_eq!(line, "{\"x\":1}");
        assert_eq!(read_bounded_line(&mut ok, 16).unwrap(), None);

        let mut too_long = Cursor::new(b"123456\n".to_vec());
        assert!(read_bounded_line(&mut too_long, 5).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn query_command_validation_rejects_out_of_bounds() {
        assert!(validate_query_command("ok", 1).is_ok());
        assert!(validate_query_command("", 1).is_err());
        assert!(validate_query_command("ok", 0).is_err());
        assert!(validate_query_command("ok", QUERY_TIMEOUT_MS_MAX + 1).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn query_result_validation_rejects_mismatch_and_duplicates() {
        let mut seen = Vec::new();
        let ok = QueryResult {
            label: "phase1".into(),
            request_id: Some(501),
            native_write_code: 0,
            native_write_ok: true,
            write_attempted: true,
            correlated_response: true,
            version: Some("fw0.6.3".into()),
            error: None,
        };
        assert!(validate_query_result(&ok, "phase1", None, &mut seen).is_ok());
        assert_eq!(seen, vec![501]);

        let wrong_label = QueryResult {
            label: "stale".into(),
            ..ok.clone()
        };
        assert!(validate_query_result(&wrong_label, "phase2", None, &mut seen).is_err());

        let wrong_id = QueryResult {
            label: "observe".into(),
            request_id: Some(600),
            ..ok.clone()
        };
        assert!(validate_query_result(&wrong_id, "observe", Some(601), &mut seen).is_err());

        let duplicate = QueryResult {
            label: "dup".into(),
            request_id: Some(501),
            ..ok
        };
        assert!(validate_query_result(&duplicate, "dup", None, &mut seen).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn recv_reader_event_propagates_eof_and_errors() {
        let (tx, rx) = mpsc::channel();
        tx.send(ReaderEvent::Eof).unwrap();
        assert!(recv_reader_event(&rx, Duration::from_millis(5)).is_err());

        let (tx, rx) = mpsc::channel();
        tx.send(ReaderEvent::Error("parse".into())).unwrap();
        assert!(recv_reader_event(&rx, Duration::from_millis(5)).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn ipc_reader_surfaces_oversized_line_error() {
        let oversized = "x".repeat(IPC_MAX_MESSAGE_BYTES + 8);
        let rx = ChildConn::spawn_reader(Cursor::new(format!("{oversized}\n").into_bytes()), false);
        let event = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(event, ReaderEvent::Error(error) if error == "ipc line too long"));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn request_allocator_is_global_and_unique() {
        let mut ids = RequestIdAllocator::new(FIRST_LAB_REQUEST_ID);
        assert_eq!(ids.next().unwrap(), FIRST_LAB_REQUEST_ID);
        assert_eq!(ids.next().unwrap(), FIRST_LAB_REQUEST_ID + 1);
        assert_eq!(ids.next().unwrap(), FIRST_LAB_REQUEST_ID + 2);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn observe_path_does_not_invoke_write_seam() {
        let packet = encode_json(&json!({"id": 7777, "result": {"version":"fw0.6.3"}}))
            .unwrap()
            .remove(0)
            .to_vec();
        let mut queue = VecDeque::from([packet]);
        let mut decoder = Decoder::new();
        let mut writes = 0u32;
        let result = observe_with_pump(
            &mut decoder,
            "observe",
            7777,
            Duration::from_millis(50),
            || queue.pop_front(),
            || {},
            |_| {
                writes += 1;
                0
            },
        );
        assert!(result.correlated_response);
        assert!(!result.write_attempted);
        assert_eq!(writes, 0);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn child_init_hang_is_killed_and_reaped_on_open_failure() {
        let error = match ChildConn::spawn_fixture("init_hang", Duration::from_millis(150)) {
            Ok(_) => panic!("expected init_hang fixture to fail open"),
            Err(error) => error,
        };
        assert!(error.contains("cleanup_reaped=true"));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn close_ack_then_hang_gets_killed_after_bounded_wait() {
        let mut conn = ChildConn::spawn_fixture("close_ack_hang", Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let closed = conn.close().unwrap();
        assert_eq!(closed.close_code, 0);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(conn.child.try_wait().unwrap().is_some());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn watchdog_fixture_exits_124_while_idle() {
        let mut cmd = ChildConn::fixture_test_command("watchdog_idle_ms:200").unwrap();
        let mut child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(124));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn fixture_exits_on_stdin_eof() {
        let mut cmd = ChildConn::fixture_test_command("eof_exit").unwrap();
        let mut child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        assert!(status.success());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn qualified_descriptor_requires_exact_interface_transport_and_lengths() {
        let base = HidDescriptor {
            identity: HidIdentity {
                vendor_id: APPLICATION_VID,
                product_id: APPLICATION_PID,
            },
            usage_page: USAGE_PAGE,
            usage: USAGE,
            product: None,
            serial: None,
            max_input_report_len: REPORT_SIZE,
            max_output_report_len: REPORT_SIZE,
            interface_number: Some(2),
            bus: Some(HidBus::Usb),
            location_id: None,
            path: HidPath::new("unit").unwrap(),
        };
        assert!(is_qualified_micro_descriptor(&base));
        let mut wrong_interface = base.clone();
        wrong_interface.interface_number = Some(0);
        assert!(!is_qualified_micro_descriptor(&wrong_interface));
        let mut wrong_bus = base.clone();
        wrong_bus.bus = Some(HidBus::Bluetooth);
        assert!(!is_qualified_micro_descriptor(&wrong_bus));
        let mut wrong_output = base;
        wrong_output.max_output_report_len = 65;
        assert!(!is_qualified_micro_descriptor(&wrong_output));
    }
}
