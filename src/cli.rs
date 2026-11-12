//! Offline command parsing and execution. Parsing completes before stdin, file
//! mutation, or an explicit contained diagnostic-worker route is considered.

mod ct_probe;
mod hid_probe;

use std::{
    io::Read,
    path::{Component, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    devices::{akp03, firmware, micro},
    native::{
        portable::{clock::sample_monotonic, inventory::portable_inventory},
        windows_observation as observation, windows_worker as worker,
    },
    persistence::root::{Name, PrivateRoot},
    presentation::{
        assignment::{CandidateAssignment, EXPORT_FILENAME, recorded_assignment},
        evidence::DeviceKind,
        profiles,
        realization::{Preferences, akp03_svg, synthetic_demo},
    },
};

pub const HELP: &str = "\
Offline controller integration. No device discovery or action delivery by default.

Usage: copilot-controller <command>
  --help, --version
  capabilities
  device-profiles
  device-preview <micro|ajazz|ct> [--reduced-motion] [--high-contrast]
                 [--brightness <0..1>] [--svg (ajazz only)]
  device-assign [--export-root <absolute-dir> --confirm ASSIGN]
  doctor
  micro-doctor [--discover (unavailable)]
  micro-decode <hex-report>... (1..128 exact 64-byte reports)
  micro-preview
  micro-probe [--capture-ms <0..30000>] [--query] [--read-keymap]
              [--test-pattern] [--health-check]
  akp03-doctor [--discover (unavailable)]
  akp03-decode <hex> [--rev2]
  akp03-preview
  akp03-probe [--capture-ms <0..30000>] [--initialize]
              [--test-pattern] [--health-check]
  micro-firmware-plan --image <absolute-file> --esptool <absolute-file>
                      (offline pinned-file validation; micro-firmware feature)
  probe-health (contained Windows/macOS ARM64 worker)
  ct-probe [--schema-version 1] [--total-timeout-ms <1000..8000>]
           [--io-timeout-ms <100..1500>] [--capture-ms <0..5000>]
           [--max-events <1..128>] [--max-received-bytes <256..32768>]
           [--test-pattern] (explicit contained Windows serial diagnostic)

All previews and probes are not ready or hardware-qualified. Native probes use
contained Windows workers or macOS ARM64 HID workers. CT remains Windows-only.
HID capture defaults to 2000 ms. Micro keymap
and LED test require --query; AKP03 display test requires --initialize.
--health-check conflicts with these opt-ins and performs no device access.
Windows HID descriptor evidence is unavailable, so HID live probes fail closed;
macOS HID requires its device feature and exact native descriptor evidence;
IOKit access/writes require the private supervised-child capability. Linux live
probes and firmware execution remain unavailable. No hardware acceptance claimed.
Doctor observes the native clock and read-only
process/desktop metadata where supported; it never discovers or opens devices.
No runtime or terminal actions are exposed.";

#[derive(Debug)]
pub enum Command {
    Help,
    Version,
    Capabilities,
    Profiles,
    Preview {
        device: DeviceKind,
        preferences: Preferences,
        svg: bool,
    },
    Assign {
        export_root: Option<PathBuf>,
    },
    Doctor,
    DeviceDoctor {
        device: DeviceKind,
        discover: bool,
    },
    MicroDecode(Vec<Vec<u8>>),
    MicroPreview,
    Akp03Decode {
        profile: akp03::Profile,
        report: Vec<u8>,
    },
    Health,
    CtProbe(crate::devices::ct::probe::Options),
    MicroProbe(micro::probe::MicroProbeOptions),
    Akp03Probe(akp03::probe::Akp03ProbeOptions),
    FirmwarePlan {
        image: PathBuf,
        tool: PathBuf,
    },
    Unavailable(&'static str),
}

pub enum Output {
    Json(Value),
    Text(String),
    Svg(String),
}

impl Output {
    pub fn render(self) -> Result<String, String> {
        let text = match self {
            Self::Json(value) => {
                serde_json::to_string_pretty(&value).map_err(|_| "output serialization failed")?
            }
            Self::Text(text) | Self::Svg(text) => text,
        };
        Ok(format!("{}\n", text.trim_end_matches('\n')))
    }
}

fn json_output(value: impl Serialize) -> Result<Output, String> {
    serde_json::to_value(value)
        .map(Output::Json)
        .map_err(|_| "output serialization failed".into())
}

fn no_args(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("unexpected command arguments".into())
    }
}

fn device(text: &str) -> Result<DeviceKind, String> {
    match text {
        "micro" => Ok(DeviceKind::Micro),
        "ajazz" => Ok(DeviceKind::Ajazz),
        "ct" => Ok(DeviceKind::Ct),
        _ => Err("device must be micro, ajazz, or ct".into()),
    }
}

fn preview(args: &[String]) -> Result<Command, String> {
    let (name, options) = args.split_first().ok_or("device is required")?;
    let device = device(name)?;
    let mut preferences = Preferences::default();
    let mut svg = false;
    let mut seen = std::collections::BTreeSet::new();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        if !seen.insert(option.as_str()) {
            return Err("duplicate preview option".into());
        }
        match option.as_str() {
            "--reduced-motion" => preferences.reduced_motion = true,
            "--high-contrast" => preferences.high_contrast = true,
            "--svg" => svg = true,
            "--brightness" => {
                preferences.brightness = options
                    .next()
                    .ok_or("brightness requires a value")?
                    .parse()
                    .map_err(|_| "brightness must be a finite number in 0..1")?;
            }
            _ => return Err("unknown preview option".into()),
        }
    }
    preferences.validate().map_err(|error| error.to_string())?;
    if svg && device != DeviceKind::Ajazz {
        return Err("SVG is available only for ajazz".into());
    }
    Ok(Command::Preview {
        device,
        preferences,
        svg,
    })
}

