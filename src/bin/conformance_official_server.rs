// Official ConnectRPC conformance server (easy-rpc, Rust). Server-under-test
// mode: reads a size-prefixed ServerCompatRequest from stdin, starts an
// easy-rpc hyper server (HTTP/1.1 + h2c) on an ephemeral port implementing the
// official connectrpc.conformance.v1.ConformanceService, then writes the
// size-prefixed ServerCompatResponse to stdout.
use easy_rpc::connectrpc::conformance::v1::*;
use easy_rpc::connectrpc::conformance::v1::conformance_payload::RequestInfo as ConformancePayloadRequestInfo;
use easy_rpc::connectrpc::conformance::v1::Error as ConfError;
use easy_rpc::connectrpc::conformance::v1::method_specs;
use easy_rpc::protocol::{ErrorDetail, HandlerContext, RPCError};
use easy_rpc::server::{hyper_serve, ServerRegistry, StreamHandler, UnaryHandler};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use prost::Message;
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::net::TcpListener;

fn main() {
    // The hyper server runs on a multi-thread tokio runtime; the main thread
    // blocks on stdin.
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    let mut len_buf = [0u8; 4];
    if std::io::stdin().read_exact(&mut len_buf).is_err() {
        return;
    }
    let n = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; n];
    if std::io::stdin().read_exact(&mut body).is_err() {
        return;
    }
    let req = ServerCompatRequest::decode(&body[..]).expect("decode ServerCompatRequest");

    let methods = method_specs();
    let reg = build_registry();
    let ctx = Srv { reg: Arc::new(reg), methods: Arc::new(methods) };

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().unwrap().port();

    let resp = ServerCompatResponse { host: "127.0.0.1".to_string(), port: port as u32, pem_cert: Vec::new() };
    let out = resp.encode_to_vec();
    let mut hdr = [0u8; 4];
    hdr.copy_from_slice(&(out.len() as u32).to_be_bytes());
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(&hdr);
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let builder = auto::Builder::new(TokioExecutor::new());
            let service = service_fn(move |req| {
                let ctx = ctx.clone();
                async move { hyper_serve(ctx.methods.clone(), ctx.reg.clone(), req).await }
            });
            let _ = builder.serve_connection(io, service).await;
        });
    }
}

#[derive(Clone)]
struct Srv {
    reg: Arc<ServerRegistry>,
    methods: Arc<Vec<easy_rpc::protocol::MethodSpec>>,
}

const HEADER_TIMEOUT: &str = "connect-timeout-ms";

fn request_any<M: Message>(msg: &M, name: &str) -> prost_types::Any {
    prost_types::Any {
        type_url: format!("type.googleapis.com/connectrpc.conformance.v1.{name}"),
        value: msg.encode_to_vec(),
    }
}

fn make_request_info(headers: &easy_rpc::protocol::Headers, requests: Vec<prost_types::Any>) -> ConformancePayloadRequestInfo {
    // One Header per distinct name, with ALL values (the runner's checker reads
    // the last Header for a name, so a per-value split would drop values).
    let mut request_headers = Vec::new();
    for (k, vs) in headers {
        request_headers.push(Header { name: k.clone(), value: vs.clone() });
    }
    let timeout_ms = headers
        .get(HEADER_TIMEOUT)
        .and_then(|v| v.first())
        .and_then(|s| s.trim().parse::<i64>().ok());
    ConformancePayloadRequestInfo { request_headers, timeout_ms, requests, connect_get_info: None }
}

/// Build an `RPCError` from an official error definition, appending the
/// request-info Any to the details (the runner expects it).
fn to_rpc_error(e: &ConfError, info: Option<ConformancePayloadRequestInfo>) -> RPCError {
    let mut details: Vec<ErrorDetail> = e
        .details
        .iter()
        .map(|a| {
            let bare = a.type_url.rsplit('/').next().unwrap_or("").to_string();
            ErrorDetail { type_: bare, value: a.value.clone() }
        })
        .collect();
    if let Some(i) = info {
        details.push(ErrorDetail {
            type_: "connectrpc.conformance.v1.ConformancePayload.RequestInfo".to_string(),
            value: i.encode_to_vec(),
        });
    }
    RPCError {
        code: e.code,
        message: e.message.clone().unwrap_or_default(),
        details,
    }
}

