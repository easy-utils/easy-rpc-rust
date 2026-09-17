// easy-rpc Rust conformance server entry (hyper). Serves HTTP/1 + h2c (and h2)
// over the same port via hyper-util's auto connection builder, which accepts
// both HTTP/1 and HTTP/2 (cleartext prior-knowledge) on one listener.
use easy_rpc::server::{hyper_serve, ServerRegistry};
use easy_rpc::easyrpc::conformance::v1::method_specs;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioExecutor};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;

#[derive(Clone)]
struct Ctx {
    reg: std::sync::Arc<ServerRegistry>,
    methods: std::sync::Arc<Vec<easy_rpc::protocol::MethodSpec>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(18888);
    let methods = method_specs();
    let reg = build_registry();
    let ctx = Ctx { reg: std::sync::Arc::new(reg), methods: std::sync::Arc::new(methods) };

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("rust on {}", port);
    loop {
        let (stream, _) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let builder = auto::Builder::new(TokioExecutor::new());
            let service = service_fn(move |req| {
                let ctx = ctx.clone();
                async move { hyper_serve(ctx.methods.clone(), ctx.reg.clone(), req).await }
            });
            if let Err(e) = builder.serve_connection(io, service).await {
                eprintln!("conn err: {}", e);
            }
        });
    }
}

fn build_registry() -> ServerRegistry {
    use easy_rpc::protocol::{encode, RPCError};
    use easy_rpc::server::StreamHandler;
    use easy_rpc::easyrpc::conformance::v1::*;
    let mut unary = std::collections::HashMap::new();
    unary.insert("Health".to_string(), Box::new(
        move |_req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            if kind == "json" {
                Ok(br#"{"ok":true,"name":"conformance"}"#.to_vec())
            } else {
                Ok(encode(&HealthResponse { ok: true, name: "conformance".to_string() }).to_vec())
            }
        }) as easy_rpc::server::UnaryHandler);
    unary.insert("Echo".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                let input = v.get("input").and_then(|x| x.as_str()).unwrap_or("");
                Ok(serde_json::json!({"output": format!("echo:{}", input)}).to_string().into_bytes())
            } else {
                let m: EchoRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                Ok(encode(&EchoResponse { output: format!("echo:{}", m.input) }).to_vec())
            }
        }) as easy_rpc::server::UnaryHandler);
    unary.insert("Fail".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
                Ok(serde_json::json!({"ok": msg.is_empty()}).to_string().into_bytes())
            } else {
                let m: FailRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                Ok(encode(&FailResponse { ok: m.message.is_empty() }).to_vec())
            }
        }) as easy_rpc::server::UnaryHandler);
    unary.insert("FailDetails".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            let (code, message, dtype, dtext) = if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (v.get("code").and_then(|x| x.as_i64()).unwrap_or(8) as i32,
                 v.get("message").and_then(|x| x.as_str()).unwrap_or("limited").to_string(),
                 v.get("detailType").and_then(|x| x.as_str()).unwrap_or("t/x").to_string(),
                 v.get("detailText").and_then(|x| x.as_str()).unwrap_or("d").to_string())
            } else {
                let m: FailDetailsRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (m.code, m.message, m.detail_type, m.detail_text)
            };
            Err(RPCError { code, message, details: vec![easy_rpc::protocol::ErrorDetail { type_: dtype, value: dtext.into_bytes() }] })
        }) as easy_rpc::server::UnaryHandler);
    unary.insert("EchoMeta".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                let input = v.get("input").and_then(|x| x.as_str()).unwrap_or("");
                let mut meta = serde_json::Map::new();
                for k in ["x-test", "authorization"] {
                    if let Some(v) = h.get(k).and_then(|x| x.first()) {
                        meta.insert(k.to_string(), serde_json::Value::String(v.clone()));
                    }
                }
                Ok(serde_json::json!({"input": input, "meta": meta}).to_string().into_bytes())
            } else {
                let m: EchoMetaRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                let mut meta = std::collections::HashMap::new();
                for k in ["x-test", "authorization"] {
                    if let Some(v) = h.get(k).and_then(|x| x.first()) {
                        meta.insert(k.to_string(), v.clone());
                    }
                }
                Ok(encode(&EchoMetaResponse { input: m.input, meta }).to_vec())
            }
        }) as easy_rpc::server::UnaryHandler);
    unary.insert("Big".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers| -> Result<Vec<u8>, RPCError> {
            if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                let size = v.get("size").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                Ok(serde_json::json!({"size": size}).to_string().into_bytes())
            } else {
                let m: BigRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                Ok(encode(&BigResponse { size: m.size }).to_vec())
            }
        }) as easy_rpc::server::UnaryHandler);
    let mut stream = std::collections::HashMap::new();
    stream.insert("Count".to_string(), Box::new(
        move |_req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            for i in 0..3 {
                if kind == "json" {
                    let _ = emit(serde_json::json!({"index": i}).to_string().into_bytes());
                } else {
                    let _ = emit(encode(&CountResponse { index: i }).to_vec());
                }
            }
            Ok(())
        }) as easy_rpc::server::StreamHandler);
    stream.insert("StreamFail".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let (before, code, message) = if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (v.get("emitBefore").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
                 v.get("code").and_then(|x| x.as_i64()).unwrap_or(13) as i32,
                 v.get("message").and_then(|x| x.as_str()).unwrap_or("boom").to_string())
            } else {
                let m: StreamFailRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (m.emit_before, m.code, m.message)
            };
            for i in 0..before {
                if kind == "json" {
                    let _ = emit(serde_json::json!({"index": i}).to_string().into_bytes());
                } else {
                    let _ = emit(encode(&StreamFailResponse { index: i }).to_vec());
                }
            }
            Err(RPCError { code, message, ..Default::default() })
        }) as easy_rpc::server::StreamHandler);
    stream.insert("StreamFailDetails".to_string(), Box::new(
        move |req: Vec<u8>, kind: String, _h: &easy_rpc::protocol::Headers, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let (before, code, message, dtype, dtext) = if kind == "json" {
                let v: serde_json::Value = serde_json::from_slice(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (v.get("emitBefore").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
                 v.get("code").and_then(|x| x.as_i64()).unwrap_or(13) as i32,
                 v.get("message").and_then(|x| x.as_str()).unwrap_or("boom").to_string(),
                 v.get("detailType").and_then(|x| x.as_str()).unwrap_or("t/s").to_string(),
                 v.get("detailText").and_then(|x| x.as_str()).unwrap_or("sd").to_string())
            } else {
                let m: StreamFailDetailsRequest = easy_rpc::protocol::decode(&req).map_err(|e| RPCError { code: 13, message: e.to_string(), ..Default::default() })?;
                (m.emit_before, m.code, m.message, m.detail_type, m.detail_text)
            };
            for i in 0..before {
                if kind == "json" {
                    let _ = emit(serde_json::json!({"index": i}).to_string().into_bytes());
                } else {
                    let _ = emit(encode(&StreamFailDetailsResponse { index: i }).to_vec());
                }
            }
            Err(RPCError { code, message, details: vec![easy_rpc::protocol::ErrorDetail { type_: dtype, value: dtext.into_bytes() }] })
        }) as easy_rpc::server::StreamHandler);
    ServerRegistry { unary, stream }
}
