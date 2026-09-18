// easy-rpc Rust conformance server entry (hyper). Serves HTTP/1 + h2c (and h2)
// over the same port via hyper-util's auto connection builder.
//
// easy-rpc v2: proto-only, POST-only, gRPC-style paths. Handlers receive a
// `&HandlerContext` (metadata + trailer channel).
use easy_rpc::easyrpc::conformance::v1::method_specs;
use easy_rpc::protocol::HandlerContext;
use easy_rpc::server::{hyper_serve, ServerRegistry};
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

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0".to_string());
    let listener = TcpListener::bind((bind.as_str(), port)).await?;
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
    use easy_rpc::easyrpc::conformance::v1::*;
    use easy_rpc::protocol::{decode, encode, ErrorDetail, RPCError};
    use easy_rpc::server::{StreamHandler, UnaryHandler};

    let mut unary = std::collections::HashMap::new();
    let mut stream = std::collections::HashMap::new();

    unary.insert(
        "Health".to_string(),
        Box::new(|_req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            Ok(encode(&HealthResponse { ok: true, name: "conformance".to_string() }).to_vec())
        }) as UnaryHandler,
    );

    unary.insert(
        "Echo".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoRequest = decode(&req).map_err(internal)?;
            Ok(encode(&EchoResponse { output: format!("echo:{}", m.input) }).to_vec())
        }) as UnaryHandler,
    );

    unary.insert(
        "Fail".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: FailRequest = decode(&req).map_err(internal)?;
            if m.message.is_empty() {
                Ok(encode(&FailResponse { ok: true }).to_vec())
            } else {
                Err(RPCError::new(3, m.message))
            }
        }) as UnaryHandler,
    );

    unary.insert(
        "FailDetails".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: FailDetailsRequest = decode(&req).map_err(internal)?;
            Err(RPCError::new(m.code, m.message).with_details(vec![ErrorDetail {
                type_: m.detail_type,
                value: m.detail_text.into_bytes(),
            }]))
        }) as UnaryHandler,
    );

    unary.insert(
        "EchoMeta".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoMetaRequest = decode(&req).map_err(internal)?;
            let mut meta = std::collections::HashMap::new();
            for k in ["x-test", "authorization"] {
                if let Some(v) = ctx.headers.get(k).and_then(|x| x.first()) {
                    meta.insert(k.to_string(), v.clone());
                }
            }
            Ok(encode(&EchoMetaResponse { input: m.input, meta }).to_vec())
        }) as UnaryHandler,
    );

    unary.insert(
        "Big".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: BigRequest = decode(&req).map_err(internal)?;
            Ok(encode(&BigResponse { size: m.size }).to_vec())
        }) as UnaryHandler,
    );

    unary.insert(
        "EchoTrailer".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoTrailerRequest = decode(&req).map_err(internal)?;
            ctx.set_trailer("x-trl", &format!("unary-{}", m.input));
            Ok(encode(&EchoTrailerResponse { output: format!("trailer:{}", m.input) }).to_vec())
        }) as UnaryHandler,
    );

    stream.insert(
        "Count".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: CountRequest = decode(&req).map_err(internal)?;
            let n = if m.count > 0 { m.count } else { 3 };
            for i in 0..n {
                let _ = emit(encode(&CountResponse { index: i }).to_vec());
            }
            Ok(())
        }) as StreamHandler,
    );

    stream.insert(
        "StreamFail".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: StreamFailRequest = decode(&req).map_err(internal)?;
            for i in 0..m.emit_before {
                let _ = emit(encode(&StreamFailResponse { index: i }).to_vec());
            }
            Err(RPCError::new(m.code, m.message))
        }) as StreamHandler,
    );

    stream.insert(
        "StreamFailDetails".to_string(),
        Box::new(|req: Vec<u8>, _ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: StreamFailDetailsRequest = decode(&req).map_err(internal)?;
            for i in 0..m.emit_before {
                let _ = emit(encode(&StreamFailDetailsResponse { index: i }).to_vec());
            }
            Err(RPCError::new(m.code, m.message).with_details(vec![ErrorDetail {
                type_: m.detail_type,
                value: m.detail_text.into_bytes(),
            }]))
        }) as StreamHandler,
    );

    stream.insert(
        "CountTrailer".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: CountTrailerRequest = decode(&req).map_err(internal)?;
            ctx.set_trailer("x-ctrailer", "done");
            let n = if m.count > 0 { m.count } else { 3 };
            for i in 0..n {
                let _ = emit(encode(&CountTrailerResponse { index: i }).to_vec());
            }
            Ok(())
        }) as StreamHandler,
    );

    ServerRegistry { unary, stream }
}

fn internal(e: std::io::Error) -> easy_rpc::protocol::RPCError {
    easy_rpc::protocol::RPCError::new(13, e.to_string())
}
