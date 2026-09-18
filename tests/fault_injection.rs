//! Fault injection (spec §4.2 M8/M10 + F2 end-to-end): a mock server emits
//! malformed stream bodies; the client MUST surface errors (via `last_error`),
//! never partial payloads, never raw compressed bytes.
use std::io::{Read, Write};
use std::net::TcpListener;

use bytes::Bytes;
use easy_rpc::bridge_reqwest::ReqwestTransport;
use easy_rpc::protocol::{RPCError, Transport};
use flate2::read::GzDecoder;

fn frame(payload: &[u8], end: bool, compressed: bool) -> Vec<u8> {
    let flags = (if end { 0x02u8 } else { 0 }) | (if compressed { 0x01 } else { 0 });
    let mut out = vec![flags];
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Minimal HTTP/1.1 mock: one connection, one response with the given body.
fn serve(body: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf); // request (ignored)
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.write_all(&body);
        }
    });
    port
}

async fn collect(port: u16) -> (Vec<u8>, Option<RPCError>) {
    let t = ReqwestTransport::new(format!("http://127.0.0.1:{port}"));
    let mut st = t
        .open_stream(easy_rpc::protocol::Request {
            url: "/x".into(),
            
            headers: Default::default(),
            body: None,
        })
        .await
        .expect("open");
    let mut out = Vec::new();
    while let Some(b) = st.recv().await {
        out.extend_from_slice(&b);
    }
    (out, st.last_error())
}

fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut d = GzDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

#[tokio::test]
async fn f1_mid_frame_truncation_errors() {
    let mut body = frame(&[0], false, false);
    let full = frame(&[1, 1, 1, 1, 1, 1, 1, 1], false, false);
    body.extend_from_slice(&full[..full.len() - 4]); // cut mid-payload
    let port = serve(body);
    let (out, err) = collect(port).await;
    assert_eq!(out, vec![0]);
    assert!(err.is_some(), "F1: truncated frame must error");
}

#[tokio::test]
async fn f2_missing_end_frame_errors() {
    let body = frame(&[0], false, false);
    let port = serve(body);
    let (out, err) = collect(port).await;
    assert_eq!(out, vec![0]);
    let e = err.expect("F2: missing END frame must error");
    assert_eq!(e.code, 13);
}

#[tokio::test]
async fn f3_garbage_end_is_clean() {
    let mut body = frame(&[0], false, false);
    body.extend_from_slice(&frame(&[0xff, 0xfe, 0x42], true, false));
    let port = serve(body);
    let (out, err) = collect(port).await;
    assert_eq!(out, vec![0]);
    assert!(err.is_none(), "F3: garbage END is a clean end, got {err:?}");
}

#[tokio::test]
async fn f4_corrupt_gzip_errors_never_raw() {
    let corrupt = [0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef];
    let mut body = frame(&corrupt, false, true);
    body.extend_from_slice(&frame(&[], true, false));
    let port = serve(body);
    let (out, err) = collect(port).await;
    assert!(out.is_empty(), "F4: must never yield raw compressed bytes");
    assert!(err.is_some(), "F4: corrupt gzip must error");
}

#[tokio::test]
async fn f6_valid_gzip_decodes() {
    let gz = {
        use flate2::write::GzEncoder;
        use std::io::Write as _;
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&[7]).unwrap();
        e.finish().unwrap()
    };
    let mut body = frame(&gz, false, true);
    body.extend_from_slice(&frame(&[], true, false));
    let port = serve(body);
    let (out, err) = collect(port).await;
    assert_eq!(out, vec![7]);
    assert!(err.is_none());
}

#[test]
fn frame_compressed_actually_compresses() {
    // Regression: frame_compressed used to flag but not compress.
    let data = vec![0u8; 2048];
    let f = easy_rpc::protocol::frame_compressed(&data);
    assert_eq!(f[0] & 0x01, 1);
    assert!(f.len() < 5 + data.len(), "must actually compress");
    assert_eq!(gunzip(&f[5..]), data);
    let _ = Bytes::new();
}
