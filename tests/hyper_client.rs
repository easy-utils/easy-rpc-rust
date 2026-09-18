//! HyperClient (h1, dependency-light bridge) interop against the Go
//! conformance server — the locally-testable variant of the Rust bridge
//! family (reqwest is covered by interop.rs).
use easy_rpc::bridge_hyper::HyperClient;
use easy_rpc::easyrpc::conformance::v1::*;
use easy_rpc::protocol::{decode, encode, Transport};

#[tokio::test]
async fn echo_unary_h1() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".into());
    let host = base.trim_start_matches("http://").to_string();
    let c = HyperClient::new(base);
    let res = c.send(easy_rpc::protocol::Request {
        url: format!("http://{host}/easyrpc.conformance.v1.ConformanceService/Echo"),
        
        headers: Default::default(),
        body: Some(encode(&EchoRequest { input: "hi".into() })),
    }).await.expect("send");
    assert_eq!(res.status, 200);
    let out: EchoResponse = decode(&res.body).unwrap();
    assert_eq!(out.output, "echo:hi");
}

#[tokio::test]
async fn count_stream_h1() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".into());
    let host = base.trim_start_matches("http://").to_string();
    let c = HyperClient::new(base);
    let mut st = c.open_stream(easy_rpc::protocol::Request {
        url: format!("http://{host}/easyrpc.conformance.v1.ConformanceService/Count"),
        
        headers: Default::default(),
        body: Some(easy_rpc::protocol::frame(&encode(&CountRequest { count: 3 }), false).into()),
    }).await.expect("open");
    let mut idx = Vec::new();
    while let Some(b) = st.recv().await {
        idx.push(decode::<CountResponse>(&b).unwrap().index);
    }
    assert_eq!(idx, vec![0, 1, 2]);
}

#[tokio::test]
async fn fail_details_unary_error_carries_details_h1() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".into());
    let host = base.trim_start_matches("http://").to_string();
    let c = HyperClient::new(base);
    let res = c.send(easy_rpc::protocol::Request {
        url: format!("http://{host}/easyrpc.conformance.v1.ConformanceService/FailDetails"),
        
        headers: Default::default(),
        body: Some(encode(&FailDetailsRequest {
            code: 8,
            message: "limited".into(),
            detail_type: "type.googleapis.com/google.rpc.RetryInfo".into(),
            detail_text: "retry:5s".into(),
        })),
    }).await.expect("send");
    assert_eq!(res.status, 429);
    let e = res.error.expect("error must be reconstructed");
    assert_eq!(e.code, 8);
    assert_eq!(e.message, "limited");
    assert_eq!(e.details.len(), 1);
    assert_eq!(e.details[0].type_, "type.googleapis.com/google.rpc.RetryInfo");
    assert_eq!(e.details[0].value, b"retry:5s");
}
