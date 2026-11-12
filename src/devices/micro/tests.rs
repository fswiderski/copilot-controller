use std::time::Duration;

use serde_json::{Value, json};

use super::calibration::{Calibration, CalibrationError, Direction};
use super::enumeration::{self, Capabilities};
use super::error::{Disposition, MicroError, render_diagnostic};
use super::health::{self, Level, SnapshotInput};
use super::input::{
    self, Action, Bindings, CommandDisposition, CommandKey, LegacyContext, LegacyDecoder, Slot,
    StrictEventKind,
};
use super::led::{self, LedStatus};
use super::rpc::{self, RpcClient};
use super::wire::{self, Decoder, Report};

fn ms(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

fn payload(reports: &[Report]) -> Vec<u8> {
    reports
        .iter()
        .flat_map(|report| report[3..3 + usize::from(report[2])].iter().copied())
        .collect()
}

fn frame(channel: u8, bytes: &[u8]) -> Vec<u8> {
    let mut packet = vec![wire::REPORT_ID, channel, bytes.len() as u8];
    packet.extend_from_slice(bytes);
    packet
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().expect("test object").clone()
}

fn confirm_all(client: &mut RpcClient, request: &rpc::PreparedRequest, time: Duration) {
    for (index, _) in request.reports.iter().enumerate() {
        let deadline = client.confirm_write(64, time).unwrap();
        assert_eq!(
            deadline,
            (index + 1 == request.reports.len()).then_some(time + Duration::from_secs(4))
        );
    }
}

#[test]
fn golden_version_request_and_chunked_utf8_roundtrip() {
    let mut client = RpcClient::new();
    let request = client.begin("sys.version", &Value::Null).unwrap();
    let golden = br#"{"method":"sys.version","params":null,"id":101}"#;
    assert_eq!(golden.len(), 47);
    assert_eq!(request.id, 101);
    assert_eq!(request.reports.len(), 1);
    assert_eq!(&request.reports[0][..3], &[6, 2, 47]);
    assert_eq!(&request.reports[0][3..50], golden);
    assert_eq!(&request.reports[0][50..], &[0; 14]);

    let sample = json!({"result":format!("{}{}", "x".repeat(59), "☃".repeat(100))});
    let reports = wire::encode_json(&sample).unwrap();
    assert!(reports.len() > 5);
    assert!(reports.iter().all(|report| {
        report[0] == 6
            && report[1] == 2
            && report[2] <= 61
            && report[3 + usize::from(report[2])..].iter().all(|b| *b == 0)
    }));
    assert!(
        reports
            .iter()
            .any(|report| std::str::from_utf8(&report[3..3 + usize::from(report[2])]).is_err()),
        "the test must split a UTF-8 code point"
    );
    let mut decoder = Decoder::new();
    let mut decoded = None;
    for report in &reports {
        decoded = decoder.feed(report, ms(1000)).unwrap().or(decoded);
    }
    assert_eq!(decoded.unwrap().as_value(), &sample);
    assert!(decoder.buffered_bytes().is_empty());
}

#[test]
fn frame_validation_age_and_decoder_recovery() {
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.feed(&[6, 3, 0], ms(0)),
        Err(MicroError::UnknownChannel)
    );
    for invalid in [&[5, 2, 0][..], &[6, 2, 62], &[6, 2, 2, b'x'], &[6, 2]] {
        assert_eq!(decoder.feed(invalid, ms(0)), Err(MicroError::InvalidFrame));
    }
    assert_eq!(
        decoder.feed(&[6, 3, 62], ms(0)),
        Err(MicroError::InvalidFrame),
        "validate framing before the channel"
    );
    assert_eq!(decoder.feed(&frame(2, b"\r\n"), ms(0)).unwrap(), None);
    assert_eq!(decoder.started_at(), None);
    assert!(
        decoder
            .feed(&frame(2, br#"{"ok":true}"#), ms(10_000))
            .unwrap()
            .is_some()
    );

    assert_eq!(
        decoder.feed(&frame(2, br#"{"id""#), ms(10_000)).unwrap(),
        None
    );
    assert_eq!(decoder.feed(&frame(1, b"log"), ms(99_000)).unwrap(), None);
    assert_eq!(decoder.feed(&frame(2, b":"), ms(15_000)).unwrap(), None);
    assert_eq!(
        decoder.feed(&frame(2, b"1}"), ms(15_001)),
        Err(MicroError::AccumulationLimit)
    );
    assert_eq!(decoder.buffered_bytes(), br#"{"id":1}"#);
    assert_eq!(decoder.started_at(), Some(ms(10_000)));

    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.feed(&frame(2, b"[1]"), ms(0)),
        Err(MicroError::NonObjectMessage)
    );
    assert!(decoder.buffered_bytes().is_empty());
    assert_eq!(decoder.started_at(), None);
    assert!(decoder.feed(&frame(2, b"{}"), ms(0)).unwrap().is_some());

    for malformed in [br#"{"a":NaN}"#.as_slice(), b"{}{}"] {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&frame(2, malformed), ms(0)).unwrap(), None);
        assert_eq!(decoder.buffered_bytes(), malformed);
    }
    let mut decoder = Decoder::new();
    decoder.feed(&frame(2, b" "), ms(10_000)).unwrap();
    assert_eq!(decoder.started_at(), Some(ms(10_000)));
    decoder.feed(&frame(2, b"{}"), ms(9_000)).unwrap();
    assert!(
        decoder.buffered_bytes().is_empty(),
        "backward elapsed time saturates to zero"
    );
}

#[test]
fn json_surrogates_are_preserved_and_decoder_accepts_following_responses() {
    let unpaired = super::value::parse_json(br#""\ud800""#).unwrap();
    assert_eq!(super::json::repr_value(unpaired.as_value()), r#"'\ud800'"#);
    let unpaired_low = super::value::parse_json(br#""\udfff""#).unwrap();
    assert_eq!(
        super::json::repr_value(unpaired_low.as_value()),
        r#"'\udfff'"#
    );
    let mut encoded = String::new();
    super::json::append_json(&mut encoded, unpaired.as_value()).unwrap();
    assert_eq!(encoded, r#""\ud800""#);

    let private_use_after_surrogate = super::value::parse_json(br#""\ud800\ue000""#).unwrap();
    assert_eq!(
        super::json::repr_value(private_use_after_surrogate.as_value()),
        r#"'\ud800\ue000'"#
    );
    let mut encoded = String::new();
    super::json::append_json(&mut encoded, private_use_after_surrogate.as_value()).unwrap();
    assert_eq!(
        super::value::parse_json(encoded.as_bytes()).unwrap(),
        private_use_after_surrogate
    );

    let literal_marker = json!("\u{fdd0}sD800");
    let reports = wire::encode_json(&literal_marker).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&payload(&reports)).unwrap(),
        literal_marker
    );

    let paired = super::value::parse_json(br#""\ud83d\ude00""#).unwrap();
    assert_eq!(paired.as_value(), &Value::String("😀".to_owned()));

    let mut decoder = Decoder::new();
    let first = frame(2, br#"{"id":101,"result":"\ud800"}"#);
    let first = decoder.feed(&first, ms(0)).unwrap().unwrap();
    assert_eq!(
        super::json::repr_value(first.get("result").unwrap()),
        r#"'\ud800'"#
    );

    let second = frame(2, br#"{"id":102,"result":"next"}"#);
    let second = decoder.feed(&second, ms(1)).unwrap().unwrap();
    assert_eq!(second.get("id"), Some(&json!(102)));
    assert_eq!(second.get("result"), Some(&json!("next")));
    assert!(decoder.buffered_bytes().is_empty());
}

#[test]
fn replay_json_roundtrips_surrogates_and_formats_objects_prettily() {
    let parsed =
        super::value::parse_json(br#"{"high":"\ud800","low":"\udfff","pair":"\ud83d\ude00"}"#)
            .unwrap();
    let compact = parsed.to_json().unwrap();
    assert!(compact.contains(r#""high":"\ud800""#), "{compact:?}");
    assert!(compact.contains(r#""low":"\udfff""#), "{compact:?}");
    assert!(compact.contains(r#""pair":"😀""#), "{compact:?}");
    assert_eq!(
        super::value::parse_json(compact.as_bytes()).unwrap(),
        parsed
    );

    let object = parsed.into_object().unwrap();
    assert_eq!(object.to_json().unwrap(), compact);
    let pretty = object.to_pretty_json().unwrap();
    assert_eq!(
        pretty,
        "{\n  \"high\": \"\\ud800\",\n  \"low\": \"\\udfff\",\n  \"pair\": \"😀\"\n}"
    );
    let roundtrip = super::value::parse_json(pretty.as_bytes()).unwrap();
    assert_eq!(roundtrip.as_value(), object.as_value());

    let literal_marker = super::value::JsonValue::from_value(json!("\u{fdd0}sD800"));
    let replay = literal_marker.to_json().unwrap();
    let decoded: Value = serde_json::from_str(&replay).unwrap();
    assert_eq!(&decoded, literal_marker.as_value());

    let nested = super::value::JsonValue::from_value(json!({"items":[1,{"ok":true}]}));
    assert_eq!(
        nested.to_pretty_json().unwrap(),
        "{\n  \"items\": [\n    1,\n    {\n      \"ok\": true\n    }\n  ]\n}"
    );
}

#[test]
fn replay_json_enforces_depth_and_output_size_limits() {
    use super::value::{
        JsonSerializeError, JsonValue, MAX_REPLAY_JSON_BYTES, MAX_REPLAY_JSON_DEPTH,
    };

    let mut at_limit = Value::Null;
    for _ in 0..MAX_REPLAY_JSON_DEPTH {
        at_limit = Value::Array(vec![at_limit]);
    }
    let at_limit = JsonValue::from_value(at_limit);
    assert!(at_limit.to_json().is_ok());

    let mut too_deep = Value::Null;
    for _ in 0..=MAX_REPLAY_JSON_DEPTH {
        too_deep = Value::Array(vec![too_deep]);
    }
    let too_deep = JsonValue::from_value(too_deep);
    assert_eq!(too_deep.to_json(), Err(JsonSerializeError::DepthLimit));

    let at_size_limit = JsonValue::from_value(Value::String("x".repeat(MAX_REPLAY_JSON_BYTES - 2)));
    assert_eq!(
        at_size_limit.to_json().unwrap().len(),
        MAX_REPLAY_JSON_BYTES
    );
    let over_size_limit =
        JsonValue::from_value(Value::String("x".repeat(MAX_REPLAY_JSON_BYTES - 1)));
    assert_eq!(
        over_size_limit.to_json(),
        Err(JsonSerializeError::SizeLimit)
    );

    let oversized_number = format!("1{}", "0".repeat(MAX_REPLAY_JSON_BYTES));
    let number: Value = serde_json::from_str(&oversized_number).unwrap();
    assert_eq!(
        JsonValue::from_value(number).to_json(),
        Err(JsonSerializeError::SizeLimit)
    );
}

#[test]
fn json_value_equality_respects_surrogate_token_provenance() {
    use super::value::JsonValue;

    let parsed = super::value::parse_json(br#""\ud800""#).unwrap();
    let literal = JsonValue::from_value(json!("\u{fdd0}sd800"));

    assert_eq!(parsed.as_value(), literal.as_value());
    assert_ne!(parsed, literal);
    assert!(!parsed.eq(literal.as_value()));
    assert_eq!(parsed.to_json().unwrap(), r#""\ud800""#);

    let replayed_literal = literal.to_json().unwrap();
    let decoded: Value = serde_json::from_str(&replayed_literal).unwrap();
    assert_eq!(&decoded, literal.as_value());

    let parsed_marker = super::value::parse_json(br#""\ufdd0""#).unwrap();
    let literal_markers = JsonValue::from_value(json!("\u{fdd0}\u{fdd0}"));
    assert_eq!(parsed_marker.as_value(), literal_markers.as_value());
    assert_ne!(parsed_marker, literal_markers);
    assert_ne!(parsed_marker.to_json(), literal_markers.to_json());

    let parsed_text = super::value::parse_json(br#""ordinary""#).unwrap();
    assert_eq!(parsed_text, JsonValue::from_value(json!("ordinary")));
}

#[test]
fn json_value_equality_decodes_literal_markers_across_provenance() {
    use super::value::{JsonValue, parse_json};

    for text in [
        "\u{fdd0}",
        "\u{fdd0}\u{fdd0}",
        "\u{fdd0}sd800",
        "a\u{fdd0}z",
    ] {
        let plain = json!(text);
        let parsed = parse_json(serde_json::to_string(&plain).unwrap().as_bytes()).unwrap();
        let literal = JsonValue::from_value(plain);
        assert_eq!(parsed, literal);
        assert_eq!(literal, parsed);
        assert!(parsed.eq(literal.as_value()));
        assert_eq!(
            serde_json::from_str::<Value>(&parsed.to_json().unwrap()).unwrap(),
            *literal.as_value()
        );
    }

    let escaped = parse_json(br#""\ufdd0""#).unwrap();
    assert_eq!(escaped, JsonValue::from_value(json!("\u{fdd0}")));
    assert!(escaped.eq(&json!("\u{fdd0}")));
}

#[test]
fn json_value_equality_decodes_object_keys_across_provenance() {
    use super::value::{JsonValue, parse_json};

    let plain = json!({
        "\u{fdd0}": ["\u{fdd0}", {"\u{fdd0}sd800": true}],
        "\u{fdd0}\u{fdd0}": 2,
        "\u{fdd0}sd800": 3,
        "ordinary": null
    });
    let parsed = parse_json(serde_json::to_string(&plain).unwrap().as_bytes()).unwrap();
    let literal = JsonValue::from_value(plain);
    assert_eq!(parsed, literal);
    assert_eq!(literal, parsed);
    assert!(parsed.eq(literal.as_value()));
    assert_eq!(
        serde_json::from_str::<Value>(&parsed.to_json().unwrap()).unwrap(),
        *literal.as_value()
    );
    assert_eq!(
        parsed.into_object().unwrap(),
        literal.into_object().unwrap()
    );

    let surrogate_key = parse_json(br#"{"\ud800":1}"#).unwrap();
    let literal_key = JsonValue::from_value(json!({"\u{fdd0}sd800": 1}));
    assert_eq!(surrogate_key.as_value(), literal_key.as_value());
    assert_ne!(surrogate_key, literal_key);
    assert_ne!(literal_key, surrogate_key);
    assert!(!surrogate_key.eq(literal_key.as_value()));
    assert_eq!(surrogate_key.to_json().unwrap(), r#"{"\ud800":1}"#);
}

#[test]
fn decoder_size_boundary_retains_overflow_and_empty_chunks_do_not_check_age() {
    let mut decoder = Decoder::new();
    let data = [b'x'; 61];
    let chunks = wire::MAX_BUFFERED_BYTES / data.len();
    for _ in 0..chunks {
        assert!(decoder.feed(&frame(2, &data), ms(0)).unwrap().is_none());
    }
    let remaining = wire::MAX_BUFFERED_BYTES - chunks * data.len();
    decoder.feed(&frame(2, &data[..remaining]), ms(0)).unwrap();
    assert_eq!(decoder.buffered_bytes().len(), wire::MAX_BUFFERED_BYTES);
    assert_eq!(decoder.feed(&frame(2, b""), ms(90_000)).unwrap(), None);
    assert_eq!(
        decoder.feed(&frame(2, b"x"), ms(0)),
        Err(MicroError::AccumulationLimit)
    );
    assert_eq!(decoder.buffered_bytes().len(), wire::MAX_BUFFERED_BYTES + 1);
}

#[test]
fn deeply_nested_json_is_decoded_rendered_and_dropped_without_recursion() {
    let depth = 5000;
    let arrays = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
    let message = format!(r#"{{"id":101,"result":{arrays}}}"#);
    let reports = wire::frame_bytes(message.as_bytes());
    let mut client = RpcClient::new();
    let request = client.begin("device.status", &Value::Null).unwrap();
    confirm_all(&mut client, &request, ms(0));
    let mut result = None;
    for report in &reports {
        result = client.feed_report(report, ms(1)).unwrap().or(result);
    }
    let result = result.unwrap();
    let mut node = result.as_value();
    for _ in 0..depth {
        node = &node.as_array().unwrap()[0];
    }
    assert_eq!(node, &json!(0));
    assert_eq!(
        String::from_utf8(payload(&wire::encode_json(result.as_value()).unwrap())).unwrap(),
        arrays
    );
    assert_eq!(render_diagnostic(result.as_value()), arrays);
    drop(result);

    let snapshot = format!(r#"{{"heartbeat":99,"connected":true,"extra":{arrays}}}"#);
    assert!(
        health::snapshot_health(SnapshotInput::Bytes(snapshot.as_bytes()), 100.0, None)
            .configuration_ready(true)
    );
    let replaced = format!(r#"{{"x":{arrays},"x":0}}"#);
    let parsed = super::value::parse_json(replaced.as_bytes()).unwrap();
    assert_eq!(parsed.as_value()["x"], json!(0));
    let equal = super::value::parse_json(snapshot.as_bytes()).unwrap();
    let other = super::value::parse_json(snapshot.as_bytes()).unwrap();
    assert_eq!(equal, other);

    let mut decoder = Decoder::new();
    let frames = wire::frame_bytes(arrays.as_bytes());
    for frame in &frames[..frames.len() - 1] {
        assert!(decoder.feed(frame, ms(0)).unwrap().is_none());
    }
    assert_eq!(
        decoder.feed(frames.last().unwrap(), ms(0)),
        Err(MicroError::NonObjectMessage)
    );
    assert!(decoder.buffered_bytes().is_empty());
}

#[test]
fn json_parser_preserves_strings_numbers_and_duplicate_object_keys() {
    let vectors = [
        r#"{"z":[null,true,false,1,-2,3.0,1e-5],"a":{},"s":"[,:]\"\\\n☃"}"#,
        r#"{"a":1,"a":{"b":[{},["x"],false]},"zero":-0}"#,
        r#"{"emoji":"\ud83d\ude00","controls":"\t\r\n\b\f"}"#,
        r#"  {"n":123456789012345678901234567890,"empty":[[],{}]}   "#,
    ];
    for text in vectors {
        let expected: Value = serde_json::from_str(text).unwrap();
        let parsed = super::value::parse_json(text.as_bytes()).unwrap();
        assert_eq!(parsed.as_value(), &expected, "{text}");
    }
    let mut decoder = Decoder::new();
    let invalid_utf8 = b"{\"x\":\"\xff\"}";
    assert!(
        decoder
            .feed(&frame(2, invalid_utf8), ms(0))
            .unwrap()
            .is_none()
    );
    assert_eq!(decoder.buffered_bytes(), invalid_utf8);
}

#[test]
fn rpc_admission_matching_queue_and_deadline() {
    let mut client = RpcClient::new();
    assert_eq!(
        client.begin("fs.write", &Value::Null),
        Err(MicroError::MethodNotAllowlisted)
    );
    assert_eq!(
        client.begin("fs.read", &Value::Null),
        Err(MicroError::RestrictedFilesystemRead)
    );
    assert_eq!(
        client.begin("fs.read", &json!({"file":"keymap.json","extra":0})),
        Err(MicroError::RestrictedFilesystemRead)
    );
    let request = client.begin("sys.version", &Value::Null).unwrap();
    assert_eq!(request.id, 101);
    assert_eq!(
        client.check_read_deadline(ms(0)),
        Err(MicroError::WritesIncomplete)
    );
    assert_eq!(
        client.confirm_write(12, ms(10_000)),
        Err(MicroError::ShortWrite { count: 12 })
    );
    confirm_all(&mut client, &request, ms(10_000));
    assert!(client.check_read_deadline(ms(13_999)).is_ok());
    assert!(matches!(
        client.check_read_deadline(ms(14_000)),
        Err(MicroError::ResponseTimeout { .. })
    ));
    assert_eq!(client.feed_report(&[], ms(14_000)).unwrap(), None);
    assert_eq!(
        client
            .feed_message(object(json!({"id":"101","result":"wrong"})))
            .unwrap(),
        None
    );
    assert_eq!(
        client
            .feed_message(object(json!({"id":101, "method":"sys.version"})))
            .unwrap(),
        None
    );
    let notification = json!({"m":"v.oai.hid","p":{"k":"AG02","act":1}});
    client.feed_message(object(notification.clone())).unwrap();
    let response = client
        .feed_message(object(
            json!({"id":101.0,"method":"different","result":null}),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(response.as_value(), &Value::Null);
    assert_eq!(client.pop_input().unwrap().as_value(), &notification);

    let request = client
        .begin("fs.read", &json!({"file":"keymap.json"}))
        .unwrap();
    assert_eq!(request.id, 102);
    assert_eq!(
        String::from_utf8(payload(&request.reports)).unwrap(),
        r#"{"method":"fs.read","params":{"file":"keymap.json"},"id":102}"#
    );
    confirm_all(&mut client, &request, ms(20_000));
    assert!(matches!(
        client.feed_message(object(json!({"id":102,"result":3,"error":null}))),
        Err(MicroError::DeviceRejected { error, .. }) if error.as_value() == &Value::Null
    ));

    let mut client = RpcClient::new();
    let request = client.begin("device.status", &Value::Null).unwrap();
    confirm_all(&mut client, &request, ms(0));
    for i in 0..128 {
        client
            .feed_message(object(json!({"m":"v.oai.rad","n":i})))
            .unwrap();
    }
    client
        .feed_message(object(json!({"m":"unknown","n":999})))
        .unwrap();
    assert_eq!(client.queued_input_count(), 128);
    assert_eq!(
        client.feed_message(object(json!({"m":"kb.radial"}))),
        Err(MicroError::InputOverflow)
    );
    assert_eq!(client.queued_input_count(), 128);
    for i in 0..128 {
        assert_eq!(client.pop_input().unwrap().get("n"), Some(&json!(i)));
    }
    client
        .feed_message(object(json!({"m":"v.oai.hid"})))
        .unwrap();
    client.discard_connection();
    assert_eq!(client.queued_input_count(), 0);
    assert_eq!(client.begin("sys.version", &Value::Null).unwrap().id, 101);
}

#[test]
fn rpc_numeric_equality_and_fragmented_deadline_start() {
    assert!(super::json::numeric_equals_u64(&json!(true), 1));
    assert!(super::json::numeric_equals_u64(&json!(false), 0));
    assert!(super::json::numeric_equals_u64(&json!(101.0), 101));
    assert!(!super::json::numeric_equals_u64(&json!("101"), 101));
    let rounded = serde_json::from_str::<Value>("9007199254740993.0").unwrap();
    assert!(!super::json::numeric_equals_u64(
        &rounded,
        9_007_199_254_740_993
    ));
    assert!(super::json::numeric_equals_u64(
        &rounded,
        9_007_199_254_740_992
    ));

    let mut client = RpcClient::new();
    let request = client
        .begin("v.oai.thstatus", &json!({"long":"x".repeat(200)}))
        .unwrap();
    assert!(request.reports.len() > 1);
    for _ in 0..request.reports.len() - 1 {
        assert_eq!(client.confirm_write(64, ms(10_000)).unwrap(), None);
        assert_eq!(
            client.check_read_deadline(ms(99_000)),
            Err(MicroError::WritesIncomplete)
        );
    }
    assert_eq!(
        client.confirm_write(64, ms(15_000)).unwrap(),
        Some(ms(19_000))
    );
    assert!(client.check_read_deadline(ms(18_999)).is_ok());
    assert!(client.check_read_deadline(ms(19_000)).is_err());
    let response = wire::encode_json(&json!({"id":101,"result":"done"})).unwrap();
    assert_eq!(
        client
            .feed_report(&response[0], ms(19_001))
            .unwrap()
            .unwrap()
            .as_value(),
        &json!("done"),
        "a read started before the deadline can complete after it"
    );
}

#[test]
fn firmware_gate_spelling_and_result_dispositions() {
    for version in [
        "0.6.3",
        "0.6",
        "v1",
        "1",
        "vv0.10.0",
        " 0.6",
        "2.0.x-rc",
        "+1",
        "0.+0_6",
        "999999999999999999999999999999.0",
        "0.999999999999999999999999999",
        "\u{1c}0.6\u{1f}",
    ] {
        assert_eq!(
            rpc::validate_firmware_version(&json!({"version":version})).unwrap(),
            version
        );
    }
    for version in ["0.5.9", "0", "v0", "-1.9999999999999999999999"] {
        assert!(matches!(
            rpc::validate_firmware_version(&json!({"version":version})),
            Err(MicroError::UnsupportedFirmware { .. })
        ));
    }
    for version in [
        "x.6", "v.0.6", "0.6-beta", "2.0-rc", "0._6", "0.6_", "0.6__0", "0.+ 6", "０.6",
    ] {
        assert!(
            matches!(
                rpc::validate_firmware_version(&json!({"version":version})),
                Err(MicroError::InvalidFirmwareVersion { .. })
            ),
            "{version}"
        );
    }
    for value in [
        Value::Null,
        json!(false),
        json!(0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        let error = rpc::validate_firmware_version(&json!({"version":value})).unwrap_err();
        assert!(matches!(error, MicroError::UnsupportedFirmware { .. }));
        assert_eq!(error.disposition(), Disposition::ReconnectAfterThreeSeconds);
    }
    for result in [
        json!({"version":true}),
        json!({"version":1}),
        json!(null),
        json!([]),
    ] {
        let error = rpc::validate_firmware_version(&result).unwrap_err();
        assert_eq!(error, MicroError::MalformedFirmwareResult);
        assert_eq!(error.disposition(), Disposition::StopWithCleanup);
    }
}

#[test]
fn diagnostic_python_rendering_and_error_lifecycle() {
    let value = json!({
        "z": [null, true, "can't", "a'b\"c", "\n\u{001b}\u{00a0}", 1.0, -0.0, 0.00001, 1e16],
        "a": "snow ☃"
    });
    assert_eq!(
        render_diagnostic(&value),
        r#"{'a': 'snow ☃', 'z': [None, True, "can't", 'a\'b"c', '\n\x1b\xa0', 1.0, -0.0, 1e-05, 1e+16]}"#
    );
    assert_eq!(render_diagnostic(&json!("plain\ntext")), "plain\ntext");
    let error = MicroError::InvalidFirmwareVersion {
        component: "a'b".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "invalid literal for int() with base 10: \"a'b\""
    );
    assert_eq!(
        MicroError::MalformedEvent.disposition(),
        Disposition::StopWithCleanup
    );
    assert_eq!(
        MicroError::Serialization.disposition(),
        Disposition::StopWithCleanup
    );
    assert_eq!(
        MicroError::InvalidTelemetry.disposition(),
        Disposition::ReconnectAfterThreeSeconds
    );
    assert_eq!(MicroError::RpcBusy.disposition(), Disposition::CallerMisuse);
}

#[test]
fn windows_paths_capabilities_and_owner_policy() {
    let prefix = enumeration::WINDOWS_INTERFACE_PREFIX;
    let path = format!("{prefix}AbC");
    assert_eq!(
        enumeration::select_windows_path(format!("junk{path}\n").as_bytes()).unwrap(),
        path
    );
    let mixed = path.replace("HID", "hİd").replace("PID", "pıd");
    assert_eq!(
        enumeration::select_windows_path(mixed.as_bytes()).unwrap(),
        mixed
    );
    assert!(matches!(
        enumeration::select_windows_path(format!("{prefix} ").as_bytes()),
        Err(MicroError::InterfaceCount { count: 0 })
    ));
    assert!(matches!(
        enumeration::select_windows_path(path.replace("MI_02", "MI_01").as_bytes()),
        Err(MicroError::InterfaceCount { count: 0 })
    ));
    assert!(matches!(
        enumeration::select_windows_path(format!("{path}\u{1c}{path}").as_bytes()),
        Err(MicroError::InterfaceCount { count: 2 })
    ));
    let mut invalid_utf8 = prefix.as_bytes().to_vec();
    invalid_utf8.extend_from_slice(&[0xff, b'x']);
    assert!(
        enumeration::select_windows_path(&invalid_utf8)
            .unwrap()
            .ends_with("�x")
    );

    let caps = Capabilities {
        query_status: 0x0011_0000,
        usage_page: 0xff00,
        usage: 1,
        input_report_length: 64,
        output_report_length: 64,
    };
    assert!(enumeration::validate_capabilities(caps).is_ok());
    assert_eq!(
        enumeration::validate_capabilities(Capabilities {
            query_status: 0,
            ..caps
        }),
        Err(MicroError::CapabilityFailure)
    );
    for changed in [
        Capabilities {
            usage_page: 1,
            ..caps
        },
        Capabilities { usage: 2, ..caps },
        Capabilities {
            input_report_length: 63,
            ..caps
        },
        Capabilities {
            output_report_length: 65,
            ..caps
        },
    ] {
        assert_eq!(
            enumeration::validate_capabilities(changed),
            Err(MicroError::CapabilityLayout)
        );
    }
    assert_eq!(
        enumeration::validate_write(12),
        Err(MicroError::ShortWrite { count: 12 })
    );
    assert_eq!(
        enumeration::validate_report(&[0; 63]),
        Err(MicroError::ReportSize)
    );
    let processes = [
        r"C:\Apps\ChatGPT.EXE",
        r"c:input.exe",
        r"//server/share/INPUT.EXE",
        r"\\server\chatgpt.exe",
        r"c:\input.exe.old",
        r"c:\folder\.\CHATGPT.exe",
    ];
    assert_eq!(
        enumeration::conflicting_processes(processes),
        ["chatgpt.exe", "input.exe"]
    );
    for path in [
        r"\\input.exe",
        "///input.exe",
        "//server//input.exe",
        "//server/./input.exe",
        r"\\?\C:input.exe",
    ] {
        assert_eq!(
            enumeration::conflicting_processes([path]),
            ["input.exe"],
            "{path}"
        );
    }
    for path in [r"\\server\input.exe", r"\\?\UNC\server\input.exe"] {
        assert!(
            enumeration::conflicting_processes([path]).is_empty(),
            "{path}"
        );
    }
}

fn input_event(key: &str, action: Value) -> Value {
    json!({"m":"v.oai.hid","p":{"k":key,"act":action}})
}

fn legacy_sample(
    decoder: &mut LegacyDecoder,
    message: &Value,
    time: u64,
    bindings: &Bindings,
    controls_enabled: bool,
) -> Result<input::LegacyOutput, MicroError> {
    decoder.decode(
        message,
        &LegacyContext {
            monotonic: ms(time),
            wall_seconds: 100.0,
            bindings,
            controls_enabled,
            active_slots: None,
            selected_slot: None,
        },
    )
}

#[test]
fn legacy_slot_edges_dial_wrapping_and_debounce() {
    let bindings = Bindings::default();
    let mut decoder = LegacyDecoder::new(Calibration::default());
    let sample = |decoder: &mut LegacyDecoder, action: Value, time| {
        legacy_sample(
            decoder,
            &input_event("AG02", action),
            time,
            &bindings,
            false,
        )
        .unwrap()
        .slot
    };
    assert_eq!(sample(&mut decoder, json!(true), 1000), Some(Slot::ALL[2]));
    assert_eq!(sample(&mut decoder, json!(0), 1100), None);
    assert_eq!(sample(&mut decoder, json!(1.0), 1200), None);
    assert_eq!(
        sample(&mut decoder, json!(1), 1300),
        None,
        "bounced press stays held"
    );
    assert_eq!(sample(&mut decoder, json!(false), 1300), None);
    assert_eq!(sample(&mut decoder, json!(1), 1300), Some(Slot::ALL[2]));
    for key in ["ENC_CLK", "AG06", "ag00", "AG00 "] {
        assert_eq!(
            legacy_sample(
                &mut decoder,
                &input_event(key, json!(1)),
                2000,
                &bindings,
                false
            )
            .unwrap()
            .slot,
            None
        );
    }

    let active = [Slot::ALL[1], Slot::ALL[3]];
    let dial =
        |decoder: &mut LegacyDecoder, key: &str, time, selected: Option<Slot>, active: &[Slot]| {
            decoder
                .decode(
                    &input_event(key, json!(2)),
                    &LegacyContext {
                        monotonic: ms(time),
                        wall_seconds: 100.0,
                        bindings: &bindings,
                        controls_enabled: false,
                        active_slots: Some(active),
                        selected_slot: selected,
                    },
                )
                .unwrap()
                .slot
        };
    let mut decoder = LegacyDecoder::new(Calibration::default());
    assert_eq!(
        dial(&mut decoder, "ENC_CW", 1000, None, &active),
        Some(Slot::ALL[1])
    );
    assert_eq!(
        dial(&mut decoder, "ENC_CW", 1290, Some(Slot::ALL[3]), &active),
        None
    );
    assert_eq!(
        dial(&mut decoder, "ENC_CW", 1300, Some(Slot::ALL[3]), &active),
        Some(Slot::ALL[1])
    );
    assert_eq!(
        dial(&mut decoder, "ENC_CC", 1300, None, &active),
        Some(Slot::ALL[3])
    );
    assert_eq!(dial(&mut decoder, "ENC_CW", 2000, None, &[]), None);
    assert_eq!(
        dial(&mut decoder, "ENC_CW", 2000, Some(Slot::ALL[5]), &active),
        Some(Slot::ALL[1])
    );
}

#[test]
fn command_binding_precedence_hold_and_per_key_time() {
    let bindings = Bindings::default();
    let mut decoder = LegacyDecoder::new(Calibration::default());
    let sample = |decoder: &mut LegacyDecoder, action: Value, time| {
        legacy_sample(
            decoder,
            &input_event("ACT07", action),
            time,
            &bindings,
            true,
        )
        .unwrap()
        .command
    };
    let accepted = Some(input::CommandOutput {
        key: CommandKey::Act07,
        disposition: CommandDisposition::Action(Action::Accept),
    });
    assert_eq!(sample(&mut decoder, json!(1), 1000), accepted);
    assert_eq!(sample(&mut decoder, json!(0), 1200), None);
    assert_eq!(sample(&mut decoder, json!(1), 1400), None);
    assert_eq!(sample(&mut decoder, json!(1), 1600), None);
    assert_eq!(sample(&mut decoder, json!(0), 1700), None);
    assert_eq!(sample(&mut decoder, json!(1), 1700), accepted);

    let mut decoder = LegacyDecoder::new(Calibration::default());
    let output = legacy_sample(
        &mut decoder,
        &input_event("ACT07", json!(1)),
        1,
        &bindings,
        false,
    )
    .unwrap()
    .command
    .unwrap();
    assert_eq!(output.disposition, CommandDisposition::ControlsDisabled);
    let empty = Bindings::empty();
    let mut decoder = LegacyDecoder::new(Calibration::default());
    let output = legacy_sample(
        &mut decoder,
        &input_event("ACT07", json!(1)),
        1,
        &empty,
        false,
    )
    .unwrap()
    .command
    .unwrap();
    assert_eq!(output.disposition, CommandDisposition::Unbound);
    let custom =
        Bindings::from_overrides(&json!({"ACT07":"model_picker","DPAD_UP":"left"})).unwrap();
    assert_eq!(custom.get(CommandKey::Act07), Some(Action::ModelPicker));
    assert_eq!(custom.get(CommandKey::DpadUp), Some(Action::Left));
    assert_eq!(custom.get(CommandKey::Act08), Some(Action::Stop));
    for invalid in [
        json!({"AG00":"send"}),
        json!({"DPAD_UP":"accept"}),
        json!({"ACT07":"arbitrary"}),
        json!({"ACT07":null}),
    ] {
        assert!(Bindings::from_overrides(&invalid).is_err());
    }
    assert!(
        Bindings::from_entries([
            ("ACT07".to_owned(), Action::Accept),
            ("ACT07".to_owned(), Action::Send),
        ])
        .is_err()
    );
}

#[test]
fn micro_defaults_and_explicit_recommended_subset() {
    let defaults = input::MicroSettings::default();
    assert_eq!(defaults.brightness, 0.25);
    assert!(!defaults.controls_enabled && !defaults.voice_enabled && !defaults.auto_bind);
    assert_eq!(
        defaults.bindings.get(CommandKey::Act07),
        Some(Action::Accept)
    );
    assert!(!defaults.calibration.is_complete());
    let recommended = input::MicroSettings::recommended();
    assert_eq!(recommended.brightness, 0.75);
    assert!(recommended.controls_enabled && recommended.auto_bind);
    assert!(!recommended.voice_enabled);
    assert!(recommended.validate().is_ok());
    let mut invalid = defaults;
    invalid.brightness = f64::NAN;
    assert!(invalid.validate().is_err());
}

#[test]
fn microphone_two_switch_coalescing_and_rejected_bounce() {
    let bindings = Bindings::default();
    let mut decoder = LegacyDecoder::new(Calibration::default());
    let sample = |decoder: &mut LegacyDecoder, key, action, time| {
        legacy_sample(
            decoder,
            &input_event(key, json!(action)),
            time,
            &bindings,
            false,
        )
        .unwrap()
        .microphone
    };
    assert!(sample(&mut decoder, "ACT10", 1, 1000));
    assert!(!sample(&mut decoder, "ACT11", 1, 1020));
    assert!(!sample(&mut decoder, "ACT10", 0, 1100));
    assert!(!sample(&mut decoder, "ACT11", 0, 1101));
    assert!(!sample(&mut decoder, "ACT11", 1, 1399));
    assert!(!sample(&mut decoder, "ACT10", 1, 1400));
    assert!(!sample(&mut decoder, "ACT11", 0, 1401));
    assert!(!sample(&mut decoder, "ACT10", 0, 1402));
    assert!(sample(&mut decoder, "ACT11", 1, 1402));
    assert!(!sample(&mut decoder, "ACT11", 0, 1500));
    assert!(sample(&mut decoder, "ACT11", 1, 4000));
    assert!(!sample(&mut decoder, "ACT09", 1, 5000));
}

fn calibrated() -> Calibration {
    Calibration::from_json(&json!({"up":0.75,"right":0.0,"down":0.25,"left":0.5})).unwrap()
}

#[test]
fn radial_hysteresis_ties_telemetry_and_combined_order() {
    let bindings = Bindings::default();
    let mut decoder = LegacyDecoder::new(calibrated());
    let sample = |decoder: &mut LegacyDecoder, angle, distance, time| {
        legacy_sample(
            decoder,
            &json!({"m":"v.oai.rad","p":{"a":angle,"d":distance}}),
            time,
            &bindings,
            true,
        )
        .unwrap()
    };
    assert_eq!(
        sample(&mut decoder, 0.74, 1.0, 0).command.unwrap().key,
        CommandKey::DpadUp
    );
    assert!(sample(&mut decoder, 0.75, 1.0, 1).command.is_none());
    assert!(sample(&mut decoder, 0.99, 0.25, 2).command.is_none());
    assert!(!decoder.stick_armed());
    assert!(sample(&mut decoder, 0.99, 0.249, 3).command.is_none());
    assert_eq!(
        sample(&mut decoder, 0.99, 0.65, 4).command.unwrap().key,
        CommandKey::DpadRight
    );
    sample(&mut decoder, 0.0, 0.249, 5);
    assert_eq!(
        sample(&mut decoder, 0.875, 1.0, 6).command.unwrap().key,
        CommandKey::DpadUp
    );
    sample(&mut decoder, 0.0, 0.0, 7);
    let output = sample(&mut decoder, 1.0, 1.0, 8);
    assert_eq!(output.command.unwrap().key, CommandKey::DpadRight);
    assert_eq!(output.telemetry.unwrap().angle, 0.0);
    assert_eq!(decoder.telemetry().unwrap().timestamp, 100.0);

    let mut decoder = LegacyDecoder::new(Calibration::default());
    let output = legacy_sample(
        &mut decoder,
        &json!({"m":"kb.radial","p":{"a":0.3,"d":1.0,"k":"AG01","act":1}}),
        0,
        &bindings,
        false,
    )
    .unwrap();
    assert_eq!(output.slot, Some(Slot::ALL[1]));
    assert!(output.command.is_none());
    assert!(output.telemetry.is_some());
    assert!(
        decoder.stick_armed(),
        "incomplete calibration does not disarm"
    );

    for params in [
        json!({"a":true,"d":1}),
        json!({"a":0.5,"d":1.5}),
        json!({"d":1}),
        json!({"a":"0.5","d":1}),
    ] {
        let error = legacy_sample(
            &mut decoder,
            &json!({"m":"v.oai.rad","p":params}),
            1,
            &bindings,
            false,
        )
        .unwrap_err();
        assert_eq!(error, MicroError::InvalidTelemetry);
        assert_eq!(error.disposition(), Disposition::ReconnectAfterThreeSeconds);
    }
    assert_eq!(
        legacy_sample(&mut decoder, &json!({"p":null}), 2, &bindings, false),
        Err(MicroError::MalformedEvent)
    );
    assert_eq!(
        legacy_sample(
            &mut decoder,
            &json!({"p":{"k":[],"act":1}}),
            2,
            &bindings,
            false
        ),
        Err(MicroError::MalformedEvent)
    );
    assert!(legacy_sample(&mut decoder, &json!({}), 2, &bindings, false).is_ok());
}

#[test]
fn calibration_validation_capture_boundaries_and_answer() {
    assert!(Calibration::from_json(&json!({"up":0.1,"right":0.1})).is_ok());
    assert_eq!(
        Calibration::from_json(&json!({"up":0.1,"right":0.1,"down":0.5,"left":0.75})),
        Err(CalibrationError::OverlappingDirections)
    );
    assert_eq!(
        Calibration::from_json(&json!({"up":0.94,"right":0.09,"down":0.5,"left":0.75})),
        Err(CalibrationError::OverlappingDirections)
    );
    for value in [json!({"up":true}), json!({"up":1}), json!({"up":-0.1})] {
        assert_eq!(
            Calibration::from_json(&value),
            Err(CalibrationError::InvalidAngles)
        );
    }
    assert_eq!(
        Calibration::from_json(&json!({"UP":0.0})),
        Err(CalibrationError::InvalidKeys)
    );
    let empty = Calibration::default();
    let status = |timestamp, distance, angle| json!({"joystick":{"timestamp":timestamp,"distance":distance,"angle":angle}});
    let captured = empty
        .capture(
            &status(json!(98), json!(0.65), json!(0.75)),
            Direction::Up,
            100.0,
        )
        .unwrap();
    assert_eq!(captured.angle(Direction::Up), Some(0.75));
    assert_eq!(
        empty.capture(
            &status(json!(97.999), json!(1), json!(0.75)),
            Direction::Up,
            100.0
        ),
        Err(CalibrationError::NotHeld)
    );
    assert_eq!(
        empty.capture(
            &status(json!(100), json!(0.64), json!(0.75)),
            Direction::Up,
            100.0
        ),
        Err(CalibrationError::NotHeld)
    );
    assert!(
        empty
            .capture(
                &status(json!(101), json!(true), json!(0.75)),
                Direction::Up,
                100.0
            )
            .is_ok()
    );
    assert_eq!(
        empty.capture(
            &status(json!(100), json!(1), json!(1)),
            Direction::Up,
            100.0
        ),
        Err(CalibrationError::InvalidAngles)
    );
    assert_eq!(
        empty.capture(
            &status(json!(90), json!("bad"), json!(0)),
            Direction::Up,
            100.0
        ),
        Err(CalibrationError::NotHeld),
        "age is checked before distance"
    );
    assert!(super::calibration::calibration_authorized("CALIBRATE"));
    assert!(!super::calibration::calibration_authorized(" CALIBRATE"));
    assert!(super::calibration::calibration_quit(" Q "));
    assert!(!super::calibration::calibration_quit("quit"));
}

#[test]
fn strict_profile_is_narrow_and_preserves_injected_context() {
    let at = ("synthetic-clock".to_owned(), 789u64);
    let event = input::adapt_strict(&input_event("AG02", json!(1)), 42, at.clone()).unwrap();
    assert_eq!(event.control, "AG02");
    assert_eq!(event.kind, StrictEventKind::Press);
    assert_eq!(event.connection_generation, 42);
    assert_eq!(event.at, at);
    let turn = input::adapt_strict(&input_event("ENC_CC", json!(2)), 3, 5).unwrap();
    assert_eq!(turn.control, "encoder_turn");
    assert_eq!(turn.kind, StrictEventKind::Turn(-1));
    for message in [
        input_event("AG00", json!(true)),
        input_event("AG00", json!(1.0)),
        input_event("ACT10", json!(1)),
        input_event("DPAD_UP", json!(1)),
        input_event("ENC_CLK", json!(1)),
        json!({"m":"v.oai.rad","p":{"k":"AG00","act":1}}),
        json!({"m":"v.oai.hid","p":null}),
    ] {
        assert!(input::adapt_strict(&message, 0, ()).is_none());
    }
    let controls = input::declared_controls();
    assert_eq!(controls.len(), 17);
    assert!(controls.iter().any(|control| control.id == "encoder_press"));
}

#[test]
fn legacy_led_colors_order_brightness_and_shutdown() {
    let statuses = [
        LedStatus::Off,
        LedStatus::Idle,
        LedStatus::Working,
        LedStatus::Complete,
        LedStatus::Waiting,
        LedStatus::Error,
    ];
    let entries = led::legacy_frame(statuses, Some(Slot::ALL[0]), 0.4);
    assert_eq!(
        entries.map(|entry| entry.c),
        [0, 16_777_215, 2_126_079, 53_344, 16_755_200, 16_724_016]
    );
    assert_eq!(entries.map(|entry| entry.id), [0, 1, 2, 3, 4, 5]);
    assert_eq!(entries.map(|entry| entry.b), [0.4, 0.2, 0.2, 0.2, 0.2, 0.2]);
    assert!(entries.iter().all(|entry| entry.e == 1 && entry.s == 0.2));
    let shutdown = led::shutdown_frame();
    assert!(
        shutdown
            .iter()
            .all(|entry| entry.c == 0 && entry.b == 0.25 && entry.e == 1 && entry.s == 0.2)
    );
    let pattern = led::diagnostic_frame();
    assert_eq!(
        pattern.map(|entry| entry.b),
        [0.25, 0.125, 0.125, 0.125, 0.125, 0.125]
    );
    assert_eq!(
        led::active_slots(&[
            LedStatus::Off,
            LedStatus::Idle,
            LedStatus::Off,
            LedStatus::Error,
            LedStatus::Unknown,
            LedStatus::Waiting,
        ]),
        [Slot::ALL[1], Slot::ALL[3], Slot::ALL[4], Slot::ALL[5]]
    );
    assert_ne!(LedStatus::Unknown.color(), LedStatus::Off.color());
    assert!(led::legacy_frame(statuses, None, f64::NAN)[0].b.is_nan());

    let all_idle = led::legacy_frame([LedStatus::Idle; 6], Some(Slot::ALL[0]), 0.4);
    let mut client = RpcClient::new();
    let request = client.begin_led(&all_idle).unwrap();
    let text = String::from_utf8(payload(&request.reports)).unwrap();
    assert!(text.starts_with(
        r#"{"method":"v.oai.thstatus","params":[{"id":0,"c":16777215,"b":0.4,"e":1,"s":0.2},{"id":1,"c":16777215,"b":0.2,"e":1,"s":0.2}"#
    ));
    assert!(text.ends_with(r#"] ,"id":101}"#.replace(" ", "").as_str()));
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["params"]
            .as_array()
            .unwrap()
            .len(),
        6
    );

    let mut generic = RpcClient::new();
    let params = serde_json::to_value(all_idle).unwrap();
    let request = generic.begin("v.oai.thstatus", &params).unwrap();
    assert_eq!(payload(&request.reports), text.as_bytes());
}

fn semantic_fixture() -> (led::SemanticLed, led::EffectQualification) {
    let scope = led::CapabilityScope {
        device: led::DeviceKind::Micro,
        profile: input::PROFILE_ID.to_owned(),
        firmware: "0.6.3".to_owned(),
        transport: led::Transport::Usb,
        mode: "application".to_owned(),
        platform: "macos".to_owned(),
        architecture: "arm64".to_owned(),
    };
    let realized = led::SemanticLed {
        scope: scope.clone(),
        id: 2,
        token: led::SemanticToken::Working,
        rgb: [32, 112, 255],
        brightness: 0.4,
        realization: led::EffectRealization::NonNative,
    };
    let qualification = led::EffectQualification {
        capability: led::VisibleCapability {
            scope,
            feature: led::EffectFeature::Steady,
            evidence: led::Evidence::PhysicallyAccepted,
            observation_reference: Some("supplied-observation".to_owned()),
        },
        token: led::SemanticToken::Working,
        native_id: 7,
        native_speed: 12.0,
        smooth_cycle_ms: None,
    };
    (realized, qualification)
}

#[test]
fn semantic_led_requires_exact_supplied_physical_evidence() {
    let (realized, qualification) = semantic_fixture();
    let encoded = led::encode_semantic(&realized, Some(&qualification)).unwrap();
    assert_eq!(
        (encoded.id, encoded.c, encoded.b, encoded.e, encoded.s),
        (2, 0x2070ff, 0.4, 7, 12.0)
    );
    assert!(led::encode_semantic(&realized, None).is_none());
    let mut other = realized.clone();
    other.id = 6;
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other = realized.clone();
    other.brightness = f64::NAN;
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other.brightness = 1.01;
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other = realized.clone();
    other.scope.profile = "different".to_owned();
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other = realized.clone();
    other.scope.device = led::DeviceKind::Ct;
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other = realized.clone();
    other.scope.transport = led::Transport::Bluetooth;
    assert!(led::encode_semantic(&other, Some(&qualification)).is_none());
    other.scope.transport = led::Transport::Unknown;
    let mut unknown_scope = qualification.clone();
    unknown_scope.capability.scope.transport = led::Transport::Unknown;
    assert!(led::encode_semantic(&other, Some(&unknown_scope)).is_none());

    let mut bad = qualification.clone();
    bad.capability.evidence = led::Evidence::NativeObserved;
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());
    bad = qualification.clone();
    bad.capability.observation_reference = None;
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());
    bad.capability.observation_reference = Some(String::new());
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());
    bad = qualification.clone();
    bad.token = led::SemanticToken::Idle;
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());
    bad = qualification.clone();
    bad.capability.scope.mode.clear();
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());
    bad = qualification.clone();
    bad.native_speed = -0.01;
    assert!(led::encode_semantic(&realized, Some(&bad)).is_none());

    let mut pulse = realized.clone();
    pulse.realization = led::EffectRealization::Native { id: 4, speed: 0.2 };
    let mut pulse_qualification = qualification.clone();
    pulse_qualification.capability.feature = led::EffectFeature::SlowPulse;
    pulse_qualification.native_id = 4;
    pulse_qualification.native_speed = 0.2;
    pulse_qualification.smooth_cycle_ms = Some(999);
    assert!(led::encode_semantic(&pulse, Some(&pulse_qualification)).is_none());
    pulse_qualification.smooth_cycle_ms = Some(1000);
    assert_eq!(
        led::encode_semantic(&pulse, Some(&pulse_qualification))
            .unwrap()
            .e,
        4
    );
    pulse_qualification.native_id = 1;
    assert!(led::encode_semantic(&pulse, Some(&pulse_qualification)).is_none());
    pulse_qualification.native_id = 4;
    pulse_qualification.native_speed = 0.3;
    assert!(led::encode_semantic(&pulse, Some(&pulse_qualification)).is_none());
    pulse.realization = led::EffectRealization::Native { id: 4, speed: 0.0 };
    pulse_qualification.native_speed = 0.0;
    assert!(led::encode_semantic(&pulse, Some(&pulse_qualification)).is_none());
}

fn report(status: &Value, now: f64, dpad: Option<bool>) -> health::HealthReport {
    let bytes = serde_json::to_vec(status).unwrap();
    health::snapshot_health(SnapshotInput::Bytes(&bytes), now, dpad)
}

#[test]
fn snapshot_health_freshness_warnings_and_sanitization() {
    let status = json!({
        "heartbeat":99.0,
        "connected":true,
        "last_error":"bad\u{1b}[2J\nline\u{200d}\u{e000}"
    });
    let health = report(&status, 100.0, Some(false));
    assert!(health.controller_fresh);
    assert_eq!(health.connected, Some(true));
    assert!(health.configuration_ready(true));
    assert!(!health.configuration_ready(false));
    assert_eq!(health.checks[0].detail, "Current; heartbeat age 1.0s.");
    assert_eq!(health.checks[1].level, Level::Ok);
    assert_eq!(health.checks[2].level, Level::Warning);
    assert_eq!(health.checks[3].detail, "Latest snapshot: bad [2J line  ");

    for timestamp in [90.0, 101.0] {
        let historical = report(
            &json!({"heartbeat":timestamp,"connected":true,"last_error":"oops"}),
            100.0,
            None,
        );
        assert!(!historical.controller_fresh);
        assert_eq!(historical.connected, None);
        assert!(!historical.configuration_ready(true));
        assert_eq!(
            historical.checks[1].detail,
            "Unknown until the controller is current."
        );
        assert_eq!(
            historical.checks[2].detail,
            "Historical snapshot (not current): oops"
        );
    }
    for timestamp in [100.0, 90.01] {
        assert!(
            report(
                &json!({"heartbeat":timestamp,"connected":true}),
                100.0,
                None
            )
            .controller_fresh
        );
    }
    assert_eq!(
        report(&json!({"heartbeat":99}), 100.0, None).connected,
        Some(false)
    );
    assert_eq!(
        report(&json!({"heartbeat":99.75}), 100.0, None).checks[0].detail,
        "Current; heartbeat age 0.2s."
    );
    assert_eq!(
        report(&json!({"heartbeat":100.04}), 100.0, None).checks[0].detail,
        "Stopped, stale or clock changed; heartbeat age -0.0s."
    );
    let absent = report(&json!({"heartbeat":99,"stopped":true}), 100.0, None);
    assert!(
        absent.checks[0]
            .detail
            .ends_with("Stop reason: not recorded.")
    );
    let null = report(
        &json!({"heartbeat":99,"stopped":true,"stop_reason":null}),
        100.0,
        None,
    );
    assert!(null.checks[0].detail.ends_with("Stop reason: None."));
}

#[test]
fn snapshot_validation_order_bom_and_unreadable_inputs() {
    let missing = health::snapshot_health(SnapshotInput::Missing, 100.0, None);
    assert_eq!(
        missing.checks[0].detail,
        "status.json has no valid heartbeat"
    );
    let bytes = b"\xef\xbb\xbf{\"heartbeat\":99,\"connected\":true}";
    assert!(
        health::snapshot_health(SnapshotInput::Bytes(bytes), 100.0, None).configuration_ready(true)
    );
    let unreadable = health::snapshot_health(SnapshotInput::Unreadable("no\naccess"), 100.0, None);
    assert_eq!(
        unreadable.checks[0].detail,
        "Could not read status.json: no access"
    );
    let invalid = [
        (json!([]), "status.json must be an object"),
        (
            json!({"heartbeat":true,"connected":null}),
            "status.json has no valid heartbeat",
        ),
        (
            json!({"heartbeat":99,"connected":null,"stopped":null}),
            "status.json connected must be true or false",
        ),
        (
            json!({"heartbeat":99,"stopped":null}),
            "status.json stopped must be true or false",
        ),
        (
            json!({"heartbeat":99,"bindings":[]}),
            "status.json bindings must be an object",
        ),
        (
            json!({"heartbeat":99,"bindings":{"1":{"health":{"live":1}}}}),
            "status.json contains invalid binding health",
        ),
        (
            json!({"heartbeat":99,"bindings":{"1":{}}}),
            "status.json contains invalid binding health",
        ),
        (
            json!({"heartbeat":99,"slots":["unknown","bogus"]}),
            "status.json contains invalid slot states",
        ),
        (
            json!({"heartbeat":99,"last_error":1}),
            "status.json last_error must be text",
        ),
    ];
    for (status, expected) in invalid {
        let health = report(&status, 100.0, None);
        assert_eq!(health.checks[0].detail, expected);
        assert_eq!(health.connected, None);
    }
    assert!(report(&json!({"heartbeat":99,"slots":["unknown"]}), 100.0, None).controller_fresh);
}

#[test]
fn calibration_precondition_uses_separate_powershell_truthiness() {
    let base = |stopped: Value| json!({"connected":true,"heartbeat":99,"stopped":stopped});
    for stopped in [
        json!(null),
        json!(false),
        json!(0),
        json!(""),
        json!([]),
        json!([false]),
        json!([[]]),
    ] {
        assert!(health::calibration_precondition(
            true,
            None,
            &base(stopped),
            100.0
        ));
    }
    for stopped in [
        json!("false"),
        json!([[0]]),
        json!([false, false]),
        json!({}),
    ] {
        assert!(!health::calibration_precondition(
            true,
            None,
            &base(stopped),
            100.0
        ));
    }
    assert!(!health::calibration_precondition(
        false,
        None,
        &base(json!(false)),
        100.0
    ));
    assert!(!health::calibration_precondition(
        true,
        Some("bad"),
        &base(json!(false)),
        100.0
    ));
    assert!(health::calibration_precondition(
        true,
        Some(""),
        &base(json!(false)),
        100.0
    ));
    assert!(!health::calibration_precondition(
        true,
        None,
        &json!({"connected":true,"heartbeat":u64::MAX}),
        u64::MAX as f64
    ));
    assert!(!health::calibration_precondition(
        true,
        None,
        &json!({"connected":true,"heartbeat":101}),
        100.0
    ));
    assert!(health::calibration_precondition(
        true,
        None,
        &json!({"connected":true,"heartbeat":99,"stopped":[false],"bindings":1}),
        100.0
    ));
    assert_eq!(
        report(
            &json!({"connected":true,"heartbeat":99,"stopped":[false]}),
            100.0,
            None
        )
        .checks[0]
            .detail,
        "status.json stopped must be true or false"
    );
}
