use easy_rpc::bridge_reqwest::{NewClient, ReqwestTransport};
use easy_rpc::easyrpc::conformance::v1::{
    CountRequest, CountResponse, CountTrailerRequest, CountTrailerResponse, EchoRequest,
    EchoResponse, EchoTrailerRequest, EchoTrailerResponse, StreamFailRequest, StreamFailResponse,
    FailRequest,
};
use easy_rpc::protocol::{decode, encode, Transport};

fn base() -> String {
    std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".to_string())
}

const SVC: &str = "/easyrpc.conformance.v1.ConformanceService";

#[tokio::test]
async fn echo_unary_reqwest() {
    let c = ReqwestTransport::new(base());
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
    let c = NewClient(base());
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
    let c = ReqwestTransport::new(base());
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
    let c = ReqwestTransport::new(base());
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
    let c = ReqwestTransport::new(base());
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
    let c = ReqwestTransport::new(base());
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
async fn server_registry_ok() {
    use easy_rpc::protocol::MethodSpec;
    use easy_rpc::server::ServerRegistry;
    let reg = ServerRegistry { unary: Default::default(), stream: Default::default() };
    let specs = easy_rpc::easyrpc::conformance::v1::method_specs();
    assert_eq!(specs.len(), 11);
    assert!(specs.iter().any(|s: &MethodSpec| s.server_stream));
    let _ = reg;
}
