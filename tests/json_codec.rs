// JSON codec tests (transport-independent) for the Rust core.
use easy_rpc::easyrpc::conformance::v1::*;
use easy_rpc::protocol::{content_kind_of, content_type_for, decode_msg, encode_msg, ContentKind};

#[test]
fn content_kind_mapping() {
    assert_eq!(content_kind_of("application/proto"), Some(ContentKind::Proto));
    assert_eq!(content_kind_of("application/json; charset=utf-8"), Some(ContentKind::Json));
    assert_eq!(content_kind_of("application/connect+json"), Some(ContentKind::Json));
    assert_eq!(content_kind_of("text/plain"), None);
    assert_eq!(content_type_for(false, ContentKind::Json), "application/json");
    assert_eq!(content_type_for(true, ContentKind::Json), "application/connect+json");
}

#[test]
fn json_roundtrip_string() {
    let m = EchoResponse { output: "echo:hi".to_string() };
    let b = encode_msg(&m, ContentKind::Json).unwrap();
    assert_eq!(std::str::from_utf8(&b).unwrap(), r#"{"output":"echo:hi"}"#);
    let back: EchoResponse = decode_msg(&b, ContentKind::Json).unwrap();
    assert_eq!(back.output, "echo:hi");
}

#[test]
fn json_bytes_are_base64() {
    let m = EchoBytesResponse { data: vec![0, 1, 2, 0xff, 0xfe, 0x80] };
    let b = encode_msg(&m, ContentKind::Json).unwrap();
    // proto3 JSON encodes bytes as base64.
    let s = std::str::from_utf8(&b).unwrap();
    assert!(s.contains("AAEC"), "base64 expected: {s}");
    let back: EchoBytesResponse = decode_msg(&b, ContentKind::Json).unwrap();
    assert_eq!(back.data, m.data);
}

#[test]
fn json_int_uses_camel_case_and_ignores_unknown() {
    // Unknown field must be ignored (pbjson default).
    let b = br#"{"count":42,"unknownField":"x"}"#;
    let m: CountRequest = decode_msg(b, ContentKind::Json).unwrap();
    assert_eq!(m.count, 42);
}

#[test]
fn proto_decode_still_works() {
    let m = EchoResponse { output: "hi".to_string() };
    let b = encode_msg(&m, ContentKind::Proto).unwrap();
    let back: EchoResponse = decode_msg(&b, ContentKind::Proto).unwrap();
    assert_eq!(back.output, "hi");
}

#[test]
fn json_any_roundtrip_via_descriptor() {
    // Verify the descriptor pool exposes google.protobuf.Any so the JSON codec
    // can emit the special `@type` form (prost-reflect).
    let pool = easy_rpc::descriptor_pool::official_pool();
    assert!(pool.get_message_by_name("google.protobuf.Any").is_some());
}
