//! easy-rpc Rust core: zero-runtime-bindings Transport + Connect wire
//! (unary + server-stream). Bridges adapt a concrete HTTP runtime.
use bytes::Bytes;
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

/// A structured error detail (spec §4.1, aligned with Connect Error Details /
/// gRPC google.rpc status details). `type_` is the wire "type" (a type URL);
/// `value` is opaque bytes (typically an encoded protobuf message).
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorDetail {
    pub type_: String,
    pub value: Vec<u8>,
}

/// Wire-level error with a Connect code.
#[derive(Clone, Debug)]
pub struct RPCError {
    pub code: i32,
    pub message: String,
    /// Optional structured details (spec §4.1); opaque to the wire layer.
    pub details: Vec<ErrorDetail>,
}
impl Default for RPCError {
    fn default() -> Self { RPCError { code: 0, message: String::new(), details: Vec::new() } }
}
impl RPCError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        RPCError { code, message: message.into(), ..Default::default() }
    }
    /// Attach structured details (builder style).
    pub fn with_details(mut self, details: Vec<ErrorDetail>) -> Self {
        self.details = details;
        self
    }
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
    /// Set when the stream ended with a Connect end-stream error.
    fn last_error(&self) -> Option<RPCError> { None }
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
        1 => 499,
        3 => 400,
        4 => 504,
        5 => 404,
        6 => 409,
        7 => 403,
        8 => 429,
        9 => 400,
        10 => 409,
        11 => 400,
        12 => 501,
        14 => 503,
        16 => 401,
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
        409 => 10,
        504 => 4,
        501 => 12,
        499 => 1,
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

/// Connect code -> stable lowercase wire name.
pub fn code_to_string(code: i32) -> &'static str {
    match code {
        0 => "ok", 1 => "canceled", 2 => "unknown", 3 => "invalid_argument",
        4 => "deadline_exceeded", 5 => "not_found", 6 => "already_exists",
        7 => "permission_denied", 8 => "resource_exhausted", 9 => "failed_precondition",
        10 => "aborted", 11 => "out_of_range", 12 => "unimplemented", 13 => "internal",
        14 => "unavailable", 15 => "data_loss", 16 => "unauthenticated",
        _ => "unknown",
    }
}

/// Wire code name -> Connect code (unknown -> 2).
pub fn code_from_string(name: &str) -> i32 {
    match name {
        "ok" => 0, "canceled" => 1, "unknown" => 2, "invalid_argument" => 3,
        "deadline_exceeded" => 4, "not_found" => 5, "already_exists" => 6,
        "permission_denied" => 7, "resource_exhausted" => 8,
        "failed_precondition" => 9, "aborted" => 10, "out_of_range" => 11,
        "unimplemented" => 12, "internal" => 13, "unavailable" => 14,
        "data_loss" => 15, "unauthenticated" => 16,
        _ => 2,
    }
}

fn wire_details(details: &[ErrorDetail]) -> Vec<serde_json::Value> {
    details.iter()
        .map(|d| serde_json::json!({"type": d.type_, "value": b64_encode(&d.value)}))
        .collect()
}

fn parse_wire_details(v: Option<&serde_json::Value>) -> Vec<ErrorDetail> {
    let arr = match v.and_then(|x| x.as_array()) { Some(a) => a, None => return Vec::new() };
    let mut out = Vec::new();
    for el in arr {
        let t = el.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let val = el.get("value").and_then(|x| x.as_str()).unwrap_or("");
        if t.is_empty() || val.is_empty() {
            continue; // malformed entry: skip, never fatal (matrix M7)
        }
        if let Some(bytes) = b64_decode(val) {
            out.push(ErrorDetail { type_: t.to_string(), value: bytes });
        }
    }
    out
}

/// Encode an END-frame payload in the Connect end-stream JSON shape:
/// `{"error":{"code":"<name>","message":"..."}}`; a clean end is empty.
/// Details (spec §4.1) are included when non-empty.
pub fn encode_end_stream(code: i32, message: &str, details: &[ErrorDetail]) -> Vec<u8> {
    if code == 0 {
        return Vec::new();
    }
    let mut err = serde_json::json!({"code": code_to_string(code), "message": message});
    if !details.is_empty() {
        err["details"] = serde_json::Value::Array(wire_details(details));
    }
    serde_json::to_vec(&serde_json::json!({"error": err})).unwrap_or_default()
}

// ---- gzip (opt-in) ----

/// Compression headers + threshold.
pub const HEADER_ACCEPT_ENCODING: &str = "connect-accept-encoding";
pub const ENCODING_GZIP: &str = "gzip";
pub const COMPRESS_MIN_BYTES: usize = 1024;

/// gzip-compress data.
pub fn gzip_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// gzip-decompress data (identity on failure).
pub fn gzip_decompress(data: &[u8]) -> Vec<u8> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut out = Vec::new();
    if GzDecoder::new(data).read_to_end(&mut out).is_ok() {
        out
    } else {
        data.to_vec()
    }
}

/// Frame a payload with the Compressed flag set.
pub fn frame_compressed(payload: &[u8]) -> Vec<u8> {
    let mut out = frame(payload, false);
    out[0] |= 0x01;
    out
}

