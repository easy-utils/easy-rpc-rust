//! ASGI-style pure server dispatch for easy-rpc.
//!
//! `handle` is the protocol-agnostic application: it maps a core `Request` into
//! a push-based `ResponseWriter` given method specs + a service registry. It
//! never imports an HTTP runtime. Backends (hyper, axum, warp, a custom server,
//! ...) implement `ResponseWriter` for their transport.
//!
//! Server-stream is written frame-by-frame: the adapter flushes each frame, so
//! responses are truly incremental — never buffered.
use crate::protocol::{
    Headers, MethodSpec, RPCError, Request, encode_end_stream, encode_error_json, frame, http_status,
    parse_timeout, HEADER_TIMEOUT, HEADER_PROTOCOL_VERSION, CONNECT_PROTOCOL_VERSION, DEFAULT_MAX_MESSAGE_BYTES,
    HEADER_ACCEPT_ENCODING, ENCODING_GZIP, COMPRESS_MIN_BYTES, gzip_compress, frame_compressed,
};
use crate::server::ServerRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Push-based server sink. Adapters implement this for their transport.
pub trait ResponseWriter: Send + Sync {
    /// Set the HTTP status (called before the first `write_frame`).
    fn status(&self, code: u16);
    /// Set response headers (called before the first `write_frame`).
    fn header(&self, headers: Headers);
    /// Write one payload: an already-framed stream frame (or the unary body).
    fn write_frame(&self, payload: Vec<u8>) -> Result<(), RPCError>;
}

/// Resolve an RPC request, pushing the response into `w`.
///
/// Deadline: the Connect timeout header is parsed and surfaced to handlers via
/// `RequestContext.deadline_ms`. Rust handlers are synchronous closures, so
/// enforcement is cooperative — a handler that loops must check the context.
pub async fn handle(
    ctx: &RequestContext,
    req: Request,
    methods: &[MethodSpec],
    reg: &ServerRegistry,
    w: Arc<dyn ResponseWriter>,
) {
    let path = req.url.split('?').next().unwrap_or("").to_string();
    let kind = content_kind_headers(&req.headers);
    let ct = if kind == "json" { "application/json" } else { "application/proto" };

    if let Some(pv) = req.headers.get(HEADER_PROTOCOL_VERSION).and_then(|v| v.first()) {
        if pv != CONNECT_PROTOCOL_VERSION {
            return write_error(w.as_ref(), &RPCError { code: 12, message: format!("unsupported connect-protocol-version: {pv}"), ..Default::default() });
        }
    }
    if req.body.as_ref().map(|b| b.len()).unwrap_or(0) > DEFAULT_MAX_MESSAGE_BYTES {
        return write_error(w.as_ref(), &RPCError { code: 8, message: "request too large".into(), ..Default::default() });
    }

    let spec = methods.iter().find(|m| m.path == path);
    let Some(spec) = spec else {
        return write_error(w.as_ref(), &RPCError { code: 5, message: "not found".into(), ..Default::default() });
    };

    if spec.server_stream {
        let Some(h) = reg.stream.get(&spec.name) else {
            return write_error(w.as_ref(), &RPCError { code: 5, message: "method not found".into(), ..Default::default() });
        };
        // Connect semantics: stream is always HTTP 200; failures ride the END frame.
        w.status(200);
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            vec![if kind == "json" { "application/connect+json" } else { "application/connect+proto" }.to_string()],
        );
        w.header(headers);
        let wants_gzip = req
            .headers
            .get(HEADER_ACCEPT_ENCODING)
            .map(|vs| vs.iter().any(|v| v.split(',').any(|e| e.trim() == ENCODING_GZIP)))
            .unwrap_or(false);
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let end_flag = ended.clone();
        let wc = w.clone();
        let emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync> = Box::new(move |p: Vec<u8>| {
            if end_flag.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            if wants_gzip && p.len() >= COMPRESS_MIN_BYTES {
                return wc.write_frame(frame_compressed(&gzip_compress(&p)));
            }
            wc.write_frame(frame(&p, false))
        });
        let result = h(req.body.clone().unwrap_or_default().to_vec(), kind.clone(), &req.headers, emit);
        if ended.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Err(e) = result {
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = w.write_frame(frame(&encode_end_stream(e.code, &e.message, &e.details), true));
            return;
        }
        ended.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = w.write_frame(frame(&[], true));
        return;
    }

    let Some(h) = reg.unary.get(&spec.name) else {
        return write_error(w.as_ref(), &RPCError { code: 5, message: "method not found".into(), ..Default::default() });
    };
    match h(req.body.clone().unwrap_or_default().to_vec(), kind.clone(), &req.headers) {
        Ok(out) => {
            w.status(200);
            let mut headers = BTreeMap::new();
            headers.insert("content-type".to_string(), vec![ct.to_string()]);
            w.header(headers);
            let _ = w.write_frame(out);
        }
        Err(e) => write_error(w.as_ref(), &e),
    }
}

fn write_error(w: &dyn ResponseWriter, e: &RPCError) {
    w.status(http_status(e.code));
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), vec!["application/json".to_string()]);
    w.header(headers);
    let _ = w.write_frame(encode_error_json(e.code, &e.message, &e.details));
}

/// Minimal request context (headers). User-defined middleware can enrich this;
/// easy-rpc only guarantees it carries the incoming headers for authz.
#[derive(Debug, Default, Clone)]
pub struct RequestContext {
    pub headers: BTreeMap<String, Vec<String>>,
    /// Connect deadline in milliseconds (0 = none).
    pub deadline_ms: u64,
}

impl RequestContext {
    pub fn new(headers: BTreeMap<String, Vec<String>>) -> Self {
        let deadline_ms = headers
            .get(HEADER_TIMEOUT)
            .and_then(|v| v.first())
            .map(|s| parse_timeout(s))
            .unwrap_or(0);
        Self { headers, deadline_ms }
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.first()).map(|s| s.as_str())
    }
}

fn content_kind_headers(h: &crate::protocol::Headers) -> String {
    if let Some(ct) = h.get("content-type").and_then(|v| v.first()) {
        if ct.starts_with("application/json") || ct.starts_with("application/connect+json") { return "json".to_string() }
    }
    if let Some(ac) = h.get("accept").and_then(|v| v.first()) {
        if ac.starts_with("application/json") || ac.starts_with("application/connect+json") { return "json".to_string() }
    }
    "proto".to_string()
}
