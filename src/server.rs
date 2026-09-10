//! Optional easy-rpc server module (hyper adapter + ServerRegistry). Gated
//! behind the `server` feature so a client-only build links no HTTP server
//! runtime. Import the server surface from here, not from `protocol`.
use crate::protocol::{Headers, Request, Response, RPCError, MethodSpec, frame};
use bytes::Bytes;
use http_body_util::Full;

// ---- server-side (hyper) ----
pub type UnaryHandler = Box<dyn Fn(Vec<u8>, String) -> Result<Vec<u8>, RPCError> + Send + Sync>;
pub type StreamHandler = Box<dyn Fn(Vec<u8>, String, Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>) -> Result<(), RPCError> + Send + Sync>;

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

/// Build a hyper service_fn handler from method specs + a registry.
pub async fn hyper_serve(
    methods: &[MethodSpec],
    reg: &ServerRegistry,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Full<bytes::Bytes>>, std::convert::Infallible> {
    let uri = req.uri().clone();
    let path = uri.path();
    let method = req.method().clone();
    let req_headers = req.headers().clone();
    let body = { use http_body_util::BodyExt; req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default() };
    let mut req_headers2 = Headers::new();
    if let Some(ct) = req_headers.get("content-type") {
        req_headers2.insert("content-type".to_string(), vec![ct.to_str().unwrap_or("").to_string()]);
    }
    let req2 = Request {
        url: uri.to_string(), method: method.to_string(),
        headers: req_headers2, body: Some(bytes::Bytes::copy_from_slice(&body)),
    };
    let kind = content_kind(&req2);
    let spec = methods.iter().find(|m| m.path == path);
    let Some(spec) = spec else {
        return Ok(hyper::Response::builder().status(404).body(Full::new(bytes::Bytes::new())).unwrap());
    };
    for (name, h) in &reg.unary {
        if *name == spec.name {
            match h(req2.body.clone().unwrap_or_default().to_vec(), kind.clone()) {
                Ok(out) => {
                    return Ok(hyper::Response::builder()
                        .header("content-type", if kind=="json"{"application/json"}else{"application/proto"})
                        .body(Full::new(bytes::Bytes::from(out))).unwrap());
                }
                Err(e) => return Ok(hyper::Response::builder().status(e.code as u16+300).body(Full::new(bytes::Bytes::from(e.message))).unwrap()),
            }
        }
    }
    for (name, h) in &reg.stream {
        if *name == spec.name {
            let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
            let f2 = frames.clone();
            let emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync> =
                Box::new(move |p: Vec<u8>| { f2.lock().unwrap().push(p); Ok(()) });
            let _ = h(req2.body.clone().unwrap_or_default().to_vec(), kind.clone(), emit);
            let mut body = Vec::new();
            for fr in frames.lock().unwrap().iter() { body.extend_from_slice(&frame(fr, false)); }
            return Ok(hyper::Response::builder()
                .header("content-type", if kind=="json"{"application/connect+json"}else{"application/connect+proto"})
                .body(Full::new(bytes::Bytes::from(body))).unwrap());
        }
    }
    Ok(hyper::Response::builder().status(404).body(Full::new(bytes::Bytes::new())).unwrap())
}
