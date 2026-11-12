# Disposable diagnostic worker

This module provides one-request Windows and macOS ARM64 workers, not a terminal action,
credential sandbox, or readiness/qualification signal. It has no built-in
production callbacks and never selects a caller-provided executable or shell.
The executable must supply a finite, static **diagnostic-only** callback
allowlist. Callbacks must validate their typed payloads, avoid protocol stdout,
and return only privacy-safe diagnostic data.

## Integration

Add this directory as a Rust module and provide these dependencies:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde"] }

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Security_Cryptography",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_System_JobObjects",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```

Construct a `DiagnosticAllowlist` from a `'static` slice of
`DiagnosticCallback::new(kind, function)` entries. Functions have the type
`fn(serde_json::Value) -> Result<serde_json::Value, DiagnosticCallbackError>`.
The registry rejects invalid or duplicate kinds; both coordinator and worker
must use the same allowlist. Do not register general command dispatchers.

Before normal argument handling or console attachment, route
`maybe_run_hidden_worker(std::env::args_os().skip(1), &allowlist)`. If it
returns `Some(result)`, exit immediately: zero for `Ok`, nonzero for `Err`.
Do not expose the hidden argument as a public command. An executable must make
this routing change itself; this directory does not modify its entry point.

Call `probe(&request, timeout, &cancelled, &allowlist)` with an `AtomicBool`
cancellation flag. `DiagnosticResult` contains the correlated worker lifetime
and callback payload. `WorkerError` contains a static kind, optional worker
lifetime, and `replacement_allowed`; its display never includes native or
callback strings. Callers must still validate typed report target/window/time
correlation and lifecycle state. A successful response must not change
`read_only`, `command_ready`, or qualification policy.

Admission checks the 1 ms–120 s timeout, prelaunch cancellation, request and
allowlist, and bounded encoding before platform dispatch. macOS ARM64 admits
only health, Micro and AKP03 and revalidates complete typed options before spawn.
Other non-Windows targets return `unsupported_platform` without a callback.
Frames use one 65,536-byte-maximum JSON body and require EOF; stderr is
discarded with a 16,384-byte limit. Duplicate JSON object keys (including inside
generic payloads) fail as `protocol` before callback dispatch.
Responses are completely encoded before stdout is written. Serialization/frame
failure produces a compact correlated `response_encoding_failed` frame, mapped
to `frame_limit`, instead of disappearing as EOF. It does not imply zero device
writes. Device callbacks use the 65,280-byte payload budget and explicit
post-cleanup omission metadata; the 65,536-byte frame bound is unchanged.

The controller executable's static registry contains `health`, `ct_serial_probe`,
`micro_hid_probe`, and `akp03_hid_probe`. This explicitly combined
`diagnostic_allowlist()` is distinct from the original health-only
`health_allowlist()`, which rejects device requests. HID coordinators use
kind-only registries; the named AKP03 hidden route shares the same supervisor.
Only explicit `ct-probe` launches the latter kind; its typed §6
options cannot choose a port, path, command or selector. The worker callback
owns the native CT transport and returns only validated report/failure data.
The coordinator treats a transported CT failure as a nonzero CLI error even
when the framing/worker transport itself completed successfully. See
`specs/DISPLAY_DEVICES.md` §6 for CLI flags and the <=10.25-second supervision
budget (session plus cancellation/close grace and bounded response allowance).

On Windows the service creates private standard pipes with noninheritable
parent ends, enables inheritance only for child ends, restricts the inheritance
list, creates the current executable suspended and detached, assigns it to a
one-process kill-on-close Job, and checks its retained PID/creation time and
request nonce. I/O is concurrent. Failure cleanup waits
up to five seconds for exit and one additional second for I/O joining; an
unconfirmed result permanently blocks further probes in that supervisor
process. These budgets cannot physically bound a stuck OS or driver. The
fence is neither durable nor cross-process, and the Job is not a
credential-restricted hostile-code sandbox.

