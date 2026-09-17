use easy_rpc::bridge_reqwest::{ReqwestTransport, NewClient};
use easy_rpc::protocol::{Transport, encode, decode};
use easy_rpc::easyrpc::conformance::v1::{EchoRequest, EchoResponse, CountRequest, CountResponse};

#[tokio::test]
async fn echo_unary_reqwest() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".to_string());
    let c = ReqwestTransport::new(base);
    let req = easy_rpc::protocol::Request {
        url: "/v1/echo".to_string(),
        method: "POST".to_string(),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&EchoRequest { input: "hi".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    let out: EchoResponse = decode(&res.body).expect("decode");
    assert_eq!(out.output, "echo:hi");
}

#[tokio::test]
async fn count_stream_reqwest() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".to_string());
    let c = NewClient(base);
    let req = easy_rpc::protocol::Request {
        url: "/v1/count".to_string(),
        method: "POST".to_string(),
        headers: easy_rpc::protocol::Headers::new(),
        body: Some(encode(&CountRequest { count: 3 })),
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
async fn server_registry_ok() {
    use easy_rpc::server::ServerRegistry;
    use easy_rpc::protocol::MethodSpec;
    let reg = ServerRegistry { unary: Default::default(), stream: Default::default() };
    let specs = easy_rpc::easyrpc::conformance::v1::method_specs();
    assert_eq!(specs.len(), 9);
    assert!(specs.iter().any(|s: &MethodSpec| s.server_stream));
    let _ = reg;
}
