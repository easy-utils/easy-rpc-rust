//! easy-rpc Rust core: zero-runtime-bindings Transport + Connect wire
//! (unary + server-stream). Bridges adapt a concrete HTTP runtime.
use bytes::Bytes;
use http_body_util::Full;
use prost::Message;
use std::collections::BTreeMap;

pub type Headers = BTreeMap<String, Vec<String>>;

/// Normalized RPC request (protocol-agnostic).
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Headers,
    pub body: Option<Bytes>,
}

/// Normalized response.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Bytes,
    pub error: Option<RPCError>,
}

/// Wire-level error with a Connect code.
#[derive(Clone, Debug)]
pub struct RPCError {
    pub code: i32,
    pub message: String,
}
impl std::fmt::Display for RPCError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "easyrpc: code={} {}", self.code, self.message)
    }
}

/// Stream of server-stream message bytes (dyn-compatible via async_trait).
#[async_trait::async_trait]
pub trait Stream: Send {
    /// Next raw (de-framed) message; None at end.
    async fn recv(&mut self) -> Option<Bytes>;
    fn cancel(&mut self);
    fn close(&mut self);
}

/// The core interface a bridge must implement.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, req: Request) -> Result<Response, RPCError>;
    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError>;
}

// ---- connect code mapping ----
pub fn http_status(code: i32) -> u16 {
    match code {
        3 => 400,
        5 => 404,
        7 => 403,
        8 => 429,
        16 => 401,
        14 => 503,
        _ => 500,
    }
}
pub fn connect_from_status(status: u16) -> i32 {
    match status {
        400 => 3,
        404 => 5,
        403 => 7,
        401 => 16,
        429 => 8,
        503 => 14,
        _ => 13,
    }
}

// ---- framing ----
const FLAG_COMPRESSED: u8 = 0x01;
const FLAG_END_STREAM: u8 = 0x02;

/// Encode a single streaming frame.
pub fn frame(payload: &[u8], end_stream: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(if end_stream { FLAG_END_STREAM } else { 0 });
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode one frame from a byte buffer, returning (payload, end, consumed).
pub fn read_frame(buf: &[u8]) -> Option<(Vec<u8>, bool, usize)> {
    if buf.len() < 5 {
        return None;
    }
    let flags = buf[0];
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 5 + len {
        return None;
    }
    let payload = buf[5..5 + len].to_vec();
    let end = (flags & FLAG_END_STREAM) != 0;
    Some((payload, end, 5 + len))
}

/// Default gRPC-style path.
pub fn url_for(pkg: &str, svc: &str, method: &str) -> String {
    format!("/{pkg}.{svc}/{method}")
}

/// MethodSpec mirrors generated metadata.
#[derive(Clone, Debug)]
pub struct MethodSpec {
    pub service: String,
    pub name: String,
    pub path: String,
    pub http_method: String,
    pub client_stream: bool,
    pub server_stream: bool,
    pub body: String,
}

/// Encode a prost message to bytes.
pub fn encode<M: Message>(m: &M) -> Bytes {
    Bytes::from(m.encode_to_vec())
}
/// Decode bytes into a prost message.
pub fn decode<M: Message + Default>(b: &[u8]) -> Result<M, std::io::Error> {
    <M as Message>::decode(b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

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
