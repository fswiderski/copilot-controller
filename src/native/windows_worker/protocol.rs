#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
use std::io::Read;
use std::io::{self, Write};

use serde::Serialize;
#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
use serde::{Deserialize, de::DeserializeOwned};
#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
use serde_json::Value;
use uuid::Uuid;

#[cfg(any(windows, test))]
use super::MAX_DIAGNOSTIC_BYTES;
#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
use super::{DiagnosticAllowlist, DiagnosticCallbackFn, WorkerIdentity};
use super::{DiagnosticRequest, MAX_FRAME_BODY_BYTES, WorkerErrorKind};

pub(super) const VERSION: u32 = 1;

#[derive(Serialize)]
pub(super) struct RequestEnvelopeRef<'a> {
    pub version: u32,
    pub request_nonce: Uuid,
    pub request: &'a DiagnosticRequest,
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestEnvelope {
    pub version: u32,
    pub request_nonce: Uuid,
    pub request: DiagnosticRequest,
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResponseEnvelope {
    pub version: u32,
    pub request_nonce: Uuid,
    pub worker: WorkerIdentity,
    pub outcome: Outcome,
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum Outcome {
    Ok { payload: Value },
    CallbackFailed,
    ResponseEncodingFailed,
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
pub(super) fn invoke_callback(callback: DiagnosticCallbackFn, payload: Value) -> Outcome {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(payload))) {
        Ok(Ok(payload)) => Outcome::Ok { payload },
        Ok(Err(_)) => Outcome::CallbackFailed,
        Err(panic_payload) => {
            // A panic payload can itself panic on drop; the worker exits after
            // writing the generic response, so leave reclamation to process exit.
            std::mem::forget(panic_payload);
            Outcome::CallbackFailed
        }
    }
}

pub(super) fn valid_kind(kind: &str) -> bool {
    let bytes = kind.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
pub(super) fn validate_request(
    envelope: &RequestEnvelope,
    allowlist: &DiagnosticAllowlist,
) -> Result<(), WorkerErrorKind> {
    if envelope.version != VERSION
        || envelope.request_nonce.is_nil()
        || !valid_kind(&envelope.request.kind)
        || allowlist.find(&envelope.request.kind).is_none()
    {
        return Err(WorkerErrorKind::InvalidRequest);
    }
    Ok(())
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
pub(super) fn validate_response(
    envelope: ResponseEnvelope,
    nonce: Uuid,
    identity: WorkerIdentity,
) -> Result<Outcome, WorkerErrorKind> {
    if envelope.version != VERSION || envelope.request_nonce != nonce || envelope.worker != identity
    {
        return Err(WorkerErrorKind::Protocol);
    }
    Ok(envelope.outcome)
}

struct BoundedFrame {
    bytes: Vec<u8>,
}

impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let available = MAX_FRAME_BODY_BYTES + 4 - self.bytes.len();
        if bytes.len() > available {
            return Err(io::Error::other("frame limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, WorkerErrorKind> {
    let mut frame = BoundedFrame { bytes: vec![0; 4] };
    serde_json::to_writer(&mut frame, value).map_err(|_| WorkerErrorKind::FrameLimit)?;
    let body_len = frame.bytes.len() - 4;
    if body_len == 0 || body_len > MAX_FRAME_BODY_BYTES {
        return Err(WorkerErrorKind::FrameLimit);
    }
    frame.bytes[..4].copy_from_slice(&(body_len as u32).to_le_bytes());
    Ok(frame.bytes)
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
pub(super) fn encode_response(
    outcome: &impl Serialize,
    request_nonce: Uuid,
    worker: WorkerIdentity,
) -> Result<Vec<u8>, WorkerErrorKind> {
    #[derive(Serialize)]
    struct Envelope<'a, T> {
        version: u32,
        request_nonce: Uuid,
        worker: WorkerIdentity,
        outcome: &'a T,
    }
    encode_frame(&Envelope {
        version: VERSION,
        request_nonce,
        worker,
        outcome,
    })
    .or_else(|_| {
        // Encoding finishes before writing any stdout bytes. Even an oversized
        // callback gets a correlated compact failure instead of unframed EOF.
        encode_frame(&Envelope {
            version: VERSION,
            request_nonce,
            worker,
            outcome: &Outcome::ResponseEncodingFailed,
        })
    })
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
pub(super) fn read_frame<R: Read, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, WorkerErrorKind> {
    let mut header = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| WorkerErrorKind::Transport)?;
    let body_len = u32::from_le_bytes(header) as usize;
    if body_len == 0 || body_len > MAX_FRAME_BODY_BYTES {
        return Err(WorkerErrorKind::FrameLimit);
    }
    let mut body = vec![0; body_len];
    reader
        .read_exact(&mut body)
        .map_err(|_| WorkerErrorKind::Transport)?;

    let mut trailing = [0; 1];
    loop {
        match reader.read(&mut trailing) {
            Ok(0) => break,
            Ok(_) => return Err(WorkerErrorKind::Protocol),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WorkerErrorKind::Transport),
        }
    }
    // Value discards duplicate object keys. Reject them while the framed bytes
    // still exist, before typed callback validation can authorize device I/O.
    serde_json::from_slice::<UniqueKeys>(&body).map_err(|_| WorkerErrorKind::Protocol)?;
    serde_json::from_slice(&body).map_err(|_| WorkerErrorKind::Protocol)
}

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
struct UniqueKeys;

#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
impl<'de> Deserialize<'de> for UniqueKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueKeys;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<UniqueKeys, M::Error> {
                let mut keys = std::collections::BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    map.next_value::<UniqueKeys>()?;
                }
                Ok(UniqueKeys)
            }

            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: S,
            ) -> Result<UniqueKeys, S::Error> {
                while sequence.next_element::<UniqueKeys>()?.is_some() {}
                Ok(UniqueKeys)
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }

            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(any(windows, test))]
pub(super) fn drain_diagnostics<R: Read>(reader: &mut R) -> Result<(), WorkerErrorKind> {
    let mut total = 0usize;
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if count > MAX_DIAGNOSTIC_BYTES - total {
                    return Err(WorkerErrorKind::DiagnosticLimit);
                }
                total += count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WorkerErrorKind::Transport),
        }
    }
}