/// Encode a Connect unary error body `{code,message[,details]}`.
pub fn encode_error_json(code: i32, message: &str, details: &[ErrorDetail]) -> Vec<u8> {
    let mut body = serde_json::json!({"code": code_to_string(code), "message": message});
    if !details.is_empty() {
        body["details"] = serde_json::Value::Array(wire_details(details));
    }
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Decode a Connect unary error body; (0, "") when not an error body.
/// Malformed bodies return (0, "", []); malformed detail entries are skipped
/// (matrix M7).
pub fn decode_error_json(body: &[u8]) -> (i32, String, Vec<ErrorDetail>) {
    if body.is_empty() { return (0, String::new(), Vec::new()); }
    let v: serde_json::Value = match serde_json::from_slice(body) { Ok(v) => v, Err(_) => return (0, String::new(), Vec::new()) };
    let name = match v.get("code").and_then(|x| x.as_str()) { Some(n) => n, None => return (0, String::new(), Vec::new()) };
    (code_from_string(name),
     v.get("message").and_then(|x| x.as_str()).unwrap_or("").to_string(),
     parse_wire_details(v.get("details")))
}

/// Decode a Connect end-stream payload. `(0, "")` = clean end; malformed
/// input is a clean end (matrix M2), and an error object without a code maps
/// to code 2 (M3/M4). Unknown fields are ignored (M5).
pub fn decode_end_stream(payload: &[u8]) -> (i32, String, Vec<ErrorDetail>) {
    if payload.is_empty() {
        return (0, String::new(), Vec::new());
    }
    let v: serde_json::Value = match serde_json::from_slice(payload) { Ok(v) => v, Err(_) => return (0, String::new(), Vec::new()) };
    let err = match v.get("error") { Some(e) => e, None => return (0, String::new(), Vec::new()) };
    let code = match err.get("code").and_then(|x| x.as_str()) {
        Some(c) => code_from_string(c),
        None => 2,
    };
    (code,
     err.get("message").and_then(|x| x.as_str()).unwrap_or("").to_string(),
     parse_wire_details(err.get("details")))
}

// Minimal, dependency-free base64 (standard alphabet, padded) for error
// details; details are small so a simple table decoder is fine.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16) | ((*chunk.get(1).unwrap_or(&0) as u32) << 8) | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    if s.len() % 4 != 0 { return None; }
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None, // strict: invalid char rejects the whole value (M7)
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Decode one frame from a byte buffer, returning (payload, end, consumed).
pub fn read_frame(buf: &[u8]) -> Option<(Vec<u8>, bool, usize)> {
    if buf.len() < 5 {
        return None;
    }
    let flags = buf[0];
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len > DEFAULT_MAX_MESSAGE_BYTES {
        return None;
    }
    if buf.len() < 5 + len {
        return None;
    }
    let payload = buf[5..5 + len].to_vec();
    let end = (flags & FLAG_END_STREAM) != 0;
    Some((payload, end, 5 + len))
}

/// The Connect request-timeout header.
pub const HEADER_TIMEOUT: &str = "connect-timeout-ms";

/// The Connect protocol-version header + the version we speak.
pub const HEADER_PROTOCOL_VERSION: &str = "connect-protocol-version";
pub const CONNECT_PROTOCOL_VERSION: &str = "1";

/// Default read/write size cap (Connect default).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Parse the Connect timeout header into milliseconds (0 = none).
pub fn parse_timeout(value: &str) -> u64 {
    if value.is_empty() { return 0; }
    value.trim().parse::<u64>().unwrap_or(0)
}

/// Attach a deadline to a request's headers.
pub fn with_timeout(mut req: Request, timeout_ms: u64) -> Request {
    if timeout_ms == 0 { return req; }
    req.headers.insert(HEADER_TIMEOUT.to_string(), vec![timeout_ms.to_string()]);
    req
}

/// Default gRPC-style path.
pub fn url_for(pkg: &str, svc: &str, method: &str) -> String {
    format!("/{pkg}.{svc}/{method}")
}

/// An interceptor observes/rewrites a call. To stay object-safe and ergonomic
/// in Rust, it is a pre-hook (`intercept`) plus an optional post-hook
/// (`observe`). Metadata, deadlines, and logging are all expressible this way.
/// A wrap-style interceptor (retry / circuit breaking) can compose several
/// calls inside `intercept` if needed.
#[async_trait::async_trait]
pub trait Interceptor: Send + Sync {
    /// Rewrite the request before the call, or return an error to short-circuit.
    async fn intercept(&self, req: Request) -> Result<Request, RPCError> {
        Ok(req)
    }
    /// Observe a completed call (success case).
    async fn observe(&self, _req: &Request, _resp: &Response) {}
}

/// Attach fixed metadata to every call.
pub struct MetadataInterceptor(pub Headers);

#[async_trait::async_trait]
impl Interceptor for MetadataInterceptor {
    async fn intercept(&self, mut req: Request) -> Result<Request, RPCError> {
        for (k, v) in &self.0 {
            req.headers.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Ok(req)
    }
}

/// Attach a Connect deadline to every call (header; adapters honour it locally
/// too when they support cancellation).
pub struct TimeoutInterceptor(pub u64);

#[async_trait::async_trait]
impl Interceptor for TimeoutInterceptor {
    async fn intercept(&self, req: Request) -> Result<Request, RPCError> {
        Ok(with_timeout(req, self.0))
    }
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
