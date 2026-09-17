//! Optional easy-rpc server module (hyper adapter + ServerRegistry). Gated
//! behind the `server` feature so a client-only build links no HTTP server
//! runtime. Import the server surface from here, not from `protocol`.
//!
//! Streaming: the handler runs in a background task pushing frames into a
//! channel; `hyper_serve` returns as soon as the FIRST frame (and thus the
//! status/headers) is available, and the response body is the live channel.
//! Frames therefore reach the client as they are produced — never buffered.
use crate::dispatch::{RequestContext, ResponseWriter, handle};
use crate::protocol::{Headers, MethodSpec, RPCError, Request};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame as BodyFrame;
use std::sync::Arc;

// ---- server-side (hyper) ----
pub type UnaryHandler = Box<dyn Fn(Vec<u8>, String, &Headers) -> Result<Vec<u8>, RPCError> + Send + Sync>;
pub type StreamHandler = Box<dyn Fn(Vec<u8>, String, &Headers, Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>) -> Result<(), RPCError> + Send + Sync>;

pub struct ServerRegistry {
    pub unary: std::collections::HashMap<String, UnaryHandler>,
    pub stream: std::collections::HashMap<String, StreamHandler>,
}

pub fn content_kind(req: &Request) -> String {
    if let Some(ct) = req.headers.get("content-type").and_then(|v| v.first()) {
        if ct.starts_with("application/json") { return "json".to_string() }
    }
    "proto".to_string()
}

/// A hyper `ResponseWriter` that forwards frames into a channel; the channel is
/// the streaming response body.
struct ChannelWriter {
    tx: tokio::sync::mpsc::UnboundedSender<Result<BodyFrame<Bytes>, std::io::Error>>,
    status: std::sync::atomic::AtomicU16,
    headers: std::sync::Mutex<Headers>,
}

impl ResponseWriter for ChannelWriter {
    fn status(&self, code: u16) {
        self.status.store(code, std::sync::atomic::Ordering::SeqCst);
    }
    fn header(&self, headers: Headers) {
        *self.headers.lock().unwrap() = headers;
    }
    fn write_frame(&self, payload: Vec<u8>) -> Result<(), RPCError> {
        self.tx
            .send(Ok(BodyFrame::data(Bytes::from(payload))))
            .map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })
    }
}

/// Build a hyper service_fn handler from method specs + a registry. The response
/// is a live stream: it is returned once the first frame is available, and the
/// remaining frames flow through as the handler produces them.
pub async fn hyper_serve(
    methods: Arc<Vec<MethodSpec>>,
    reg: Arc<ServerRegistry>,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<BoxBody<Bytes, std::io::Error>>, std::convert::Infallible> {
    let uri = req.uri().clone();
    let _path = uri.path().to_string();
    let method = req.method().clone();
    let req_headers = req.headers().clone();
    let body = { use http_body_util::BodyExt; req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default() };
    // All request headers are forwarded (lowercased, like hyper's HeaderMap):
    // handlers read auth/metadata via the Headers argument.
    let mut req_headers2 = Headers::new();
    for (name, value) in req_headers.iter() {
        let k = name.as_str().to_ascii_lowercase();
        let v = value.to_str().unwrap_or("").to_string();
        req_headers2.entry(k).or_default().push(v);
    }
    let req2 = Request {
        url: uri.to_string(), method: method.to_string(),
        headers: req_headers2.clone(), body: Some(bytes::Bytes::copy_from_slice(&body)),
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<BodyFrame<Bytes>, std::io::Error>>();
    let writer = Arc::new(ChannelWriter {
        tx,
        status: std::sync::atomic::AtomicU16::new(200),
        headers: std::sync::Mutex::new(Headers::new()),
    });

    // Run the dispatch in the background so this function can return the moment
    // the first frame (and thus status/headers) is ready.
    let w2 = writer.clone();
    let ctx = RequestContext::new(req_headers2);
    let task = tokio::spawn(async move {
        handle(&ctx, req2, &methods, &reg, w2).await;
    });

    // Wait for the first frame or for the handler to finish (empty response).
    let first: Option<Result<BodyFrame<Bytes>, std::io::Error>> = if task.is_finished() {
        None
    } else {
        tokio::select! {
            f = rx.recv() => f,
            _ = task => None,
        }
    };

    let code = writer.status.load(std::sync::atomic::Ordering::SeqCst);
    let headers = std::mem::take(&mut *writer.headers.lock().unwrap());
    drop(writer);

    let mut builder = hyper::Response::builder().status(code);
    for (k, vs) in &headers {
        for v in vs {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }

    let rest = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    let stream = futures::stream::iter(first).chain(rest);
    let body: BoxBody<Bytes, std::io::Error> =
        http_body_util::BodyExt::boxed(StreamBody::new(stream));
    Ok(builder.body(body).unwrap_or_else(|_| empty_response(500)))
}

pub fn empty_response(status: u16) -> hyper::Response<BoxBody<Bytes, std::io::Error>> {
    let full: BoxBody<Bytes, std::io::Error> = Full::<Bytes>::new(Bytes::new())
        .map_err(|e: std::convert::Infallible| match e {})
        .boxed();
    hyper::Response::builder().status(status).body(full).unwrap()
}