fn assignment(args: &[String]) -> Result<Command, String> {
    let mut root = None;
    let mut confirmation = None;
    let mut args = args.iter();
    while let Some(option) = args.next() {
        match option.as_str() {
            "--export-root" if root.is_none() => {
                root = Some(PathBuf::from(args.next().ok_or("export root is required")?));
            }
            "--confirm" if confirmation.is_none() => {
                confirmation = Some(args.next().ok_or("confirmation is required")?);
            }
            _ => return Err("unknown or duplicate assignment option".into()),
        }
    }
    match (&root, confirmation) {
        (None, None) => {}
        (Some(root), Some(confirmation)) => {
            if confirmation != "ASSIGN" {
                return Err("export confirmation must be exactly ASSIGN".into());
            }
            if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
                return Err("export root must be absolute without parent traversal".into());
            }
        }
        _ => return Err("export requires --export-root and --confirm ASSIGN".into()),
    }
    Ok(Command::Assign { export_root: root })
}

fn micro_reports(args: &[String]) -> Result<Command, String> {
    if !(1..=128).contains(&args.len()) {
        return Err("micro-decode requires 1..128 reports".into());
    }
    let reports = args
        .iter()
        .map(|text| {
            if text.len() != 128 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("Micro reports require exactly 128 ASCII hex digits".into());
            }
            text.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let digit = |byte: u8| match byte {
                        b'0'..=b'9' => byte - b'0',
                        b'a'..=b'f' => byte - b'a' + 10,
                        b'A'..=b'F' => byte - b'A' + 10,
                        _ => unreachable!("hex was validated"),
                    };
                    Ok((digit(pair[0]) << 4) | digit(pair[1]))
                })
                .collect()
        })
        .collect::<Result<_, String>>()?;
    Ok(Command::MicroDecode(reports))
}

