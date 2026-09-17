//! Reqwest bridge implementing easy-rpc Transport.
//!
//! reqwest is the recommended Rust client: it provides a connection pool, TLS
//! and (behind the `h3` feature + `--cfg reqwest_unstable`) HTTP/3. The `h3`
//! feature enables `NewAuto` which negotiates h3 -> h2 -> h1; the `h1h2`
//! feature builds without any QUIC dependency and only does h1/h2/h2c.
//!
//! This replaces the previous hand-rolled hyper client. The hyper *server*
//! (hyper_serve in protocol.rs) is unchanged.
use crate::protocol::{Request, Response, RPCError, Stream, Transport, connect_from_status};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::{Client, Method};
use std::sync::Arc;

/// ReqwestTransport is a reqwest::Client-backed Transport. It negotiates
/// HTTP/1, HTTP/2 (and h2c where supported) automatically.
pub struct ReqwestTransport {
    client: Client,
    base: String,
}

impl ReqwestTransport {
    pub fn new(base: String) -> Self {
        Self { client: Client::new(), base }
    }

    pub fn with_client(base: String, client: Client) -> Self {
        Self { client, base }
    }

    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        }
    }
}

/// NewClient returns a Transport. With the `h3` feature it is an AutoTransport
/// that prefers HTTP/3 then falls back to h2/h2c then h1. Without it (the
/// `h1h2` feature) it is a plain reqwest h1/h2 transport.
pub fn NewClient(base: String) -> Box<dyn Transport> {
    #[cfg(feature = "h3")]
    {
        return Box::new(AutoTransport::new(base));
    }
    #[allow(unreachable_code)]
    return Box::new(ReqwestTransport::new(base));
}

/// AutoTransport negotiates h3 -> h2 -> h1 using reqwest's http3 feature.
#[cfg(feature = "h3")]
pub struct AutoTransport {
    base: String,
    client: Client,
}

#[cfg(feature = "h3")]
impl AutoTransport {
    pub fn new(base: String) -> Self {
        Self {
            base: base.clone(),
            client: reqwest::Client::builder().http3_prior_knowledge().build().unwrap(),
        }
    }
}

#[cfg(feature = "h3")]
#[async_trait::async_trait]
impl Transport for AutoTransport {
    async fn send(&self, req: Request) -> Result<Response, RPCError> {
        send_with(&self.client, &self.base, req).await
    }
    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        open_stream_with(&self.client, &self.base, req).await
    }
}

#[async_trait::async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, req: Request) -> Result<Response, RPCError> {
        send_with(&self.client, &self.base, req).await
    }
    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        open_stream_with(&self.client, &self.base, req).await
    }
}

async fn send_with(client: &Client, base: &str, req: Request) -> Result<Response, RPCError> {
    let url = join(base, &req.url);
    let method = Method::from_bytes(req.method.as_bytes()).map_err(|e| err_box(e.to_string()))?;
    let mut rb = client.request(method, url);
    for (k, vs) in req.headers.clone() {
        for v in vs {
            rb = rb.header(k.clone(), v);
        }
    }
    rb = rb.header("content-type", "application/proto");
    let resp = rb.body(req.body.clone().unwrap_or_default()).send().await.map_err(|e| err_box(e.to_string()))?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = if status < 300 {
        resp.bytes().await.map_err(|e| err_box(e.to_string()))?
    } else {
        resp.bytes().await.unwrap_or_default()
    };
    let msg = String::from_utf8_lossy(&bytes).to_string();
    let err = if status >= 300 {
        let (c, m, ds) = crate::protocol::decode_error_json(&bytes);
        Some(if c != 0 { RPCError { code: c, message: m, details: ds } } else { RPCError { code: connect_from_status(status), message: msg, ..Default::default() } })
    } else { None };
    Ok(Response {
        status,
        headers: reqwest_headers_to_headers(&headers),
        body: bytes,
        error: err,
    })
}

async fn open_stream_with(client: &Client, base: &str, req: Request) -> Result<Box<dyn Stream>, RPCError> {
    let url = join(base, &req.url);
    let method = Method::from_bytes(req.method.as_bytes()).map_err(|e| err_box(e.to_string()))?;
    let mut rb = client.request(method, url);
    for (k, vs) in req.headers.clone() {
        for v in vs {
            rb = rb.header(k.clone(), v);
        }
    }
    rb = rb.header("content-type", "application/connect+proto");
    let resp = rb.body(req.body.clone().unwrap_or_default()).send().await.map_err(|e| err_box(e.to_string()))?;
    if resp.status().as_u16() >= 300 {
        return Err(RPCError { code: connect_from_status(resp.status().as_u16()), message: "http error".to_string(), ..Default::default() });
    }
    let mut stream = resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, String>>();
    tokio::spawn(async move {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => { let _ = tx.send(Ok(bytes)); }
                Err(e) => { let _ = tx.send(Err(e.to_string())); break }
            }
        }
    });
    Ok(Box::new(ReqwestStream { rx, acc: Bytes::new(), err: None }))
}

struct ReqwestStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<Result<Bytes, String>>,
    acc: Bytes,
    err: Option<RPCError>,
}

#[async_trait::async_trait]
impl Stream for ReqwestStream {
    async fn recv(&mut self) -> Option<Bytes> {
        loop {
            while self.acc.len() >= 5 {
                let flags = self.acc[0];
                let len = u32::from_be_bytes([self.acc[1], self.acc[2], self.acc[3], self.acc[4]]) as usize;
                if self.acc.len() < 5 + len { break }
                let payload = self.acc.slice(5..5 + len);
                self.acc = self.acc.slice(5 + len..);
                let payload = if flags & 0x01 != 0 {
                    Bytes::from(crate::protocol::gzip_decompress(&payload))
                } else { payload };
                if flags & 0x02 != 0 {
                    let (code, message, details) = crate::protocol::decode_end_stream(&payload);
                    if code != 0 {
                        self.err = Some(RPCError { code, message, details });
                    }
                    return None;
                }
                return Some(payload);
            }
            match self.rx.recv().await {
                Some(Ok(bytes)) => { self.acc = Bytes::from([self.acc.to_vec(), bytes.to_vec()].concat()); }
                Some(Err(_)) | None => return None,
            }
        }
    }
    fn last_error(&self) -> Option<RPCError> { self.err.clone() }
    fn cancel(&mut self) {}
    fn close(&mut self) {}
}

fn join(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{}{}", base.trim_end_matches('/'), path)
    }
}

fn reqwest_headers_to_headers(h: &reqwest::header::HeaderMap) -> crate::protocol::Headers {
    let mut out = crate::protocol::Headers::new();
    for (k, v) in h.iter() {
        if let Ok(s) = v.to_str() {
            out.insert(k.as_str().to_string(), vec![s.to_string()]);
        }
    }
    out
}

fn err_box(e: String) -> RPCError {
    RPCError { code: 13, message: e, ..Default::default() }
}
