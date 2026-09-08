use easy_rpc::bridge_hyper::HyperClient;
use easy_rpc::protocol::{Transport, encode, decode};
use easy_rpc::easyrpc::conformance::v1::{EchoRequest, EchoResponse};
use hex::encode as hexenc;

#[tokio::test]
async fn echo_unary() {
    let c = HyperClient::new("http://127.0.0.1:18888".to_string());
    let req = easy_rpc::protocol::Request {
        url: c.url("/v1/echo"),
        method: "POST".to_string(),
        headers: Default::default(),
        body: Some(encode(&EchoRequest { input: "hi".to_string() })),
    };
    let res = c.send(req).await.expect("send");
    eprintln!("status={} body_hex={}", res.status, hexenc(&res.body));
    match decode::<EchoResponse>(&res.body) {
        Ok(out) => assert_eq!(out.output, "echo:hi"),
        Err(e) => panic!("decode failed: {e}"),
    }
}
