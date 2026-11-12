use serde::Serialize;
use serde_json::Value;

use super::json::{category_c, numeric, str_value};
use super::led::LedStatus;
use super::value::{JsonValue, parse_json};

const RESTART: &str = "Restart the controller and check its local status.";
const USB_GUIDANCE: &str = "Check the data cable and intended application layer, close competing owners, and let the single controller retry.";
const DPAD_GUIDANCE: &str = "Ensure the controller and USB are ready before calibration; enable command controls for navigation.";
const ERROR_GUIDANCE: &str = "Do not blindly retry Send or Accept; inspect the intended selected/foreground session and actual dialog/draft, and consult the local log.";

#[derive(Debug, Clone, Copy)]
pub enum SnapshotInput<'a> {
    Missing,
    Unreadable(&'a str),
    Bytes(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub level: Level,
    pub detail: String,
    pub action: String,
}

impl Check {
    fn new(name: &str, level: Level, detail: String, action: &str) -> Self {
        Self {
            name: name.to_owned(),
            level,
            detail: sanitize_detail(&detail),
            action: action.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReport {
    pub controller_fresh: bool,
    pub connected: Option<bool>,
    pub checks: Vec<Check>,
}

impl HealthReport {
    /// Snapshot/configuration readiness is not command or hardware authority.
    pub fn configuration_ready(&self, configuration_loaded: bool) -> bool {
        configuration_loaded
            && self.controller_fresh
            && self.connected == Some(true)
            && self.checks.iter().all(|check| check.level != Level::Error)
    }
}

pub fn sanitize_detail(detail: &str) -> String {
    detail
        .chars()
        .map(|character| {
            if category_c(character) {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn read_snapshot(input: SnapshotInput<'_>) -> Result<JsonValue, String> {
    match input {
        SnapshotInput::Missing => Ok(JsonValue::from_value(Value::Object(Default::default()))),
        SnapshotInput::Unreadable(error) => Err(format!("Could not read status.json: {error}")),
        SnapshotInput::Bytes(bytes) => {
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            let text = std::str::from_utf8(bytes)
                .map_err(|_| "status.json is not valid UTF-8".to_owned())?;
            parse_json(text.as_bytes()).map_err(|_| "status.json is not valid JSON".to_owned())
        }
    }
}

struct ValidStatus<'a> {
    heartbeat: f64,
    connected: bool,
    stopped: bool,
    stop_reason: Option<&'a Value>,
    last_error: Option<&'a str>,
}

fn validate_status(value: &Value) -> Result<ValidStatus<'_>, &'static str> {
    let object = value.as_object().ok_or("status.json must be an object")?;
    let heartbeat = object
        .get("heartbeat")
        .and_then(numeric)
        .filter(|value| *value > 0.0)
        .ok_or("status.json has no valid heartbeat")?;
    let connected = match object.get("connected") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("status.json connected must be true or false"),
    };
    let stopped = match object.get("stopped") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("status.json stopped must be true or false"),
    };
    if let Some(bindings) = object.get("bindings") {
        let bindings = bindings
            .as_object()
            .ok_or("status.json bindings must be an object")?;
        for binding in bindings.values() {
            let health = binding
                .as_object()
                .and_then(|binding| binding.get("health"))
                .and_then(Value::as_object)
                .ok_or("status.json contains invalid binding health")?;
            if health
                .get("live")
                .is_some_and(|live| !matches!(live, Value::Bool(_)))
            {
                return Err("status.json contains invalid binding health");
            }
        }
    }
    if let Some(slots) = object.get("slots") {
        let slots = slots
            .as_array()
            .filter(|slots| slots.len() <= 6)
            .ok_or("status.json contains invalid slot states")?;
        if slots
            .iter()
            .any(|slot| slot.as_str().and_then(LedStatus::from_name).is_none())
        {
            return Err("status.json contains invalid slot states");
        }
    }
    let last_error = match object.get("last_error") {
        None | Some(Value::Null) => None,
        Some(Value::String(error)) => Some(error.as_str()),
        Some(_) => return Err("status.json last_error must be text"),
    };
    Ok(ValidStatus {
        heartbeat,
        connected,
        stopped,
        stop_reason: object.get("stop_reason"),
        last_error,
    })
}

/// Evaluates only supplied snapshot bytes and supplied wall-clock time.
pub fn snapshot_health(
    input: SnapshotInput<'_>,
    wall_now: f64,
    dpad_calibrated: Option<bool>,
) -> HealthReport {
    let mut checks = Vec::new();
    let snapshot = read_snapshot(input);
    let valid = snapshot
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|value| validate_status(value.as_value()).map_err(str::to_owned));
    let (fresh, connected, last_error) = match valid {
        Err(error) => {
            checks.push(Check::new(
                "controller",
                Level::Error,
                error.to_owned(),
                RESTART,
            ));
            (false, None, None)
        }
        Ok(status) => {
            let age = wall_now - status.heartbeat;
            let fresh = !status.stopped && (0.0..10.0).contains(&age);
            let detail = if fresh {
                format!("Current; heartbeat age {age:.1}s.")
            } else {
                let mut detail =
                    format!("Stopped, stale or clock changed; heartbeat age {age:.1}s.");
                if status.stopped {
                    let reason = status
                        .stop_reason
                        .map_or_else(|| "not recorded".to_owned(), str_value);
                    detail.push_str(&format!(" Stop reason: {reason}."));
                }
                detail
            };
            checks.push(Check::new(
                "controller",
                if fresh { Level::Ok } else { Level::Error },
                detail,
                if fresh { "" } else { RESTART },
            ));
            (
                fresh,
                fresh.then_some(status.connected),
                status.last_error.filter(|error| !error.is_empty()),
            )
        }
    };

    let (level, detail) = match connected {
        Some(true) => (Level::Ok, "Connected in the latest controller snapshot."),
        Some(false) => (Level::Warning, "Not connected in the latest snapshot."),
        None => (Level::Warning, "Unknown until the controller is current."),
    };
    checks.push(Check::new("usb", level, detail.to_owned(), USB_GUIDANCE));
    if let Some(calibrated) = dpad_calibrated {
        let (level, detail) = if calibrated {
            (
                Level::Ok,
                "Four directions saved; physical accuracy is not checked.",
            )
        } else {
            (Level::Warning, "Not fully calibrated.")
        };
        checks.push(Check::new("dpad", level, detail.to_owned(), DPAD_GUIDANCE));
    }
    if let Some(error) = last_error {
        let prefix = if fresh {
            "Latest snapshot: "
        } else {
            "Historical snapshot (not current): "
        };
        checks.push(Check::new(
            "last reported error",
            Level::Warning,
            format!("{prefix}{error}"),
            ERROR_GUIDANCE,
        ));
    }
    HealthReport {
        controller_fresh: fresh,
        connected,
        checks,
    }
}

fn powershell_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.to_string().parse::<f64>().is_ok_and(|n| n != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Object(_) => true,
        Value::Array(values) => match values.as_slice() {
            [] => false,
            [Value::Array(nested)] => !nested.is_empty(),
            [single] => powershell_truthy(single),
            _ => true,
        },
    }
}

fn calibration_heartbeat(value: &Value) -> Option<f64> {
    let Value::Number(number) = value else {
        return None;
    };
    let raw = number.to_string();
    if raw.contains(['.', 'e', 'E']) {
        let value = raw.parse::<f64>().ok()?;
        value.is_finite().then_some(value)
    } else {
        raw.parse::<i64>().ok().map(|value| value as f64)
    }
}

/// Deliberately separate from full health validation, matching the calibration gate.
pub fn calibration_precondition(
    process_running: bool,
    status_error: Option<&str>,
    status: &Value,
    wall_now: f64,
) -> bool {
    if !process_running || status_error.is_some_and(|error| !error.is_empty()) {
        return false;
    }
    let Some(status) = status.as_object() else {
        return false;
    };
    if status.get("connected") != Some(&Value::Bool(true))
        || status.get("stopped").is_some_and(powershell_truthy)
    {
        return false;
    }
    let Some(heartbeat) = status.get("heartbeat").and_then(calibration_heartbeat) else {
        return false;
    };
    (0.0..10.0).contains(&(wall_now - heartbeat))
}
