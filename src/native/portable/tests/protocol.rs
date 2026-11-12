use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::super::protocol::{
    DiagnosticRequest, FrameError, MAX_FRAME_BODY, WireRequest, WireResponse, WorkerError,
    WorkerErrorKind, WorkerIdentity, WorkerOutcome, encode_frame, prepare_probe, read_frame,
    write_frame,
};

fn request(kind: &str) -> DiagnosticRequest {
    DiagnosticRequest {
        kind: kind.to_owned(),
        payload: json!({"arbitrary": {"field": 1}}),
    }
}

struct OneByte<'a>(&'a [u8]);

impl Read for OneByte<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.0.is_empty() {
            return Ok(0);
        }
        buffer[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}

struct ErrorAtEof<'a>(&'a [u8]);

impl Read for ErrorAtEof<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Err(io::Error::other("private error text"));
        }
        self.0.read(buffer)
    }
}

#[test]
fn frame_boundaries_truncation_trailing_bytes_and_partial_reads() {
    let value = json!({"test": [1, 2, 3]});
    let frame = encode_frame(&value).unwrap();
    assert_eq!(read_frame::<_, Value>(OneByte(&frame)).unwrap(), value);
    for length in [0_u32, 65_537, u32::MAX] {
        assert_eq!(
            read_frame::<_, Value>(&length.to_le_bytes()[..]),
            Err(FrameError::FrameLimit)
        );
    }
    assert_eq!(
        read_frame::<_, Value>(&[1, 0][..]),
        Err(FrameError::Transport)
    );
    assert_eq!(
        read_frame::<_, Value>(&[4, 0, 0, 0, b'{'][..]),
        Err(FrameError::Transport)
    );
    assert_eq!(
        read_frame::<_, Value>(&[1, 0, 0, 0, b'{'][..]),
        Err(FrameError::Protocol)
    );
    for suffix in [&b"\0"[..], &b" "[..], &frame[..]] {
        let mut trailing = frame.clone();
        trailing.extend_from_slice(suffix);
        assert_eq!(
            read_frame::<_, Value>(&trailing[..]),
            Err(FrameError::Protocol)
        );
    }
    assert_eq!(
        read_frame::<_, Value>(ErrorAtEof(&frame)),
        Err(FrameError::Transport)
    );
    let maximum = "a".repeat(MAX_FRAME_BODY - 2);
    let maximum_frame = encode_frame(&maximum).unwrap();
    assert_eq!(maximum_frame.len(), MAX_FRAME_BODY + 4);
    assert_eq!(
        read_frame::<_, String>(&maximum_frame[..]).unwrap(),
        maximum
    );
    assert_eq!(
        encode_frame(&"a".repeat(MAX_FRAME_BODY - 1)),
        Err(FrameError::FrameLimit)
    );
    assert_eq!(
        prepare_probe(
            Duration::from_secs(1),
            false,
            DiagnosticRequest {
                kind: "echo".into(),
                payload: json!("a".repeat(MAX_FRAME_BODY)),
            },
        )
        .unwrap_err()
        .kind(),
        WorkerErrorKind::FrameLimit
    );
}

struct FailingSerializer;

impl Serialize for FailingSerializer {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("private serializer text"))
    }
}

struct OwnedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    dropped: Arc<AtomicBool>,
}

impl Write for OwnedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for OwnedWriter {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn encoding_failure_is_bounded_and_owned_writer_is_closed() {
    assert_eq!(
        encode_frame(&FailingSerializer),
        Err(FrameError::FrameLimit)
    );
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let dropped = Arc::new(AtomicBool::new(false));
    let writer = OwnedWriter {
        bytes: Arc::clone(&bytes),
        dropped: Arc::clone(&dropped),
    };
    write_frame(writer, &json!({"ok": true})).unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        read_frame::<_, Value>(&bytes.lock().unwrap()[..]).unwrap(),
        json!({"ok": true})
    );
}

#[test]
fn request_schema_kind_validation_and_admission_order() {
    for kind in [
        "",
        "UPPER",
        "slash/part",
        "a b",
        "health --command",
        &"x".repeat(65),
    ] {
        assert_eq!(
            request(kind).validate(),
            Err(WorkerErrorKind::InvalidRequest)
        );
    }
    for kind in ["1", ".-_0", "windows_process"] {
        assert_eq!(request(kind).validate(), Ok(()));
    }
    let mut wire = WireRequest {
        version: 1,
        request_nonce: Uuid::from_u128(1),
        request: request("health"),
    };
    assert!(wire.validate().is_ok());
    wire.version = 2;
    assert_eq!(wire.validate(), Err(WorkerErrorKind::InvalidRequest));
    wire.version = 1;
    wire.request_nonce = Uuid::nil();
    assert_eq!(wire.validate(), Err(WorkerErrorKind::InvalidRequest));

    let extra = json!({"kind":"health","payload":{},"executable":"anything"});
    assert!(serde_json::from_value::<DiagnosticRequest>(extra).is_err());
    assert!(serde_json::from_value::<DiagnosticRequest>(json!(["health", {}])).is_err());
    let duplicate = r#"{"kind":"health","kind":"health","payload":{}}"#;
    assert!(serde_json::from_str::<DiagnosticRequest>(duplicate).is_err());
    let extra = json!({
        "version":1,"request_nonce":Uuid::from_u128(1).to_string(),
        "request":{"kind":"health","payload":{}},"extra":1
    });
    assert!(serde_json::from_value::<WireRequest>(extra).is_err());
    assert!(
        serde_json::from_value::<WireRequest>(json!([
            1,
            Uuid::from_u128(1).to_string(),
            {"kind":"health","payload":{}}
        ]))
        .is_err()
    );

    for timeout in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(120) + Duration::from_nanos(1),
    ] {
        let error = prepare_probe(timeout, true, request("invalid kind")).unwrap_err();
        assert_eq!(error.kind(), WorkerErrorKind::InvalidTimeout);
        assert!(error.replacement_allowed());
    }
    let error = prepare_probe(Duration::from_secs(1), true, request("invalid kind")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::Cancelled);
    assert!(error.replacement_allowed());
    let error = prepare_probe(Duration::from_secs(1), false, request("invalid kind")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::InvalidRequest);
    let prepared = prepare_probe(Duration::from_millis(1), false, request("health")).unwrap();
    let decoded: WireRequest = read_frame(prepared.frame()).unwrap();
    assert_eq!(decoded.request_nonce, prepared.request_nonce());
    assert_eq!(prepared.timeout(), Duration::from_millis(1));
    assert!(!prepared.request_nonce().is_nil());
    assert!(decoded.validate().is_ok());
    assert!(prepare_probe(Duration::from_secs(120), false, request("health")).is_ok());
}