/// Parse a complete invocation without reading input or consulting any backend.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some((name, args)) = args.split_first() else {
        return Ok(Command::Help);
    };
    match name.as_str() {
        "device-preview" => return preview(args),
        "device-assign" => return assignment(args),
        "micro-decode" => return micro_reports(args),
        "ct-probe" => return ct_probe::parse(args).map(Command::CtProbe),
        "micro-probe" | "akp03-probe" => {
            return hid_probe::parse(args, name == "micro-probe").map(|options| match options {
                hid_probe::Options::Micro(options) => Command::MicroProbe(options),
                hid_probe::Options::Akp03(options) => Command::Akp03Probe(options),
            });
        }
        "micro-firmware-plan" => {
            let mut image = None;
            let mut tool = None;
            let mut args = args.iter();
            while let Some(option) = args.next() {
                let target = match option.as_str() {
                    "--image" if image.is_none() => &mut image,
                    "--esptool" if tool.is_none() => &mut tool,
                    _ => return Err("unknown or duplicate firmware-plan option".into()),
                };
                let path = PathBuf::from(args.next().ok_or("artifact path is required")?);
                if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir)
                {
                    return Err("artifact paths must be absolute without parent traversal".into());
                }
                *target = Some(path);
            }
            return Ok(Command::FirmwarePlan {
                image: image.ok_or("--image is required")?,
                tool: tool.ok_or("--esptool is required")?,
            });
        }
        "akp03-decode" => {
            let mut profile = akp03::Profile::Original;
            let mut hex = None;
            let mut rev2 = false;
            for arg in args {
                if arg == "--rev2" && !rev2 {
                    rev2 = true;
                    profile = akp03::Profile::Rev2;
                } else if arg != "--rev2" && hex.is_none() {
                    hex = Some(arg.as_str());
                } else {
                    return Err("akp03-decode requires one hex report and optional --rev2".into());
                }
            }
            let report = akp03::input::parse_hex_report(hex.ok_or("hex report is required")?)
                .map_err(|error| error.to_string())?;
            akp03::input::decode_report(profile, &report).map_err(|error| error.to_string())?;
            return Ok(Command::Akp03Decode { profile, report });
        }
        "micro-doctor" | "akp03-doctor" => {
            let discover = match args {
                [] => false,
                [option] if option == "--discover" => true,
                _ => return Err("doctor accepts only optional --discover".into()),
            };
            return Ok(Command::DeviceDoctor {
                device: if name == "micro-doctor" {
                    DeviceKind::Micro
                } else {
                    DeviceKind::Ajazz
                },
                discover,
            });
        }
        _ => {}
    }
    no_args(args)?;
    match name.as_str() {
        "-h" | "--help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        "capabilities" => Ok(Command::Capabilities),
        "device-profiles" => Ok(Command::Profiles),
        "doctor" => Ok(Command::Doctor),
        "micro-preview" => Ok(Command::MicroPreview),
        "akp03-preview" => Ok(Command::Preview {
            device: DeviceKind::Ajazz,
            preferences: Preferences::default(),
            svg: true,
        }),
        "probe-health" => Ok(Command::Health),
        "micro-firmware-update" => Ok(Command::Unavailable("firmware CLI/native backend")),
        _ => Err("unsupported command; no action-delivery interface is available".into()),
    }
}

fn profile_report(device: DeviceKind) -> Value {
    let profile = profiles::default_profile(device);
    let descriptor = match device {
        DeviceKind::Micro => json!({
            "vendor_id": micro::enumeration::APPLICATION_VID,
            "product_id": micro::enumeration::APPLICATION_PID,
            "usage_page": micro::enumeration::USAGE_PAGE,
            "usage": micro::enumeration::USAGE,
            "report_size": micro::enumeration::REPORT_SIZE,
        }),
        DeviceKind::Ajazz => json!(akp03::Profile::Original.identity()),
        DeviceKind::Ct => json!({"product_id": 3, "transport": "serial"}),
    };
    json!({
        "profile": profile,
        "descriptor": descriptor,
        "prioritized_roles": profile.prioritized_controls(),
        "command_ready": false,
        "runtime_qualified": false,
        "blockers": [if device == DeviceKind::Ct && cfg!(windows) {
                        "explicit_probe_required"
                    } else {
                        "native_backend_unavailable"
                    }, "physical_acceptance_pending",
                     "independent_action_authority_required"],
    })
}

