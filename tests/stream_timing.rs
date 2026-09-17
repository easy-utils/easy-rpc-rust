// Server-stream must be incremental: frames are pushed and flushed as produced,
// never buffered until the handler returns.
#![cfg(feature = "server")]

use std::sync::Arc;
use bytes::Bytes;
use std::time::{Duration, Instant};

use easy_rpc::dispatch::ResponseWriter;
use easy_rpc::protocol::{Headers, MethodSpec, RPCError};
use easy_rpc::server::ServerRegistry;

struct ChanWriter {
    tx: tokio::sync::mpsc::UnboundedSender<Bytes>,
    status: std::sync::atomic::AtomicU16,
    headers: std::sync::Mutex<Headers>,
}

impl ResponseWriter for ChanWriter {
    fn status(&self, code: u16) {
        self.status.store(code, std::sync::atomic::Ordering::SeqCst);
    }
    fn header(&self, headers: Headers) {
        *self.headers.lock().unwrap() = headers;
    }
    fn write_frame(&self, payload: Vec<u8>) -> Result<(), RPCError> {
        self.tx.send(Bytes::from(payload)).map_err(|e| RPCError { code: 13, message: e.to_string() })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_is_incremental() {
    let methods = vec![MethodSpec {
        service: "t".into(), name: "Slow".into(), path: "/t.Slow".into(),
        http_method: "POST".into(), client_stream: false, server_stream: true, body: "".into(),
    }];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let w = Arc::new(ChanWriter { tx, status: std::sync::atomic::AtomicU16::new(200), headers: std::sync::Mutex::new(Headers::new()) });
    let ctx = easy_rpc::dispatch::RequestContext::default();
    let req = easy_rpc::protocol::Request { url: "/t.Slow".into(), method: "POST".into(), headers: Headers::new(), body: Some(Bytes::new()) };

    // Real-time async handler (sleeps, not blocks), mirroring production.
    let mut reg2 = ServerRegistry { unary: Default::default(), stream: Default::default() };
    reg2.stream.insert(
        "Slow".to_string(),
        Box::new(|_req: Vec<u8>, _kind: String, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| {
            for i in 0..3u8 {
                let _ = emit(vec![i]);
                std::thread::sleep(Duration::from_millis(150));
            }
            Ok(())
        }),
    );
    let reg2 = Arc::new(reg2);
    let methods2 = methods.clone();

    let start = Instant::now();
    let handle = tokio::spawn({
        let w = w.clone();
        async move { easy_rpc::dispatch::handle(&ctx, req, &methods2, &reg2, w).await }
    });
    let first = rx.recv().await.expect("first frame");
    let first_at = start.elapsed();
    assert_eq!(first[5], 0); // payload byte 0
    handle.await.unwrap();
    assert!(first_at < Duration::from_millis(100), "first frame at {first_at:?} — buffered");
    println!("PASS: first frame at {first_at:?}");
}

#[test]
fn timeout_helpers() {
    assert_eq!(easy_rpc::protocol::parse_timeout(""), 0);
    assert_eq!(easy_rpc::protocol::parse_timeout("0"), 0);
    assert_eq!(easy_rpc::protocol::parse_timeout("250"), 250);
    let req = easy_rpc::protocol::Request {
        url: "/x".into(), method: "POST".into(),
        headers: easy_rpc::protocol::Headers::new(), body: None,
    };
    let req = easy_rpc::protocol::with_timeout(req, 300);
    assert_eq!(req.headers.get(easy_rpc::protocol::HEADER_TIMEOUT).unwrap()[0], "300");
    let ctx = easy_rpc::dispatch::RequestContext::new(req.headers);
    assert_eq!(ctx.deadline_ms, 300);
}

#[tokio::test]
async fn interceptor_transport_applies_metadata() {
    use easy_rpc::interceptors::with_interceptors;
    use easy_rpc::protocol::*;
    use std::sync::{Arc, Mutex};

    struct Cap(Arc<Mutex<Option<Headers>>>);
    #[async_trait::async_trait]
    impl Transport for Cap {
        async fn send(&self, req: Request) -> Result<Response, RPCError> {
            *self.0.lock().unwrap() = Some(req.headers);
            Ok(Response { status: 200, headers: Headers::new(), body: bytes::Bytes::new(), error: None })
        }
        async fn open_stream(&self, _req: Request) -> Result<Box<dyn Stream>, RPCError> {
            Err(RPCError { code: 12, message: "n/a".into() })
        }
    }

    let seen = Arc::new(Mutex::new(None));
    let mut md = Headers::new();
    md.insert("x-test".into(), vec!["abc".into()]);
    let inner: Arc<dyn Transport> = Arc::new(Cap(seen.clone()));
    let t = with_interceptors(inner, vec![Arc::new(MetadataInterceptor(md)) as Arc<dyn Interceptor>]);
    let _ = t.send(Request { url: "/x".into(), method: "POST".into(), headers: Headers::new(), body: None }).await;
    let h = seen.lock().unwrap().clone().unwrap();
    assert_eq!(h.get("x-test").unwrap()[0], "abc");
}

#[test]
fn error_json_roundtrip() {
    let b = easy_rpc::protocol::encode_error_json(7, "denied");
    assert_eq!(std::str::from_utf8(&b).unwrap(), r#"{"code":"permission_denied","message":"denied"}"#);
    let (c, m) = easy_rpc::protocol::decode_error_json(&b);
    assert_eq!((c, m.as_str()), (7, "denied"));
    assert_eq!(easy_rpc::protocol::decode_error_json(b"plain").0, 0);
}

#[test]
fn limits_and_version_consts() {
    assert_eq!(easy_rpc::protocol::CONNECT_PROTOCOL_VERSION, "1");
    assert_eq!(easy_rpc::protocol::DEFAULT_MAX_MESSAGE_BYTES, 4 * 1024 * 1024);
}

#[test]
fn gzip_roundtrip() {
    let orig: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let z = easy_rpc::protocol::gzip_compress(&orig);
    assert!(z.len() < orig.len(), "did not compress");
    assert_eq!(easy_rpc::protocol::gzip_decompress(&z), orig);
    let fr = easy_rpc::protocol::frame_compressed(&orig);
    assert_ne!(fr[0] & 0x01, 0);
}
