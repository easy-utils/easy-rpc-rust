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

/// Encode an END-frame payload in the Connect end-stream JSON shape:
/// `{"error":{"code":"<name>","message":"..."}}`; a clean end is empty.
pub fn encode_end_stream(code: i32, message: &str) -> Vec<u8> {
    if code == 0 {
        return Vec::new();
    }
    let esc = message.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{{\"error\":{{\"code\":\"{}\",\"message\":\"{}\"}}}}", code_to_string(code), esc).into_bytes()
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

/// Encode a Connect unary error body `{code,message}`.
pub fn encode_error_json(code: i32, message: &str) -> Vec<u8> {
    let esc = message.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{{\"code\":\"{}\",\"message\":\"{}\"}}", code_to_string(code), esc).into_bytes()
}

/// Decode a Connect unary error body; (0, "") when not an error body.
pub fn decode_error_json(body: &[u8]) -> (i32, String) {
    if body.is_empty() { return (0, String::new()); }
    let s = match std::str::from_utf8(body) { Ok(s) => s, Err(_) => return (0, String::new()) };
    match extract_json_str(s, "code") {
        Some(name) => (code_from_string(&name), extract_json_str(s, "message").unwrap_or_default()),
        None => (0, String::new()),
    }
}

/// Decode a Connect end-stream payload. `(0, "")` = clean end.
pub fn decode_end_stream(payload: &[u8]) -> (i32, String) {
    if payload.is_empty() {
        return (0, String::new());
    }
    let s = match std::str::from_utf8(payload) { Ok(s) => s, Err(_) => return (0, String::new()) };
    // Minimal parse: pull the "code" and "message" string values.
    let code = extract_json_str(s, "code").map(|c| code_from_string(&c)).unwrap_or(2);
    let message = extract_json_str(s, "message").unwrap_or_default();
    (code, message)
}

fn extract_json_str(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let i = s.find(&needle)? + needle.len();
    let rest = &s[i..];
    let colon = rest.find(':')? + 1;
    let after = rest[colon..].trim_start();
    let after = after.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => { if let Some(n) = chars.next() { out.push(n); } }
            _ => out.push(c),
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

/// An interceptor wraps a call. It may mutate the request (auth/metadata),
/// impose a deadline, or observe/short-circuit. `next` performs the call.
/// Kept object-safe and minimal; bridges apply them around the Transport.
pub trait Interceptor: Send + Sync {
    fn unary<'a>(
        &'a self,
        req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>>;

    fn stream<'a>(
        &'a self,
        req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>>;
}

/// Attach fixed metadata to every call.
pub struct MetadataInterceptor(pub Headers);

impl Interceptor for MetadataInterceptor {
    fn unary<'a>(
        &'a self,
        mut req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>> {
        for (k, v) in &self.0 {
            req.headers.entry(k.clone()).or_insert_with(|| v.clone());
        }
        next(req)
    }
    fn stream<'a>(
        &'a self,
        mut req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>> {
        for (k, v) in &self.0 {
            req.headers.entry(k.clone()).or_insert_with(|| v.clone());
        }
        next(req)
    }
}

/// Attach a Connect deadline to every call.
pub struct TimeoutInterceptor(pub u64);

impl Interceptor for TimeoutInterceptor {
    fn unary<'a>(
        &'a self,
        req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, RPCError>> + Send + 'a>> {
        next(with_timeout(req, self.0))
    }
    fn stream<'a>(
        &'a self,
        req: Request,
        next: Box<dyn FnOnce(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>> + Send + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Stream>, RPCError>> + Send + 'a>> {
        next(with_timeout(req, self.0))
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