fn capabilities() -> Value {
    json!({
        "schema": 1,
        "read_only": true,
        "command_ready": false,
        "qualification": "foundation_only",
        "inventory": portable_inventory(),
        "offline_codecs": ["micro", "akp03-original", "akp03e-rev2", "ct-pid-0003"],
        "hardware_backend_available": cfg!(windows) || cfg!(all(target_os = "macos", target_arch = "aarch64", any(feature = "micro-hid", feature = "akp03-hid"))),
        "ct_serial_probe_available": cfg!(windows),
        "micro_hid_probe_available": cfg!(all(target_os = "macos", target_arch = "aarch64", feature = "micro-hid")),
        "akp03_hid_probe_available": cfg!(all(target_os = "macos", target_arch = "aarch64", feature = "akp03-hid")),
        "firmware_execution_available": false,
        "runtime_owner_acquired": false,
        "state_store_api_available": cfg!(any(target_os = "linux", target_os = "macos")),
        "firmware_offline_validation_available": cfg!(feature = "micro-firmware"),
        "build_features": {
            "akp03_hid": cfg!(feature = "akp03-hid"),
            "micro_hid": cfg!(feature = "micro-hid"),
            "micro_firmware": cfg!(feature = "micro-firmware"),
        },
        "limitations": ["Availability is compile-time support, not native evidence or hardware acceptance. Native CLI probing requires explicit opt-in and a contained Windows or macOS ARM64 worker; CT remains Windows-only.",
            "Micro/AKP03 live probes fail closed without device-specific features and exact native descriptor evidence. Windows HID bus properties are optional; exact interface numbers and raw descriptors remain unavailable. Windows HID requires a private contained-child capability. macOS has a child-side parent-lifetime watchdog, not kernel-enforced parent-death/escape containment; IOKit requires a private child capability and canonical same-executable parent.",
            "Micro/AKP03 health checks perform no device access and do not require HID features."],
    })
}

fn export(candidate: &CandidateAssignment, path: PathBuf) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec_pretty(candidate).map_err(|_| "candidate serialization failed")?;
    bytes.push(b'\n');
    let root = PrivateRoot::open(path).map_err(|error| error.to_string())?;
    root.check_mutation_support()
        .map_err(|error| error.to_string())?;
    let name = Name::new(EXPORT_FILENAME).map_err(|error| error.to_string())?;
    root.create_exclusive(&name, &bytes)
        .map_err(|error| error.to_string())
}

