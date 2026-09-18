//! Hyper (HTTP/1.1 + h2) bridge implementing easy-rpc Transport.
use crate::protocol::{Request, Response, RPCError, Stream, Transport, read_frame, http_status, connect_from_status};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HReq, Response as HRes};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use tokio::net::TcpStream;

/// Simple blocking-ish async client that connects per request (works for
/// conformance interop; production can reuse a connection pool).
pub struct HyperClient {
    pub base: String,
}

impl HyperClient {
    pub fn new(base: String) -> Self { Self { base } }
    pub fn url(&self, path: &str) -> String { format!("{}{}", self.base, path) }
}

#[async_trait::async_trait]
impl Transport for HyperClient {
    async fn send(&self, req: Request) -> Result<Response, RPCError> {
        let conn = connect(&req.url).await?;
        let (mut sender, mut conn2) = conn;
        tokio::spawn(async move {
            let _ = conn2.await;
        });
        let target = origin_form(&req.url);
        let body = Full::new(req.body.unwrap_or_default());
        let has_ct = req.headers.keys().any(|k| k.eq_ignore_ascii_case("content-type"));
        let mut builder = HReq::builder()
            .method(req.method.as_str())
            .uri(target.as_str())
            .header("host", host_of(&req.url));
        // Caller-supplied headers first; default content-type ONLY when absent.
        for (k, vs) in req.headers.iter() {
            for v in vs {
                builder = builder.header(k.as_str(), v.as_str());
            }
        }
        if !has_ct {
            builder = builder.header("content-type", "application/proto");
        }
        let hreq = builder
            .body(body)
            .map_err(|e| err_box(e.to_string()))?;
        let res = sender.send_request(hreq).await.map_err(|e| err_box(e.to_string()))?;
        let (parts, incoming) = res.into_parts();
        let body = incoming.collect().await.map_err(|e| err_box(e.to_string()))?.to_bytes();
        let status = parts.status.as_u16();
        let mut headers = crate::protocol::Headers::new();
        for (name, value) in parts.headers.iter() {
            let k = name.as_str().to_ascii_lowercase();
            let v = String::from_utf8_lossy(value.as_bytes()).to_string();
            headers.entry(k).or_default().push(v);
        }
        let error = if status >= 300 {
            let hdr_code = headers.get("connect-code").and_then(|v| v.first());
            let (c, m, ds) = crate::protocol::decode_error_json(&body);
            Some(match hdr_code.and_then(|c| c.parse::<i32>().ok()) {
                Some(c) => {
                    // header carries the exact code; body may carry details
                    let msg = headers.get("connect-error").and_then(|v| v.first()).cloned().unwrap_or_default();
                    RPCError { code: c, message: msg, details: ds }
                }
                None if c != 0 => RPCError { code: c, message: m, details: ds },
                None => RPCError {
                    code: crate::protocol::connect_from_status(status),
                    message: String::from_utf8_lossy(&body).to_string(),
                    ..Default::default()
                },
            })
        } else {
            None
        };
        Ok(Response { status, headers, body, error })
    }

    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        let conn = connect(&req.url).await?;
        let (mut sender, mut conn2) = conn;
        tokio::spawn(async move {
            let _ = conn2.await;
        });
        let target = origin_form(&req.url);
        let body = Full::new(req.body.unwrap_or_default());
        let has_ct = req.headers.keys().any(|k| k.eq_ignore_ascii_case("content-type"));
        let mut builder = HReq::builder()
            .method(req.method.as_str())
            .uri(target.as_str())
            .header("host", host_of(&req.url));
        // Caller-supplied headers first; default content-type ONLY when absent.
        for (k, vs) in req.headers.iter() {
            for v in vs {
                builder = builder.header(k.as_str(), v.as_str());
            }
        }
        if !has_ct {
            builder = builder.header("content-type", "application/connect+proto");
        }
        let hreq = builder
            .body(body)
            .map_err(|e| err_box(e.to_string()))?;
        let res = sender.send_request(hreq).await.map_err(|e| err_box(e.to_string()))?;
        let (parts, incoming) = res.into_parts();
        if parts.status.as_u16() >= 300 {
            return Err(RPCError { code: connect_from_status(parts.status.as_u16()), message: "http error".to_string(), ..Default::default() });
        }
        Ok(Box::new(HyperStream { incoming, buf: Vec::new(), err: None, ended: false }))
    }
}

