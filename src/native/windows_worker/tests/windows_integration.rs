#![cfg(windows)]

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn controlled_same_executable_worker_suite() {
    let helper = std::env::var_os("WINDOWS_WORKER_TEST_HELPER").expect("worker fixture path");
    let mut child = Command::new(helper)
        .arg("suite")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start self-authored test coordinator");
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .expect("observe test coordinator")
            .is_some()
        {
            break;
        }
        if started.elapsed() >= Duration::from_secs(45) {
            child.kill().expect("terminate stalled test coordinator");
            let _ = child.wait();
            panic!("test coordinator exceeded its bound");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect test coordinator");
    assert!(
        output.status.success(),
        "test coordinator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "synthetic Windows worker suite passed"
    );
}
