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
    use easy_rpc::protocol::{decode_msg, encode_msg, ContentKind, ErrorDetail, RPCError};
    use easy_rpc::server::{StreamHandler, UnaryHandler};

    // Codec-aware helpers: handlers decode/encode by the request's ContentKind.
    fn dec<M: prost::Message + Default + prost::Name>(b: &[u8], k: ContentKind) -> Result<M, RPCError> {
        decode_msg(b, k)
    }
    fn enc<M: prost::Message + prost::Name>(m: &M, k: ContentKind) -> Vec<u8> {
        encode_msg(m, k).unwrap_or_default()
    }

    let mut unary = std::collections::HashMap::new();
    let mut stream = std::collections::HashMap::new();

    unary.insert(
        "Health".to_string(),
        Box::new(|_req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            Ok(enc(&HealthResponse { ok: true, name: "conformance".to_string() }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "Echo".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            Ok(enc(&EchoResponse { output: format!("echo:{}", m.input) }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "Fail".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: FailRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            if m.message.is_empty() {
                Ok(enc(&FailResponse { ok: true }, ctx.kind))
            } else {
                Err(RPCError::new(3, m.message))
            }
        }) as UnaryHandler,
    );

    unary.insert(
        "FailDetails".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: FailDetailsRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            Err(RPCError::new(m.code, m.message).with_details(vec![ErrorDetail {
                type_: m.detail_type,
                value: m.detail_text.into_bytes(),
            }]))
        }) as UnaryHandler,
    );

    unary.insert(
        "EchoMeta".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoMetaRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            let mut meta = std::collections::HashMap::new();
            for k in ["x-test", "authorization"] {
                if let Some(v) = ctx.headers.get(k).and_then(|x| x.first()) {
                    meta.insert(k.to_string(), v.clone());
                }
            }
            Ok(enc(&EchoMetaResponse { input: m.input, meta }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "Big".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: BigRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            Ok(enc(&BigResponse { size: m.size }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "EchoBytes".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoBytesRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            Ok(enc(&EchoBytesResponse { data: m.data }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "Sleep".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: SleepRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            // Honor the Connect deadline (M12/M13).
            let timeout: i32 = ctx
                .headers
                .get("connect-timeout-ms")
                .and_then(|v| v.first())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if timeout > 0 && timeout < m.millis {
                std::thread::sleep(std::time::Duration::from_millis(timeout as u64));
                return Err(RPCError::new(4, "deadline exceeded"));
            }
            if m.millis > 0 {
                std::thread::sleep(std::time::Duration::from_millis(m.millis as u64));
            }
            Ok(enc(&SleepResponse { ok: true }, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "Empty".to_string(),
        Box::new(|_req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            Ok(enc(&EmptyResponse {}, ctx.kind))
        }) as UnaryHandler,
    );

    unary.insert(
        "EchoTrailer".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m: EchoTrailerRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            ctx.set_trailer("x-trl", &format!("unary-{}", m.input));
            Ok(enc(&EchoTrailerResponse { output: format!("trailer:{}", m.input) }, ctx.kind))
        }) as UnaryHandler,
    );

    stream.insert(
        "BigStream".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: BigStreamRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            let n = if m.count > 0 { m.count } else { 3 };
            for i in 0..n {
                let _ = emit(enc(&BigStreamResponse { index: i, size: m.size }, ctx.kind));
            }
            Ok(())
        }) as StreamHandler,
    );

    stream.insert(
        "Count".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: CountRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            let n = if m.count > 0 { m.count } else { 3 };
            for i in 0..n {
                let _ = emit(enc(&CountResponse { index: i }, ctx.kind));
            }
            Ok(())
        }) as StreamHandler,
    );

    stream.insert(
        "StreamFail".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: StreamFailRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            for i in 0..m.emit_before {
                let _ = emit(enc(&StreamFailResponse { index: i }, ctx.kind));
            }
            Err(RPCError::new(m.code, m.message))
        }) as StreamHandler,
    );

    stream.insert(
        "StreamFailDetails".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext, emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>| -> Result<(), RPCError> {
            let m: StreamFailDetailsRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            for i in 0..m.emit_before {
                let _ = emit(enc(&StreamFailDetailsResponse { index: i }, ctx.kind));
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
            let m: CountTrailerRequest = dec(&req, ctx.kind).map_err(rpc_internal)?;
            ctx.set_trailer("x-ctrailer", "done");
            let n = if m.count > 0 { m.count } else { 3 };
            for i in 0..n {
                let _ = emit(enc(&CountTrailerResponse { index: i }, ctx.kind));
            }
            Ok(())
        }) as StreamHandler,
    );

    ServerRegistry { unary, stream }
}

fn rpc_internal(e: easy_rpc::protocol::RPCError) -> easy_rpc::protocol::RPCError {
    e
}