async fn connect(url: &str) -> Result<(http1::SendRequest<Full<Bytes>>, http1::Connection<TokioIo<TcpStream>, Full<Bytes>>), RPCError> {
    let u = url.parse::<http::Uri>().map_err(|e| err_box(e.to_string()))?;
    let host = u.host().map(str::to_string).ok_or_else(|| err_box("no host".to_string()))?;
    let port = u.port_u16().unwrap_or(80);
    let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|e: std::net::AddrParseError| err_box(e.to_string()))?;
    let tcp = TcpStream::connect(addr).await.map_err(|e| err_box(e.to_string()))?;
    let io = TokioIo::new(tcp);
    http1::handshake(io).await.map_err(|e| err_box(e.to_string()))
}

fn host_of(url: &str) -> String {
    url.parse::<http::Uri>().ok().and_then(|u| u.host().map(str::to_string)).unwrap_or_default()
}

fn origin_form(url: &str) -> String {
    url.parse::<http::Uri>().ok()
        .map(|u| u.path_and_query().map(|pq| pq.as_str().to_string()).unwrap_or_else(|| "/".to_string()))
        .unwrap_or_else(|| url.to_string())
}

fn err_box(e: String) -> RPCError {
    RPCError { code: 13, message: e, ..Default::default() }
}

struct HyperStream {
    incoming: Incoming,
    buf: Vec<u8>,
    err: Option<RPCError>,
    ended: bool,
}

#[async_trait::async_trait]
impl Stream for HyperStream {
    async fn recv(&mut self) -> Option<Bytes> {
        loop {
            if self.ended { return None; }
            // Try to parse one complete frame out of the accumulated buffer:
            // frames MAY be split across arbitrary network chunks (fault
            // matrix F5) — a partial chunk must never drop bytes.
            if self.buf.len() >= 5 {
                let flags = self.buf[0];
                let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
                if self.buf.len() >= 5 + len {
                    let payload = self.buf[5..5 + len].to_vec();
                    self.buf.drain(..5 + len);
                    let payload = if flags & 0x01 != 0 {
                        match crate::protocol::gzip_decompress(&payload) {
                            Ok(p) => p,
                            Err(e) => { self.err = Some(e); return None; }
                        }
                    } else { payload };
                    if flags & 0x02 != 0 {
                        self.ended = true;
                        let (code, message, details) = crate::protocol::decode_end_stream(&payload);
                        if code != 0 { self.err = Some(RPCError { code, message, details }); }
                        return None;
                    }
                    return Some(Bytes::from(payload));
                }
            }
            let frame = self.incoming.frame().await;
            match frame {
                Some(Ok(f)) => {
                    if let Some(chunk) = f.data_ref() { self.buf.extend_from_slice(chunk); }
                }
                Some(Err(e)) => {
                    self.err = Some(RPCError { code: 13, message: e.to_string(), ..Default::default() });
                    return None;
                }
                None => {
                    // Fault matrix F2/M8: missing END frame or trailing
                    // partial bytes = truncated mid-stream.
                    if !self.buf.is_empty() {
                        self.err = Some(RPCError { code: 13, message: "truncated frame at end of stream".into(), ..Default::default() });
                    } else if !self.ended {
                        self.err = Some(RPCError { code: 13, message: "stream ended without END frame".into(), ..Default::default() });
                    }
                    return None;
                }
            }
        }
    }
    fn last_error(&self) -> Option<RPCError> { self.err.clone() }
    fn cancel(&mut self) {}
    fn close(&mut self) {}
}

