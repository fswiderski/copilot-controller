//! Synthetic helper used only by the isolated platform test harness.

use std::{
    env, fs,
    io::{self, Write},
    process::{self, Command, Stdio},
    thread,
    time::Duration,
};

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("success") => {
            eprint!("discarded diagnostic text");
            print!("marker");
        }
        Some("failure") => {
            print!("discarded output");
            process::exit(7);
        }
        Some("output") => {
            let count: usize = args.next().expect("count").parse().expect("number");
            let block = [b'x'; 8_192];
            let mut stdout = io::stdout().lock();
            let mut remaining = count;
            while remaining != 0 {
                let part = remaining.min(block.len());
                stdout.write_all(&block[..part]).expect("write");
                remaining -= part;
            }
        }
        Some("stall") | Some("descendant") => thread::sleep(Duration::from_secs(30)),
        Some("descendant-leader") => {
            let pid_file = args.next().expect("fixture pid file");
            #[allow(
                clippy::zombie_processes,
                reason = "The synthetic leader must exit before its descendant for cleanup testing."
            )]
            let child = Command::new(env::current_exe().expect("helper executable"))
                .arg("descendant")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn self-authored descendant");
            fs::write(pid_file, child.id().to_string()).expect("write fixture");
            print!("done");
            io::stdout().flush().expect("flush");
        }
        Some("clock") => {
            let epoch = args.next().expect("epoch");
            let (ticks, frequency) = native_clock();
            print!("{epoch} {ticks} {frequency}");
        }
        _ => process::exit(64),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_clock() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_MONOTONIC;
    #[cfg(target_os = "macos")]
    let clock = libc::CLOCK_UPTIME_RAW;
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(unsafe { libc::clock_gettime(clock, time.as_mut_ptr()) }, 0);
    let time = unsafe { time.assume_init() };
    (
        time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64,
        1_000_000_000,
    )
}

#[cfg(windows)]
fn native_clock() -> (u64, u64) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(value: *mut i64) -> i32;
        fn QueryPerformanceFrequency(value: *mut i64) -> i32;
    }
    let mut counter = 0_i64;
    let mut frequency = 0_i64;
    assert_ne!(unsafe { QueryPerformanceFrequency(&mut frequency) }, 0);
    assert_ne!(unsafe { QueryPerformanceCounter(&mut counter) }, 0);
    (counter as u64, frequency as u64)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn native_clock() -> (u64, u64) {
    panic!("unsupported test platform")
}