Windows HID backend construction and native operations additionally require a
private thread-bound, per-request capability. The worker issues it only after
complete typed Micro/AKP03 option validation for a live (not health) callback
and detached one-process/no-breakaway/kill-on-close Job validation. Health,
other callback kinds, ordinary in-process callers and expired scopes cannot
access HID. Optional exact BusTypeGuid metadata uses a bounded native parent
walk only across logical HID bus nodes; it stops at missing/unknown transport.
Incomplete scans or unresolved matching candidates fail closed rather than
returning false-unique inventories. Native ancestry is revalidated after open;
interface numbers and original report descriptors remain unavailable, so
Micro/AKP03 still fail closed before transport open/report I/O. In particular,
`IOCTL_HID_GET_REPORT_DESCRIPTOR` is a minidriver request, not a supported
user-mode collection API. See `specs/PLATFORM_AND_DIAGNOSTICS.md` §2.5 for
native sources, bounds, revalidation and official references.

## Tests

### macOS ARM64

The macOS platform block retains the existing module/API name and exact routes.
It spawns only the current executable with PGID == PID, uses bounded nonblocking
stdio (no I/O threads), correlates native PID/start time plus nonce, and enforces
the Micro/AKP03 §9 budgets (caller timeouts can only shorten them). On every
completion it signals the group while WNOWAIT retains the leader PID, then reaps
within a two-second per-attempt cleanup budget (Drop may retry once) and
explicitly closes pipes. Unconfirmed
cleanup stickily forbids replacement. An independent child watchdog checks the
parent's native PID/start time every 50 ms, kills the worker group on parent
death/mismatch/unavailable observation, and can terminate a blocked main thread
without destructors or HID locks. Startup is acknowledged within one second;
ordinary stop/unpark/join consumes the remaining original child budget rather
than a separate short grace. During that wait the main thread checks parent
lifetime and the child deadline; failure or a watchdog panic kills the group.
The supervisor's shorter caller deadline still covers shutdown, EOF and exit,
so a delayed watchdog cannot independently turn completion into worker failure
before those bounds expire. Child lifetime is capped at 120 seconds before
request validation, then the device-spec budget.
This is user-space polling, not a kernel parent-death or escape guarantee;
suspended processes, scheduler/driver stalls and escaped descendants remain
outside that guarantee. Watchdog termination cannot grant durable replacement
eligibility or prove physical output cleanup.

Only a fully validated child callback can receive the private, thread-bound,
per-request IOKit capability; native access also verifies a same-executable
parent with both executable paths canonicalized (errors fail closed). Public
in-process backend construction fails closed. Health receives no
HID capability and never constructs/enumerates/opens transport. Native IOKit
output has no per-write deadline and may leave physical state changed on kill.
CT remains Windows-only; Linux live probes remain disabled. No native descriptor
or physical input/output acceptance is claimed. See platform appendix §8.7.

The self-authored fixture exercises no hardware and adds no production callback:

```sh
cargo build --all-features --example macos_worker_fixture
MACOS_WORKER_TEST_HELPER="$PWD/target/debug/examples/macos_worker_fixture" \
  cargo test --all-features --lib native::windows_worker -- --include-ignored
```

Run the same commands without `--all-features` to verify feature-independent
health and fail-closed capability paths. Tests cover same-executable success,
native identity/nonce mismatch, malformed/duplicate/trailing frames, blocked
stdin, deadline/cancellation kill/reap, stdout/stderr bounds, private-route and
capability rejection, stale capability invalidation and sticky lost-leader
cleanup, symlink-launched capability, parent termination during a blocked
callback, held descendant pipes and truncated response headers/bodies.
Fake SetReport fixtures exercise the actual write gate without IOKit
enumeration/open/output. Windows runtime tests below remain platform-specific.

`tests.rs` exercises framing, admission, correlation, cancellation, failure
precedence, and fencing with synthetic data. `tests/worker_fixture.rs` and
`tests/windows_integration.rs` are test-only, self-authored helpers: they
exercise the actual same-executable Windows path without vendor code or
hardware. The external harness can register them as the
`windows_worker_fixture` binary and `windows_integration` test; its library
crate name for the fixture is `windows_worker_harness`. Never add the fixture
callback to a production allowlist. Windows cross-checking compiles these
tests but does not execute them.