#[test]
#[cfg(not(windows))]
fn non_windows_dispatch_checks_before_reporting_unsupported() {
    use super::super::protocol::unavailable_framed_worker;

    let error =
        unavailable_framed_worker(Duration::from_nanos(1), true, request("bad kind")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::InvalidTimeout);
    let error =
        unavailable_framed_worker(Duration::from_secs(1), true, request("bad kind")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::Cancelled);
    let error =
        unavailable_framed_worker(Duration::from_secs(1), false, request("bad kind")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::InvalidRequest);
    let error =
        unavailable_framed_worker(Duration::from_secs(1), false, request("health")).unwrap_err();
    assert_eq!(error.kind(), WorkerErrorKind::UnsupportedPlatform);
    assert!(error.replacement_allowed());
    assert_eq!(error.worker(), None);
}

#[test]
fn response_requires_exact_nonce_lifetime_and_schema() {
    let nonce = Uuid::from_u128(1);
    let worker = WorkerIdentity {
        pid: 7,
        creation_time_100ns: 123,
    };
    let response = WireResponse {
        version: 1,
        request_nonce: nonce,
        worker,
        outcome: WorkerOutcome::Ok {
            payload: json!({"extra_payload_field": true}),
        },
    };
    assert_eq!(response.validate_correlation(nonce, worker), Ok(()));
    for field in 0..4 {
        let mut changed = response.clone();
        match field {
            0 => changed.version += 1,
            1 => changed.request_nonce = Uuid::from_u128(2),
            2 => changed.worker.pid += 1,
            _ => changed.worker.creation_time_100ns += 1,
        }
        assert_eq!(
            changed.validate_correlation(nonce, worker),
            Err(WorkerErrorKind::Protocol)
        );
    }
    assert_eq!(
        response.validate_correlation(Uuid::nil(), worker),
        Err(WorkerErrorKind::Protocol)
    );
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["worker"]["extra"] = json!(1);
    assert!(serde_json::from_value::<WireResponse>(encoded).is_err());
    assert!(serde_json::from_value::<WorkerIdentity>(json!([7, 123])).is_err());
    assert!(
        serde_json::from_value::<WireResponse>(json!([
            1,
            nonce.to_string(),
            {"pid":7,"creation_time_100ns":123},
            {"status":"ok","payload":{}}
        ]))
        .is_err()
    );
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["extra"] = json!(1);
    assert!(serde_json::from_value::<WireResponse>(encoded).is_err());
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["outcome"]["extra"] = json!(1);
    assert!(serde_json::from_value::<WireResponse>(encoded).is_ok());
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["outcome"] = json!({"status": "callback_failed", "extra": 1});
    assert!(serde_json::from_value::<WireResponse>(encoded).is_ok());
}

#[test]
fn unsafe_cleanup_never_permits_worker_replacement() {
    for kind in [
        WorkerErrorKind::Busy,
        WorkerErrorKind::SupervisionBlocked,
        WorkerErrorKind::CleanupUnconfirmed,
    ] {
        assert!(!WorkerError::before_launch(kind).replacement_allowed());
    }
    let worker = WorkerIdentity {
        pid: 7,
        creation_time_100ns: 123,
    };
    let error = WorkerError::after_launch(WorkerErrorKind::TimedOut, Some(worker), false);
    assert_eq!(error.kind(), WorkerErrorKind::CleanupUnconfirmed);
    assert_eq!(error.worker(), Some(worker));
    assert!(!error.replacement_allowed());
    assert_eq!(error.to_string(), "diagnostic worker: CleanupUnconfirmed");
    let cleaned = WorkerError::after_launch(WorkerErrorKind::Cancelled, Some(worker), true);
    assert!(cleaned.replacement_allowed());
    assert_eq!(cleaned.kind(), WorkerErrorKind::Cancelled);
    let encoded = serde_json::to_value(error).unwrap();
    assert_eq!(encoded["replacement_allowed"], false);
}
