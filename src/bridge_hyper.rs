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
        let hreq = HReq::builder()
            .method(req.method.as_str())
            .uri(target.as_str())
            .header("host", host_of(&req.url))
            .header("content-type", "application/proto")
            .body(body)
            .map_err(|e| err_box(e.to_string()))?;
        let res = sender.send_request(hreq).await.map_err(|e| err_box(e.to_string()))?;
        let (parts, incoming) = res.into_parts();
        let body = incoming.collect().await.map_err(|e| err_box(e.to_string()))?.to_bytes();
        Ok(Response { status: parts.status.as_u16(), headers: Default::default(), body, error: None })
    }

    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        let conn = connect(&req.url).await?;
        let (mut sender, mut conn2) = conn;
        tokio::spawn(async move {
            let _ = conn2.await;
        });
        let target = origin_form(&req.url);
        let body = Full::new(req.body.unwrap_or_default());
        let hreq = HReq::builder()
            .method(req.method.as_str())
            .uri(target.as_str())
            .header("host", host_of(&req.url))
            .header("content-type", "application/proto")
            .body(body)
            .map_err(|e| err_box(e.to_string()))?;
        let res = sender.send_request(hreq).await.map_err(|e| err_box(e.to_string()))?;
        let (parts, incoming) = res.into_parts();
        if parts.status.as_u16() >= 300 {
            return Err(RPCError { code: connect_from_status(parts.status.as_u16()), message: "http error".to_string() });
        }
        Ok(Box::new(HyperStream { incoming }))
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

struct HyperStream { incoming: Incoming }
#[async_trait::async_trait]
impl Stream for HyperStream {
    async fn recv(&mut self) -> Option<Bytes> {
        loop {
            let frame = self.incoming.frame().await;
            match frame {
                Some(Ok(f)) => {
                    let chunk = f.into_data().ok()?;
                    if let Some((payload, end, _)) = read_frame(&chunk) {
                        if end { return None; }
                        return Some(Bytes::from(payload));
                    }
                }
                Some(Err(_)) | None => return None,
            }
        }
    }
    fn cancel(&mut self) {}
    fn close(&mut self) {}
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
    RPCError { code: 13, message: e }
}
