//! axum adapter: build an axum::Router for an easy-rpc service registry.
//!
//! This is the "thick" server backend: it lets you mount easy-rpc RPC routes
//! into ANY existing axum application (via `nest` / `merge`) and reuse axum's
//! middleware (auth, tracing, CORS, ...). It is gated behind the optional
//! `axum` feature; core stays dependency-free.
//!
//! Server-stream responses are real streams: the writer forwards each frame into
//! an unbounded channel that becomes the response body, so frames are flushed as
//! they are produced.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request as AxRequest, Response as AxResponse, StatusCode};
use axum::response::Response as AxResp;
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody, combinators::BoxBody};
use hyper::body::Frame as BodyFrame;

use crate::dispatch::{RequestContext, ResponseWriter, handle};
use crate::protocol::{Headers, MethodSpec, RPCError};
use crate::server::ServerRegistry;
use futures::StreamExt;

/// Build an axum Router for the given method specs + registry. Nest or merge it
/// into your own application; add middleware via `.layer(...)`.
pub fn router(methods: Vec<MethodSpec>, reg: Arc<ServerRegistry>) -> Router {
    let state = Arc::new((methods, reg));
    Router::new().fallback(any(axum_handle)).with_state(state)
}

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
            .map_err(|e| RPCError { code: 13, message: e.to_string() })
    }
}

async fn axum_handle(
    State(cfg): State<Arc<(Vec<MethodSpec>, Arc<ServerRegistry>)>>,
    req: AxRequest<Body>,
) -> AxResp<Body> {
    let (methods, reg) = cfg.as_ref().clone();
    let (parts, body) = req.into_parts();
    let raw = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let headers = map_headers(&parts.headers);
    let core_req = crate::protocol::Request {
        url: parts.uri.path().to_string(),
        method: parts.method.as_str().to_string(),
        headers: headers.clone(),
        body: Some(Bytes::from(raw)),
    };
    let is_stream = methods
        .iter()
        .find(|m| m.path == parts.uri.path())
        .map(|m| m.server_stream)
        .unwrap_or(false);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<BodyFrame<Bytes>, std::io::Error>>();
    let writer = Arc::new(ChannelWriter {
        tx,
        status: std::sync::atomic::AtomicU16::new(200),
        headers: std::sync::Mutex::new(Headers::new()),
    });
    let w2 = writer.clone();
    let methods2 = methods.clone();
    let reg2 = reg.clone();
    let ctx = RequestContext::new(headers.clone());
    let task = tokio::spawn(async move {
        handle(&ctx, core_req, &methods2, &reg2, w2).await;
    });

    // Return as soon as the first frame (status/headers) is available; the rest
    // of the stream flows through the live channel.
    let first: Option<Result<BodyFrame<Bytes>, std::io::Error>> = if task.is_finished() {
        None
    } else {
        tokio::select! {
            f = rx.recv() => f,
            _ = task => None,
        }
    };

    let status = writer.status.load(std::sync::atomic::Ordering::SeqCst);
    let out_headers = std::mem::take(&mut *writer.headers.lock().unwrap());
    drop(writer);

    let mut builder = AxResponse::builder().status(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
    );
    for (k, vs) in &out_headers {
        for v in vs {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }

    if is_stream {
        let rest = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        let stream = futures::stream::iter(first).chain(rest);
        let body: BoxBody<Bytes, std::io::Error> = http_body_util::BodyExt::boxed(StreamBody::new(stream));
        builder
            .body(Body::new(body))
            .unwrap_or_else(|_| AxResp::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap())
    } else {
        let mut buf: Vec<u8> = Vec::new();
        if let Some(Ok(f)) = first {
            if let Ok(data) = f.into_data() {
                buf.extend_from_slice(&data);
            }
        }
        while let Ok(f) = rx.try_recv() {
            if let Ok(data) = f.and_then(|fr| fr.into_data().map_err(|_| std::io::Error::other("trailers"))) {
                buf.extend_from_slice(&data);
            }
        }
        builder
            .body(Body::from(buf))
            .unwrap_or_else(|_| AxResp::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap())
    }
}

fn map_headers(h: &HeaderMap) -> Headers {
    let mut out = Headers::new();
    for (k, v) in h.iter() {
        if let Ok(s) = v.to_str() {
            out.insert(k.as_str().to_string(), vec![s.to_string()]);
        }
    }
    out
}
