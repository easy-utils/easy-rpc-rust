use easy_rpc::bridge_hyper::HyperClient;
use easy_rpc::protocol::{Transport, encode, decode};
use easy_rpc::easyrpc::conformance::v1::{EchoRequest, EchoResponse};

#[tokio::test]
async fn echo_unary() {
    let base = std::env::var("EASY_RPC_BASE").unwrap_or_else(|_| "http://127.0.0.1:18888".to_string());
    let c = HyperClient::new(base);
    let req = easy_rpc::protocol::Request {
        url: c.url("/v1/echo"),
        method: "POST".to_string(),
        headers: Default::default(),
        body: Some(encode(&EchoRequest { input: "hi".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    let out: EchoResponse = decode(&res.body).expect("decode");
    assert_eq!(out.output, "echo:hi");
}

#[tokio::test]
async fn server_registry_ok() {
    use easy_rpc::protocol::{ServerRegistry, MethodSpec};
    let reg = ServerRegistry { unary: Default::default(), stream: Default::default() };
    let specs = easy_rpc::easyrpc::conformance::v1::method_specs();
    assert_eq!(specs.len(), 4);
    assert!(specs.iter().any(|s: &MethodSpec| s.server_stream));
    let _ = reg;
}
