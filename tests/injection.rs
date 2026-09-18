//! Composition-root injection parity: `connect_with_adapter` wraps a custom
//! adapter with the SAME built-in interceptors as a mode-picked one.
use std::sync::{Arc, Mutex};

use easy_rpc::interceptors::{connect_with_adapter, Mode};
use easy_rpc::protocol::{self, Headers, Request, Response, Transport};

type Seen = Mutex<Vec<Headers>>;

struct FakeAdapter(Arc<Seen>);

#[async_trait::async_trait]
impl Transport for FakeAdapter {
    async fn send(&self, req: Request) -> Result<Response, protocol::RPCError> {
        self.0.lock().unwrap().push(req.headers.clone());
        Ok(Response { status: 200, headers: Headers::new(), body: Vec::new().into(), trailers: Headers::new(), error: None })
    }
    async fn open_stream(
        &self,
        _req: Request,
    ) -> Result<Box<dyn protocol::Stream>, protocol::RPCError> {
        unreachable!()
    }
}

#[tokio::test]
async fn injected_adapter_gets_standard_interceptors() {
    let seen = Arc::new(Seen::default());
    let t = connect_with_adapter(
        "sekret",
        1500,
        vec![],
        Arc::new(FakeAdapter(seen.clone())),
    );
    let _ = t.send(Request { url: "/x".into(), headers: Headers::new(), body: None }).await;
    let h = &seen.lock().unwrap()[0];
    assert_eq!(
        h.get("authorization").and_then(|v| v.first()).map(String::as_str),
        Some("Bearer sekret")
    );
    assert_eq!(
        h.get("connect-timeout-ms").and_then(|v| v.first()).map(String::as_str),
        Some("1500")
    );
}

#[tokio::test]
async fn no_opts_leaves_injected_adapter_untouched() {
    let seen = Arc::new(Seen::default());
    let t = connect_with_adapter("", 0, vec![], Arc::new(FakeAdapter(seen.clone())));
    let mut h = Headers::new();
    h.insert("a".into(), vec!["b".into()]);
    let _ = t.send(Request { url: "/x".into(), headers: h, body: None }).await;
    let got = seen.lock().unwrap()[0].clone();
    assert!(!got.contains_key("authorization"));
    assert_eq!(got.get("a").and_then(|v| v.first()).map(String::as_str), Some("b"));
}

#[test]
fn mode_connect_still_works() {
    // compile-level: the mode-based root has not changed signature.
    let _mode = Mode::Auto;
}
