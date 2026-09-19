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
    Headers, MethodSpec, RPCError, Request, encode_end_stream_meta, encode_error_json, frame, frame_compressed,
    http_status, parse_timeout, gzip_decompress, HEADER_TIMEOUT, HEADER_PROTOCOL_VERSION, CONNECT_PROTOCOL_VERSION,
    DEFAULT_MAX_MESSAGE_BYTES, HEADER_ACCEPT_ENCODING, ENCODING_GZIP, COMPRESS_MIN_BYTES, gzip_compress,
    CONTENT_TYPE_UNARY, CONTENT_TYPE_STREAM, mux_trailers, read_frame, HandlerContext,
    content_kind_of, is_stream_content_type, content_type_for, ContentKind,
};
use crate::server::ServerRegistry;
use std::sync::Arc;

/// Merge handler-set response headers into a base map (base keys win, handler
/// values are appended so multi-value headers survive).
fn merge_response_headers(base: Headers, extra: &Headers) -> Headers {
    let mut out = base;
    for (k, vs) in extra {
        out.entry(k.clone()).or_default().extend(vs.iter().cloned());
    }
    out
}

/// Count the frames in an enveloped request body. Returns Err on a corrupt
/// header.
fn count_frames(body: &[u8]) -> Result<usize, RPCError> {
    let mut off = 0usize;
    let mut n = 0usize;
    while off < body.len() {
        if off + 5 > body.len() {
            return Err(RPCError::new(13, "truncated frame header"));
        }
        let len = u32::from_be_bytes([body[off + 1], body[off + 2], body[off + 3], body[off + 4]]) as usize;
        if len > DEFAULT_MAX_MESSAGE_BYTES {
            return Err(RPCError::new(8, "frame too large"));
        }
        off += 5 + len;
        n += 1;
    }
    Ok(n)
}

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

    if let Some(pv) = req.headers.get(HEADER_PROTOCOL_VERSION).and_then(|v| v.first()) {
        if !pv.is_empty() && pv != CONNECT_PROTOCOL_VERSION {
            return write_error(w.as_ref(), &RPCError { code: 12, message: format!("unsupported connect-protocol-version: {pv}"), ..Default::default() });
        }
    }

    // POST-only (spec §0). A non-POST verb is 405 (code 2).
    if let Some(m) = req.headers.get(":method").and_then(|v| v.first()) {
        if !m.is_empty() && m != "POST" {
            return write_error_status(w.as_ref(), &RPCError::new(2, format!("method {m} not allowed")), 405);
        }
    }

    if req.body.as_ref().map(|b| b.len()).unwrap_or(0) > DEFAULT_MAX_MESSAGE_BYTES {
        return write_error(w.as_ref(), &RPCError { code: 8, message: "request too large".into(), ..Default::default() });
    }

    let spec = methods.iter().find(|m| m.path == path);
    // Unknown path -> 404 with code 12 (unimplemented), matching Connect.
    let Some(spec) = spec else {
        return write_error_status(w.as_ref(), &RPCError::new(12, "unimplemented"), 404);
    };

    // codec + shape negotiation (spec §2): proto (default) or proto3 JSON; the
    // content type also encodes the shape, which must match the method.
    let got_ct = req.headers.get("content-type").and_then(|v| v.first()).cloned().unwrap_or_default();
    let kind = content_kind_of(&got_ct);
    let stream_shape = is_stream_content_type(&got_ct);
    let Some(kind) = kind else {
        return write_error_status(w.as_ref(), &RPCError::new(2, format!("unsupported content-type: {got_ct}")), 415);
    };
    if stream_shape != spec.server_stream {
        return write_error_status(w.as_ref(), &RPCError::new(2, format!("unsupported content-type: {got_ct}")), 415);
    }

    let mut ctx = HandlerContext::new(req.headers.clone());
    ctx.kind = kind;

    if spec.server_stream {
        let Some(h) = reg.stream.get(&spec.name) else {
            return stream_fail_kind(w.as_ref(), &RPCError::new(12, "no handler"), kind);
        };
        // Request compression for streams uses `connect-content-encoding`.
        let req_enc = req.headers.get("connect-content-encoding").and_then(|v| v.first())
            .map(|s| s.trim().to_lowercase()).unwrap_or_default();
        if !req_enc.is_empty() && req_enc != "identity" && req_enc != ENCODING_GZIP {
            return stream_fail_kind(w.as_ref(), &RPCError::new(12, format!("unsupported content-encoding: {req_enc}")), kind);
        }
        // A server-stream request MUST carry exactly one enveloped message;
        // zero frames or more than one => unimplemented (Connect semantics).
        let frame_count = match count_frames(req.body.as_deref().unwrap_or(&[])) {
            Ok(n) => n,
            Err(e) => return stream_fail_kind(w.as_ref(), &e, kind),
        };
        if frame_count != 1 {
            let msg = if frame_count == 0 { "missing request message" } else { "server-stream request must contain exactly one message" };
            return stream_fail_kind(w.as_ref(), &RPCError::new(12, msg), kind);
        }
        // Unframe the enveloped single-request frame (decompress if flagged).
        let req_body = match read_frame(req.body.as_deref().unwrap_or(&[])) {
            Some((payload, _end, _used)) => {
                if req.body.as_deref().unwrap_or(&[]).first().map(|f| f & 0x01 != 0).unwrap_or(false) {
                    match gzip_decompress(&payload) {
                        Ok(p) => p,
                        Err(e) => return stream_fail_kind(w.as_ref(), &e, kind),
                    }
                } else {
                    payload
                }
            }
            None => return stream_fail_kind(w.as_ref(), &RPCError::new(13, "stream request: truncated frame"), kind),
        };
        // Connect semantics: stream is always HTTP 200; failures ride the END
        // frame. Handler-set headers are applied lazily on the first emit.
        w.status(200);
        let wants_gzip = req
            .headers
            .get(HEADER_ACCEPT_ENCODING)
            .map(|vs| vs.iter().any(|v| v.split(',').any(|e| e.trim() == ENCODING_GZIP)))
            .unwrap_or(false);
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let end_flag = ended.clone();
        let applied_flag = applied.clone();
        let wc = w.clone();
        let ctxc = ctx.clone();
        let emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync> = Box::new(move |p: Vec<u8>| {
            if end_flag.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            if !applied_flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let mut base = Headers::new();
                base.insert("content-type".to_string(), vec![content_type_for(true, kind).to_string()]);
                wc.header(merge_response_headers(base, &ctxc.response_headers()));
            }
            if wants_gzip && p.len() >= COMPRESS_MIN_BYTES {
                return wc.write_frame(frame_compressed(&gzip_compress(&p)));
            }
            wc.write_frame(frame(&p, false))
        });
        let result = h(req_body, &ctx, emit);
        // Ensure headers are set even for an empty successful stream.
        if !applied.load(std::sync::atomic::Ordering::SeqCst) {
            let mut base = Headers::new();
            base.insert("content-type".to_string(), vec![content_type_for(true, kind).to_string()]);
            w.header(merge_response_headers(base, &ctx.response_headers()));
        }
        if ended.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        ended.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Err(e) = result {
            let _ = w.write_frame(frame(&encode_end_stream_meta(e.code, &e.message, &e.details, &ctx.trailers()), true));
            return;
        }
        let _ = w.write_frame(frame(&encode_end_stream_meta(0, "", &[], &ctx.trailers()), true));
        return;
    }

    let Some(h) = reg.unary.get(&spec.name) else {
        return write_error(w.as_ref(), &RPCError { code: 5, message: "method not found".into(), ..Default::default() });
    };
    // Request compression (spec §3.5): unary uses `Content-Encoding: gzip`.
    let mut in_body = req.body.clone().unwrap_or_default().to_vec();
    let req_enc = req.headers.get("content-encoding").and_then(|v| v.first())
        .map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if !req_enc.is_empty() && req_enc != ENCODING_GZIP {
        return write_error(w.as_ref(), &RPCError::new(12, format!("unsupported content-encoding: {req_enc}")));
    }
    if req_enc == ENCODING_GZIP && !in_body.is_empty() {
        match gzip_decompress(&in_body) {
            Ok(b) => in_body = b,
            Err(e) => return write_error(w.as_ref(), &e),
        }
    }
    match h(in_body, &ctx) {
        Ok(out) => {
            w.status(200);
            let wants_gzip = req
                .headers
                .get(HEADER_ACCEPT_ENCODING)
                .map(|vs| vs.iter().any(|v| v.split(',').any(|e| e.trim() == ENCODING_GZIP)))
                .unwrap_or(false);
            let mut headers = Headers::new();
            let body = if wants_gzip && out.len() >= COMPRESS_MIN_BYTES {
                headers.insert("content-encoding".to_string(), vec![ENCODING_GZIP.to_string()]);
                gzip_compress(&out)
            } else {
                out
            };
            headers = mux_trailers(&headers, &ctx.trailers());
            headers = merge_response_headers(headers, &ctx.response_headers());
            headers.insert("content-type".to_string(), vec![content_type_for(false, kind).to_string()]);
            w.header(headers);
            let _ = w.write_frame(body);
        }
        Err(e) => {
            let mut headers = Headers::new();
            headers.insert("content-type".to_string(), vec!["application/json".to_string()]);
            w.status(http_status(e.code));
            headers = merge_response_headers(headers, &ctx.response_headers());
            w.header(mux_trailers(&headers, &ctx.trailers()));
            let _ = w.write_frame(encode_error_json(e.code, &e.message, &e.details));
        }
    }
}