/// Execute an already validated offline route. Only assignment reads stdin,
/// only confirmed assignment export mutates files. Native probes launch
/// compiled-in contained diagnostic workers (Windows or macOS ARM64 HID).
/// HID health checks have a pure no-I/O path on other platforms; no live-I/O
/// fallback exists.
pub fn execute(command: Command, input: impl Read) -> Result<Output, String> {
    match command {
        Command::Help => Ok(Output::Text(HELP.into())),
        Command::Version => Ok(Output::Text(format!(
            "copilot-controller {}",
            env!("CARGO_PKG_VERSION")
        ))),
        Command::Capabilities => Ok(Output::Json(capabilities())),
        Command::Profiles => Ok(Output::Json(json!({
            "schema": 1, "read_only": true, "command_ready": false,
            "profiles": [profile_report(DeviceKind::Micro),
                         profile_report(DeviceKind::Ajazz),
                         profile_report(DeviceKind::Ct)],
        }))),
        Command::Preview {
            device,
            preferences,
            svg,
        } => {
            let realized = synthetic_demo(&profiles::default_profile(device), preferences)
                .map_err(|error| error.to_string())?;
            if svg {
                akp03_svg(&realized)
                    .map(Output::Svg)
                    .map_err(|error| error.to_string())
            } else {
                json_output(realized)
            }
        }
        Command::Assign { export_root } => {
            let candidate = recorded_assignment(input).map_err(|error| error.to_string())?;
            if let Some(path) = export_root {
                export(&candidate, path)?;
            }
            json_output(candidate)
        }
        Command::Doctor => {
            let serial_unavailable = json!({
                "status": "unavailable",
                "reason": "implicit_device_discovery_prohibited",
            });
            let epoch = crate::domain::RuntimeEpoch::fresh();
            let clock = match sample_monotonic(epoch) {
                Ok(sample) => json!({"status": "observed", "evidence": sample}),
                Err(error) => json!({"status": "unavailable", "evidence": error}),
            };
            let names = observation::ExecutableNames::new("copilot.exe", "WindowsTerminal.exe")
                .map_err(|error| error.to_string())?;
            let native_epoch = uuid::Uuid::parse_str(&epoch.to_string())
                .map_err(|_| "invalid generated diagnostic epoch")?;
            Ok(Output::Json(json!({
                "schema": 1, "read_only": true, "command_ready": false,
                "qualification": "foundation_only", "w2_qualified": false,
                "platform_probes": {
                    "clock": clock,
                    "current_process": observation::observe_current_process(native_epoch, names),
                    "processes": observation::observe_process_inventory(native_epoch, names),
                    "foreground": observation::observe_foreground(native_epoch, names),
                    "desktop": observation::observe_desktop(native_epoch),
                    "serial": serial_unavailable,
                },
                "device_capabilities": [profile_report(DeviceKind::Micro),
                    profile_report(DeviceKind::Ajazz), profile_report(DeviceKind::Ct)],
                "inventory": portable_inventory(),
                "limitations": ["No device discovery/opening, worker launch, provider calls or terminal actions.",
                    "Windows basename hints use copilot.exe and WindowsTerminal.exe; they are not authenticated host identities.",
                    "Hardware qualification, host authentication, lifecycle subscriptions and physical acceptance are unavailable.",
                    "State/recovery and shared-owner APIs are library-only; this command acquires no owner lease."],
            })))
        }
        Command::DeviceDoctor { device, discover } => {
            if discover {
                return Err(
                    "unavailable: metadata-only HID enumeration backend is not integrated".into(),
                );
            }
            Ok(Output::Json(json!({
                "schema": 1, "read_only": true, "command_ready": false,
                "qualification": "foundation_only", "discovery_performed": false,
                "device": profile_report(device), "capabilities": capabilities(),
            })))
        }
        Command::MicroDecode(reports) => {
            let mut decoder = micro::wire::Decoder::new();
            let mut messages = Vec::new();
            for report in reports {
                micro::enumeration::validate_report(&report).map_err(|error| error.to_string())?;
                if let Some(message) = decoder
                    .feed(&report, Duration::ZERO)
                    .map_err(|error| error.to_string())?
                {
                    messages.push(message);
                }
            }
            let mut output = format!(
                "{{\n  \"command_ready\": false,\n  \"incomplete_bytes\": {},\n  \"messages\": [",
                decoder.buffered_bytes().len()
            );
            for (index, message) in messages.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str("\n    ");
                output.push_str(
                    &message
                        .to_pretty_json()
                        .map_err(|error| error.to_string())?
                        .replace('\n', "\n    "),
                );
            }
            if !messages.is_empty() {
                output.push_str("\n  ");
            }
            output.push_str("],\n  \"read_only\": true\n}");
            Ok(Output::Text(output))
        }
        Command::MicroPreview => Ok(Output::Json(json!({
            "schema": 1, "read_only": true, "command_ready": false,
            "synthetic": true, "device_io_performed": false,
            "led_frame": micro::led::diagnostic_frame(),
        }))),
        Command::Akp03Decode { profile, report } => {
            let decoded =
                akp03::input::decode_report(profile, &report).map_err(|error| error.to_string())?;
            Ok(Output::Json(json!({
                "read_only": true, "command_ready": false,
                "profile": profile, "decoded": decoded,
            })))
        }
        Command::Health => {
            let result = worker::probe(
                &worker::DiagnosticRequest {
                    kind: "health".into(),
                    payload: Value::Null,
                },
                Duration::from_secs(3),
                &AtomicBool::new(false),
                &health_allowlist()?,
            )
            .map_err(|error| format!("unavailable or failed: {error}"))?;
            Ok(Output::Json(json!({
                "schema": 1, "read_only": true, "command_ready": false,
                "w2_qualified": false, "qualification": "foundation_only",
                "worker": result.worker, "health": result.payload,
            })))
        }
        Command::CtProbe(options) => ct_probe::execute(options),
        Command::MicroProbe(options) => hid_probe::execute(hid_probe::Options::Micro(options)),
        Command::Akp03Probe(options) => hid_probe::execute(hid_probe::Options::Akp03(options)),
        Command::FirmwarePlan { image, tool } => {
            if !cfg!(feature = "micro-firmware") {
                return Err(
                    "offline firmware validation failed: requires the micro-firmware feature"
                        .into(),
                );
            }
            let plan = firmware::offline_plan(&image, &tool).map_err(
                |_| "offline firmware validation failed; verify feature and pinned local artifacts",
            )?;
            json_output(plan)
        }
        Command::Unavailable(backend) => Err(format!("unavailable: {backend} is not integrated")),
    }
}