fn apply_headers(list: &[Header], ctx: &HandlerContext, trailer: bool) {
    for h in list {
        for v in &h.value {
            if trailer {
                ctx.set_trailer(&h.name, v);
            } else {
                ctx.set_header(&h.name, v);
            }
        }
    }
}

fn build_registry() -> ServerRegistry {
    let mut unary = std::collections::HashMap::new();
    let mut stream = std::collections::HashMap::new();

    unary.insert(
        "Unary".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m = UnaryRequest::decode(&req[..]).map_err(internal)?;
            let info = make_request_info(&ctx.headers, vec![request_any(&m, "UnaryRequest")]);
            let def = match m.response_definition {
                Some(d) => d,
                None => return Ok(UnaryResponse { payload: Some(payload(None, Some(info))) }.encode_to_vec()),
            };
            apply_headers(&def.response_headers, ctx, false);
            apply_headers(&def.response_trailers, ctx, true);
            match def.response {
                Some(unary_response_definition::Response::Error(e)) => Err(to_rpc_error(&e, Some(info))),
                data => {
                    if def.response_delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(def.response_delay_ms as u64));
                    }
                    let data = match data {
                        Some(unary_response_definition::Response::ResponseData(d)) => d,
                        _ => Vec::new(),
                    };
                    Ok(UnaryResponse { payload: Some(payload(Some(data), Some(info))) }.encode_to_vec())
                }
            }
        }) as UnaryHandler,
    );

    unary.insert(
        "IdempotentUnary".to_string(),
        Box::new(|req: Vec<u8>, ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            let m = IdempotentUnaryRequest::decode(&req[..]).map_err(internal)?;
            let info = make_request_info(&ctx.headers, vec![request_any(&m, "IdempotentUnaryRequest")]);
            Ok(IdempotentUnaryResponse { payload: Some(payload(None, Some(info))) }.encode_to_vec())
        }) as UnaryHandler,
    );

    unary.insert(
        "Unimplemented".to_string(),
        Box::new(|_req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            Err(RPCError::new(12, "unimplemented"))
        }) as UnaryHandler,
    );

    unary.insert(
        "ClientStream".to_string(),
        Box::new(|_req: Vec<u8>, _ctx: &HandlerContext| -> Result<Vec<u8>, RPCError> {
            Err(RPCError::new(12, "client streaming is not supported"))
        }) as UnaryHandler,
    );

    stream.insert(
        "ServerStream".to_string(),
        Box::new(
            |req: Vec<u8>,
             ctx: &HandlerContext,
             emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>|
             -> Result<(), RPCError> {
                let m = ServerStreamRequest::decode(&req[..]).map_err(internal)?;
                let info = make_request_info(&ctx.headers, vec![request_any(&m, "ServerStreamRequest")]);
                let def = match m.response_definition {
                    Some(d) => d,
                    None => return Ok(()),
                };
                apply_headers(&def.response_headers, ctx, false);
                apply_headers(&def.response_trailers, ctx, true);
                let mut first = true;
                for data in &def.response_data {
                    if def.response_delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(def.response_delay_ms as u64));
                    }
                    let info_here = if first { Some(info.clone()) } else { None };
                    let resp = ServerStreamResponse { payload: Some(payload(Some(data.clone()), info_here)) };
                    emit(resp.encode_to_vec())?;
                    first = false;
                }
                if let Some(e) = &def.error {
                    return Err(to_rpc_error(e, if first { Some(info) } else { None }));
                }
                Ok(())
            },
        ) as StreamHandler,
    );

    stream.insert(
        "BidiStream".to_string(),
        Box::new(
            |_req: Vec<u8>,
             _ctx: &HandlerContext,
             _emit: Box<dyn Fn(Vec<u8>) -> Result<(), RPCError> + Send + Sync>|
             -> Result<(), RPCError> { Err(RPCError::new(12, "bidi streaming is not supported")) },
        ) as StreamHandler,
    );

    ServerRegistry { unary, stream }
}

fn payload(data: Option<Vec<u8>>, info: Option<ConformancePayloadRequestInfo>) -> ConformancePayload {
    ConformancePayload { data: data.unwrap_or_default(), request_info: info }
}

fn internal(e: prost::DecodeError) -> RPCError {
    RPCError::new(13, e.to_string())
}
