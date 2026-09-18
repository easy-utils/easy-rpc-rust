use easy_rpc::bridge_hyper::HyperClient;
use easy_rpc::bridge_reqwest::{NewClient, ReqwestTransport};
use easy_rpc::easyrpc::conformance::v1::{
    BigStreamRequest, BigStreamResponse, CountRequest, CountResponse, CountTrailerRequest,
    CountTrailerResponse, EchoBytesRequest, EchoBytesResponse, EchoRequest, EchoResponse,
    EchoTrailerRequest, EchoTrailerResponse, EmptyRequest, EmptyResponse, FailRequest,
    SleepRequest, SleepResponse, StreamFailRequest, StreamFailResponse,
};
use easy_rpc::protocol::{decode, encode, Transport};
use std::sync::Arc;

/// Transport selector (spec §7.1): EASY_RPC_TRANSPORT=reqwest|hyper (default reqwest).
fn tr() -> Arc<dyn Transport> {
    match std::env::var("EASY_RPC_TRANSPORT").as_deref() {
        Ok("hyper") => Arc::new(HyperClient::new(base())),
        _ => Arc::new(ReqwestTransport::new(base())),
    }
}

fn base() -> String {
    std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".to_string())
}

const SVC: &str = "/easyrpc.conformance.v1.ConformanceService";

#[tokio::test]
async fn echo_unary_reqwest() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/Echo"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&EchoRequest { input: "hi".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    let out: EchoResponse = decode(&res.body).expect("decode");
    assert_eq!(out.output, "echo:hi");
}

#[tokio::test]
async fn count_stream_reqwest() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/Count"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(easy_rpc::protocol::frame(&encode(&CountRequest { count: 3 }), false).into()),
    };
    let mut stream = c.open_stream(req).await.expect("open");
    let mut out = Vec::new();
    while let Some(b) = stream.recv().await {
        let m: CountResponse = decode(&b).expect("decode");
        out.push(m.index);
    }
    assert_eq!(out, vec![0, 1, 2]);
}

#[tokio::test]
async fn unary_error_surfaces() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/Fail"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&FailRequest { message: "nope".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    assert_eq!(res.error.as_ref().map(|e| e.code), Some(3));
}

#[tokio::test]
async fn stream_fail_surfaces_end_stream_error() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/StreamFail"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(easy_rpc::protocol::frame(
            &encode(&StreamFailRequest { emit_before: 2, code: 13, message: "boom".to_string() }),
            false,
        ).into()),
    };
    let mut stream = c.open_stream(req).await.expect("open");
    let mut seen = Vec::new();
    while let Some(b) = stream.recv().await {
        let m: StreamFailResponse = decode(&b).expect("decode");
        seen.push(m.index);
    }
    assert_eq!(seen, vec![0, 1]);
    assert_eq!(stream.last_error().map(|e| e.code), Some(13));
}

#[tokio::test]
async fn unary_trailer_surfaces() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/EchoTrailer"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&EchoTrailerRequest { input: "x".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    let out: EchoTrailerResponse = decode(&res.body).expect("decode");
    assert_eq!(out.output, "trailer:x");
    assert_eq!(res.trailers.get("x-trl").and_then(|v| v.first()).map(|s| s.as_str()), Some("unary-x"));
}

#[tokio::test]
async fn stream_trailer_surfaces() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/CountTrailer"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(easy_rpc::protocol::frame(&encode(&CountTrailerRequest { count: 2 }), false).into()),
    };
    let mut stream = c.open_stream(req).await.expect("open");
    let mut out = Vec::new();
    while let Some(b) = stream.recv().await {
        let m: CountTrailerResponse = decode(&b).expect("decode");
        out.push(m.index);
    }
    assert_eq!(out, vec![0, 1]);
    assert_eq!(stream.trailers().get("x-ctrailer").and_then(|v| v.first()).map(|s| s.as_str()), Some("done"));
}

#[tokio::test]
async fn echo_bytes_roundtrip() {
    let c = tr();
    let data = vec![0u8, 1, 2, 0xff, 0xfe, 0x80];
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/EchoBytes"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&EchoBytesRequest { data: data.clone() })),
    };
    let res = c.send(req).await.expect("send");
    let out: EchoBytesResponse = decode(&res.body).expect("decode");
    assert_eq!(out.data, data);
}

#[tokio::test]
async fn empty_roundtrip() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/Empty"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&EmptyRequest {})),
    };
    let res = c.send(req).await.expect("send");
    let _: EmptyResponse = decode(&res.body).expect("decode");
}

#[tokio::test]
async fn sleep_roundtrip() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/Sleep"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&SleepRequest { millis: 0 })),
    };
    let res = c.send(req).await.expect("send");
    let out: SleepResponse = decode(&res.body).expect("decode");
    assert!(out.ok);
}

#[tokio::test]
async fn big_stream_many_frames() {
    let c = tr();
    let req = easy_rpc::protocol::Request {
        url: format!("{SVC}/BigStream"),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(easy_rpc::protocol::frame(&encode(&BigStreamRequest { count: 3, size: 2048 }), false).into()),
    };
    let mut stream = c.open_stream(req).await.expect("open");
    let mut idx = Vec::new();
    while let Some(b) = stream.recv().await {
        let m: BigStreamResponse = decode(&b).expect("decode");
        idx.push(m.index);
    }
    assert_eq!(idx, vec![0, 1, 2]);
}

#[tokio::test]
async fn server_registry_ok() {
    use easy_rpc::protocol::MethodSpec;
    use easy_rpc::server::ServerRegistry;
    let reg = ServerRegistry { unary: Default::default(), stream: Default::default() };
    let specs = easy_rpc::easyrpc::conformance::v1::method_specs();
    assert_eq!(specs.len(), 15);
    assert!(specs.iter().any(|s: &MethodSpec| s.server_stream));
    let _ = reg;
}