static HEALTH_CALLBACKS: [worker::DiagnosticCallback; 1] =
    [worker::DiagnosticCallback::new("health", health_callback)];

static MICRO_CALLBACKS: [worker::DiagnosticCallback; 1] = [worker::DiagnosticCallback::new(
    hid_probe::MICRO_KIND,
    hid_probe::micro_callback,
)];

static AKP03_CALLBACKS: [worker::DiagnosticCallback; 1] = [worker::DiagnosticCallback::new(
    hid_probe::AKP03_KIND,
    hid_probe::akp03_callback,
)];

static DIAGNOSTIC_CALLBACKS: [worker::DiagnosticCallback; 4] = [
    worker::DiagnosticCallback::new("health", health_callback),
    worker::DiagnosticCallback::new(ct_probe::KIND, ct_probe::callback),
    MICRO_CALLBACKS[0],
    AKP03_CALLBACKS[0],
];

/// Compiled diagnostic registry for the shared hidden worker, never health authority.
pub fn diagnostic_allowlist() -> Result<worker::DiagnosticAllowlist, String> {
    worker::DiagnosticAllowlist::new(&DIAGNOSTIC_CALLBACKS).map_err(|error| error.to_string())
}

/// Health-only authority; this registry cannot authorize a device probe.
pub fn health_allowlist() -> Result<worker::DiagnosticAllowlist, String> {
    worker::DiagnosticAllowlist::new(&HEALTH_CALLBACKS).map_err(|error| error.to_string())
}

/// Micro-only authority for the explicit HID coordinator.
pub fn micro_probe_allowlist() -> Result<worker::DiagnosticAllowlist, String> {
    worker::DiagnosticAllowlist::new(&MICRO_CALLBACKS).map_err(|error| error.to_string())
}

/// AKP03-only authority for its named hidden route and explicit coordinator.
pub fn akp03_probe_allowlist() -> Result<worker::DiagnosticAllowlist, String> {
    worker::DiagnosticAllowlist::new_for_route(
        &AKP03_CALLBACKS,
        worker::DiagnosticWorkerRoute::Akp03,
    )
    .map_err(|error| error.to_string())
}
fn health_callback(payload: Value) -> Result<Value, worker::DiagnosticCallbackError> {
    if !payload.is_null() {
        return Err(worker::DiagnosticCallbackError);
    }
    #[cfg(windows)]
    {
        use std::mem::size_of;
        use windows_sys::Win32::System::{
            Console::{GetConsoleProcessList, GetConsoleWindow},
            JobObjects::{
                IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                QueryInformationJobObject,
            },
            Threading::GetCurrentProcess,
        };
        let mut in_job = 0;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut process_ids = [0u32; 1];
        // SAFETY: all output buffers are sized live values; null Job selects
        // the current contained process's Job, and no device is accessed.
        let valid = unsafe {
            IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) != 0
                && QueryInformationJobObject(
                    std::ptr::null_mut(),
                    JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                ) != 0
                && GetConsoleWindow().is_null()
                && GetConsoleProcessList(process_ids.as_mut_ptr(), 1) == 0
        };
        let flags = limits.BasicLimitInformation.LimitFlags;
        if !valid
            || in_job == 0
            || limits.BasicLimitInformation.ActiveProcessLimit != 1
            || flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS == 0
            || flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE == 0
            || flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0
        {
            return Err(worker::DiagnosticCallbackError);
        }
        Ok(json!({
            "detached": true, "in_job": true, "active_process_limit": 1,
            "active_limit_set": true, "kill_on_close": true, "breakaway": false,
            "read_only": true, "command_ready": false, "w2_qualified": false,
        }))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        worker::worker_health()
    }
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    Err(worker::DiagnosticCallbackError)
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(windows, any(feature = "micro-hid", feature = "akp03-hid"))
))]
pub(crate) use hid_probe::validate_worker_options;
