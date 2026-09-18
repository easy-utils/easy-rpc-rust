//! Error-path matrix (spec §4.2 M1–M13) + Error Details round-trip (§4.1).
//! Mirrored in every language implementation; inputs are constructed directly
//! against the protocol functions — no server needed.
use easy_rpc::protocol::*;

fn detail() -> ErrorDetail {
    ErrorDetail { type_: "type.googleapis.com/google.rpc.RetryInfo".to_string(), value: vec![1, 2, 3, 250] }
}

#[test]
fn matrix_end_stream_decode() {
    // M1: empty payload => clean end
    assert_eq!(decode_end_stream(b"").0, 0);
    // M2: garbage bytes => clean end, no panic
    assert_eq!(decode_end_stream(&[0xff, 0xfe, 0x00, 0x42]).0, 0);
    // M3: error without code/message => unknown code, empty message
    let (c, m, d, _md) = decode_end_stream(br#"{"error":{}}"#);
    assert_eq!((c, m.as_str(), d.len()), (2, "", 0));
    // M4: unknown code name => 2
    let (c, _, _, _md) = decode_end_stream(br#"{"error":{"code":"nope","message":"m"}}"#);
    assert_eq!(c, 2);
    // M5: unknown fields ignored
    let (c, _, _, _md) = decode_end_stream(br#"{"error":{"code":"not_found","message":"m"},"x":1}"#);
    assert_eq!(c, 5);
    // M6: details round-trip (base64 -> bytes)
    let payload = encode_end_stream(8, "rate limited", &[detail()]);
    let (c, m, d, _md) = decode_end_stream(&payload);
    assert_eq!(c, 8);
    assert_eq!(m, "rate limited");
    assert_eq!(d, vec![detail()]);
    // M7: malformed detail entries skipped, never fatal
    let (_, _, d, _md) = decode_end_stream(
        br#"{"error":{"code":"resource_exhausted","details":[{"type":"t","value":"!!!"},{"value":"x"},{"type":"ok"},{"type":"t2","value":"AQID"}]}}"#,
    );
    assert_eq!(d, vec![ErrorDetail { type_: "t2".into(), value: vec![1, 2, 3] }]);
    // details omitted when empty (v1.0 semantics)
    let payload = encode_end_stream(5, "gone", &[]);
    let text = std::str::from_utf8(&payload).unwrap();
    assert!(!text.contains("details"));
}

#[test]
fn matrix_unary_error_decode() {
    // M11: plain text is not a JSON error body
    let (c, _, _) = decode_error_json(b"busy");
    assert_eq!(c, 0);
    // details round-trip
    let b = encode_error_json(8, "limited", &[detail()]);
    let (c, m, d) = decode_error_json(&b);
    assert_eq!((c, m.as_str()), (8, "limited"));
    assert_eq!(d, vec![detail()]);
    // no-details body stays v1.0-shaped
    let b = encode_error_json(5, "x", &[]);
    assert_eq!(std::str::from_utf8(&b).unwrap(), r#"{"code":"not_found","message":"x"}"#);
    // M12/M13: deadline code
    let (c, _, _) = decode_error_json(&encode_error_json(4, "deadline exceeded", &[]));
    assert_eq!(c, 4);
}

#[test]
fn matrix_framing() {
    // M8: truncated frame errors (read_frame returns None only when a complete
    // frame is unavailable — a short header/payload must not yield partial).
    let full = frame(b"0123456789", false);
    assert!(read_frame(&full[..full.len() - 4]).is_none());
    // M9: oversized frame rejected.
    let mut huge = vec![0u8; 5];
    huge[1..5].copy_from_slice(&(4 * 1024 * 1024 + 1u32).to_be_bytes());
    assert!(read_frame(&huge).is_none());
}

#[test]
fn matrix_deadline_codes() {
    // M12/M13: deadline maps to code 4 both directions.
    assert_eq!(http_status(4), 504);
    assert_eq!(connect_from_status(504), 4);
}
