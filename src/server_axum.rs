//! axum adapter: build an axum::Router for an easy-rpc service registry.
//!
//! This is the "thick" server backend: it lets you mount easy-rpc RPC routes
//! into ANY existing axum application (via `nest` / `merge`) and reuse axum's
//! middleware (auth, tracing, CORS, ...). It is gated behind the optional
//! `axum` feature; core stays dependency-free.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request as AxRequest, Response as AxResponse, StatusCode, HeaderMap};
use axum::response::Response as AxResp;
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;

use crate::protocol::{MethodSpec, Headers};
use crate::server::ServerRegistry;
use crate::dispatch::{handle, RequestContext};

/// Build an axum Router for the given method specs + registry. Nest or merge it
/// into your own application; add middleware via `.layer(...)`.
pub fn router(methods: Vec<MethodSpec>, reg: Arc<ServerRegistry>) -> Router {
    let state = Arc::new((methods, reg));
    Router::new().fallback(any(axum_handle)).with_state(state)
}

async fn axum_handle(
    State(cfg): State<Arc<(Vec<MethodSpec>, Arc<ServerRegistry>)>>,
    req: AxRequest<Body>,
) -> AxResp<Body> {
    let (methods, reg) = cfg.as_ref().clone();
    let (parts, body) = req.into_parts();
    let raw = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let headers = map_headers(&parts.headers);
    let core_req = crate::protocol::Request {
        url: parts.uri.path().to_string(),
        method: parts.method.as_str().to_string(),
        headers: headers.clone(),
        body: Some(Bytes::from(raw)),
    };
    let ctx = RequestContext::new(headers);
    let core_resp = handle(&ctx, core_req, &methods, &reg).await;
    let mut builder = AxResponse::builder()
        .status(StatusCode::from_u16(core_resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (k, vs) in &core_resp.headers {
        for v in vs {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }
    builder.body(Body::from(core_resp.body)).unwrap_or_else(|_| AxResp::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap())
}

fn map_headers(h: &HeaderMap) -> Headers {
    let mut out = Headers::new();
    for (k, v) in h.iter() {
        if let Ok(s) = v.to_str() {
            out.insert(k.as_str().to_string(), vec![s.to_string()]);
        }
    }
    out
}

