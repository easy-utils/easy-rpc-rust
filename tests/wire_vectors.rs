// Wire golden-vector conformance (transport-independent): the protocol layer
// must reproduce easy-rpc-spec/conformance/wire-vectors.json byte-for-byte.
// The vector file is vendored at testdata/wire-vectors.json (synced from spec).
use easy_rpc::protocol::{
    code_from_string, code_to_string, decode_end_stream, demux_trailers, encode_end_stream_meta,
    encode_error_json, frame, http_status, mux_trailers, ErrorDetail, Headers,
};
use serde_json::Value;

fn vectors() -> Value {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/wire-vectors.json");
    let s = std::fs::read_to_string(p).expect("read wire-vectors.json");
    serde_json::from_str(&s).expect("parse wire-vectors.json")
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn md_of(v: &Value) -> Headers {
    let mut h = Headers::new();
    if let Some(obj) = v.as_object() {
        for (k, arr) in obj {
            if let Some(a) = arr.as_array() {
                h.insert(k.clone(), a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect());
            }
        }
    }
    h
}

#[test]
fn wire_vectors_frames() {
    for f in vectors()["frames"].as_array().unwrap() {
        let enc = &f["encode"];
        let payload = from_hex(enc["payloadHex"].as_str().unwrap());
        let end = enc["end"].as_bool().unwrap();
        let compressed = enc["compressed"].as_bool().unwrap();
        let mut bytes = frame(&payload, end);
        if compressed {
            bytes[0] |= 0x01;
        }
        assert_eq!(hex_of(&bytes), f["bytesHex"].as_str().unwrap(), "{}", f["name"]);
    }
}

#[test]
fn wire_vectors_end_stream() {
    for e in vectors()["endStream"].as_array().unwrap() {
        let raw = from_hex(e["decode"]["bytesHex"].as_str().unwrap());
        let (code, message, _details, metadata) = decode_end_stream(&raw);
        assert_eq!(code, e["code"].as_i64().unwrap() as i32, "{} code", e["name"]);
        assert_eq!(message, e["message"].as_str().unwrap(), "{} message", e["name"]);
        if !e["metadata"].is_null() {
            assert_eq!(metadata, md_of(&e["metadata"]), "{} metadata", e["name"]);
        }
        if !e["encode"].is_null() {
            let enc = &e["encode"];
            let got = encode_end_stream_meta(
                enc["code"].as_i64().unwrap() as i32,
                enc["message"].as_str().unwrap(),
                &[],
                &if enc["metadata"].is_null() { Headers::new() } else { md_of(&enc["metadata"]) },
            );
            // JSON payloads compare semantically.
            let got_v: Value = serde_json::from_slice(&got).expect("got json");
            let want_json: Value = serde_json::from_slice(&from_hex(e["bytesHex"].as_str().unwrap())).expect("want json");
            assert_eq!(got_v, want_json, "{} encode", e["name"]);
        }
    }
}

#[test]
fn wire_vectors_unary_error() {
    for u in vectors()["unaryError"].as_array().unwrap() {
        let enc = &u["encode"];
        let details: Vec<ErrorDetail> = enc["details"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|d| ErrorDetail {
                        type_: d["type"].as_str().unwrap().to_string(),
                        value: from_hex(d["valueHex"].as_str().unwrap()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let got = encode_error_json(enc["code"].as_i64().unwrap() as i32, enc["message"].as_str().unwrap(), &details);
        // JSON payloads are compared SEMANTICALLY (key order is not significant).
        let got_v: Value = serde_json::from_slice(&got).expect("got json");
        let want_v: Value = serde_json::from_str(u["bytesHex"].as_str().unwrap()).unwrap_or(Value::Null);
        let _ = want_v;
        // bytesHex is hex, not JSON text: decode then parse.
        let want_bytes = from_hex(u["bytesHex"].as_str().unwrap());
        let want_json: Value = serde_json::from_slice(&want_bytes).expect("want json");
        assert_eq!(got_v, want_json, "{}", u["name"]);
    }
}

#[test]
fn wire_vectors_trailers() {
    for t in vectors()["trailerHeaders"].as_array().unwrap() {
        if !t["demux"].is_null() {
            let (h, tl) = demux_trailers(&md_of(&t["demux"]));
            assert_eq!(h, md_of(&t["headers"]), "{} headers", t["name"]);
            assert_eq!(tl, md_of(&t["trailers"]), "{} trailers", t["name"]);
        }
        if !t["mux"].is_null() {
            let m = &t["mux"];
            let got = mux_trailers(&md_of(&m["headers"]), &md_of(&m["trailers"]));
            assert_eq!(got, md_of(&t["result"]), "{} mux", t["name"]);
        }
    }
}

#[test]
fn wire_vectors_code_map() {
    for c in vectors()["codeNames"].as_array().unwrap() {
        let code = c["code"].as_i64().unwrap() as i32;
        let name = c["name"].as_str().unwrap();
        assert_eq!(code_to_string(code), name, "code_to_string {code}");
        assert_eq!(code_from_string(name), code, "code_from_string {name}");
        if code != 0 {
            assert_eq!(http_status(code), c["http"].as_i64().unwrap() as u16, "http {code}");
        }
    }
}