/// Emit a server-stream failure: HTTP 200 + END frame carrying the error.
fn stream_fail(w: &dyn ResponseWriter, e: &RPCError) {
    stream_fail_kind(w, e, ContentKind::Proto);
}

fn stream_fail_kind(w: &dyn ResponseWriter, e: &RPCError, kind: ContentKind) {
    w.status(200);
    let mut headers = Headers::new();
    headers.insert("content-type".to_string(), vec![content_type_for(true, kind).to_string()]);
    w.header(headers);
    let _ = w.write_frame(frame(&encode_end_stream_meta(e.code, &e.message, &e.details, &Headers::new()), true));
}

fn write_error_status(w: &dyn ResponseWriter, e: &RPCError, status: u16) {
    w.status(status);
    let mut headers = Headers::new();
    headers.insert("content-type".to_string(), vec!["application/json".to_string()]);
    w.header(headers);
    let _ = w.write_frame(encode_error_json(e.code, &e.message, &e.details));
}

fn write_error(w: &dyn ResponseWriter, e: &RPCError) {
    w.status(http_status(e.code));
    let mut headers = Headers::new();
    headers.insert("content-type".to_string(), vec!["application/json".to_string()]);
    w.header(headers);
    let _ = w.write_frame(encode_error_json(e.code, &e.message, &e.details));
}

/// Minimal request context (headers). User-defined middleware can enrich this;
/// easy-rpc only guarantees it carries the incoming headers for authz.
#[derive(Debug, Default, Clone)]
pub struct RequestContext {
    pub headers: Headers,
    /// Connect deadline in milliseconds (0 = none).
    pub deadline_ms: u64,
}

impl RequestContext {
    pub fn new(headers: Headers) -> Self {
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

