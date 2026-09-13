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
