//! ASGI-style pure server dispatch for easy-rpc.
//!
//! `dispatch` is the protocol-agnostic application: it maps a core
//! `Request` to a core `Response` given method specs + a service registry.
//! It never imports an HTTP runtime. Backends (hyper, axum, warp, a custom
//! server, ...) only adapt `Request <-> Response` by calling `Handle`.
use bytes::Bytes;
use std::collections::BTreeMap;

use crate::protocol::{Request, Response, MethodSpec, RPCError, frame, http_status};
use crate::server::ServerRegistry;

/// Resolve a response for an RPC request. Async because server-stream handlers
/// may await; unary is sync inside.
pub async fn handle(
    _ctx: &RequestContext,
    req: Request,
    methods: &[MethodSpec],
    reg: &ServerRegistry,
) -> Response {
    let path = req.url.split('?').next().unwrap_or("").to_string();
    let kind = content_kind_headers(&req.headers);
    let ct = if kind == "json" { "application/json" } else { "application/proto" };

    let spec = methods.iter().find(|m| m.path == path);
    let Some(spec) = spec else {
        return err_response(&RPCError { code: 5, message: "not found".into() });
    };

    if spec.server_stream {
        let Some(h) = reg.stream.get(&spec.name) else {
            return err_response(&RPCError { code: 5, message: "method not found".into() });
        };
        let payloads = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let f = payloads.clone();
        let emit = Box::new(move |p: Vec<u8>| -> Result<(), RPCError> { f.lock().unwrap().push(p); Ok(()) });
        let _ = h(req.body.clone().unwrap_or_default().to_vec(), kind.clone(), emit);
        let mut out = Vec::new();
        for p in payloads.lock().unwrap().iter() { out.extend_from_slice(&frame(p, false)); }
        out.extend_from_slice(&frame(&[], true));
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), vec![if kind == "json" { "application/connect+json" } else { "application/connect+proto" }.to_string()]);
        return Response { status: 200, headers, body: Bytes::from(out), error: None };
    }

    let Some(h) = reg.unary.get(&spec.name) else {
        return err_response(&RPCError { code: 5, message: "method not found".into() });
    };
    match h(req.body.clone().unwrap_or_default().to_vec(), kind.clone()) {
        Ok(out) => {
            let mut headers = BTreeMap::new();
            headers.insert("content-type".to_string(), vec![ct.to_string()]);
            Response { status: 200, headers, body: Bytes::from(out), error: None }
        }
        Err(e) => err_response(&e),
    }
}

/// Minimal request context (headers). User-defined middleware can enrich this;
/// easy-rpc only guarantees it carries the incoming headers for authz.
#[derive(Debug, Default, Clone)]
pub struct RequestContext {
    pub headers: BTreeMap<String, Vec<String>>,
}

impl RequestContext {
    pub fn new(headers: BTreeMap<String, Vec<String>>) -> Self { Self { headers } }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.first()).map(|s| s.as_str())
    }
}

fn content_kind_headers(h: &crate::protocol::Headers) -> String {
    if let Some(ct) = h.get("content-type").and_then(|v| v.first()) {
        if ct.starts_with("application/json") { return "json".to_string() }
    }
    if let Some(ac) = h.get("accept").and_then(|v| v.first()) {
        if ac.starts_with("application/json") { return "json".to_string() }
    }
    "proto".to_string()
}

fn err_response(e: &RPCError) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), vec!["text/plain".to_string()]);
    Response {
        status: http_status(e.code),
        headers,
        body: Bytes::from(e.message.clone().into_bytes()),
        error: Some(e.clone()),
    }
}

#[allow(dead_code)]
fn _unused(u: u16) { let _ = u; }
