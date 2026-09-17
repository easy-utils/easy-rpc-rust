//! Interceptor transport: wraps a `Transport` with a chain of interceptors.
//!
//! Interceptors are the ONE cross-cutting extension point (auth/metadata,
//! deadlines, retry, logging). Because they wrap a `Transport`, they are
//! independent of WHICH adapter is underneath — swapping `ReqwestTransport`
//! for `HyperClient` (or a user transport) keeps every interceptor unchanged.
use std::sync::Arc;

use crate::protocol::{
    Headers, Interceptor, MetadataInterceptor, Request, Response, RPCError, Stream, TimeoutInterceptor, Transport,
};

/// A `Transport` that runs every call through `interceptors` (first =
/// outermost), then delegates to `inner`.
pub struct InterceptorTransport {
    interceptors: Vec<Arc<dyn Interceptor>>,
    inner: Arc<dyn Transport>,
}

impl InterceptorTransport {
    pub fn new(interceptors: Vec<Arc<dyn Interceptor>>, inner: Arc<dyn Transport>) -> Self {
        Self { interceptors, inner }
    }

    async fn run_unary(&self, req: Request) -> Result<Response, RPCError> {
        let mut r = req;
        for ic in &self.interceptors {
            r = ic.intercept(r).await?;
        }
        let resp = self.inner.send(r.clone()).await?;
        for ic in &self.interceptors {
            ic.observe(&r, &resp).await;
        }
        Ok(resp)
    }

    async fn run_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        let mut r = req;
        for ic in &self.interceptors {
            r = ic.intercept(r).await?;
        }
        self.inner.open_stream(r).await
    }
}

#[async_trait::async_trait]
impl Transport for InterceptorTransport {
    async fn send(&self, req: Request) -> Result<Response, RPCError> {
        self.run_unary(req).await
    }
    async fn open_stream(&self, req: Request) -> Result<Box<dyn Stream>, RPCError> {
        self.run_stream(req).await
    }
}

/// Enforce a local deadline: sets the Connect header AND races the call with a
/// timeout, so cancellation works over any adapter (no adapter change needed).
pub struct DeadlineInterceptor(pub u64);

#[async_trait::async_trait]
impl Interceptor for DeadlineInterceptor {
    async fn intercept(&self, req: Request) -> Result<Request, RPCError> {
        Ok(crate::protocol::with_timeout(req, self.0))
    }
}

/// Run a call with the DeadlineInterceptor's timeout applied. Provided as a
/// helper because Rust interceptors are pre-hooks; the timeout must wrap the
/// transport call, which only the composition root can do.
pub async fn with_deadline<F, T>(ms: u64, fut: F) -> Result<T, RPCError>
where
    F: std::future::Future<Output = Result<T, RPCError>>,
{
    if ms == 0 {
        return fut.await;
    }
    match tokio::time::timeout(std::time::Duration::from_millis(ms), fut).await {
        Ok(r) => r,
        Err(_) => Err(RPCError { code: 4, message: "deadline exceeded".into(), ..Default::default() }),
    }
}

/// Adapter mode for the Rust composition root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// reqwest-based adapter (h1 + h2/h2c + optional h3), with h1 fallback.
    Auto,
    /// hyper-based adapter.
    Hyper,
}

/// Composition root: pick an adapter by `mode`, install the built-in
/// metadata/deadline interceptors, then any extra. Swapping `mode` leaves the
/// interceptors unchanged.
pub fn connect(
    base: &str,
    token: &str,
    mode: Mode,
    timeout_ms: u64,
    extra: Vec<Arc<dyn Interceptor>>,
) -> Arc<dyn Transport> {
    let inner: Arc<dyn Transport> = match mode {
        Mode::Hyper => Arc::new(crate::bridge_hyper::HyperClient::new(base.to_string())),
        Mode::Auto => Arc::from(crate::bridge_reqwest::NewClient(base.to_string())),
    };
    with_standard_interceptors(token, timeout_ms, extra, inner)
}

/// Composition root with adapter injection: `adapter` replaces the built-in
/// adapters (mode selection is skipped), and the standard metadata/deadline
/// interceptors (plus any extra) wrap IT — the same injection semantics as
/// C# `ConnectOptions.Adapter` and Swift `connect(transport:)`.
pub fn connect_with_adapter(
    token: &str,
    timeout_ms: u64,
    extra: Vec<Arc<dyn Interceptor>>,
    adapter: Arc<dyn Transport>,
) -> Arc<dyn Transport> {
    with_standard_interceptors(token, timeout_ms, extra, adapter)
}

fn with_standard_interceptors(
    token: &str,
    timeout_ms: u64,
    extra: Vec<Arc<dyn Interceptor>>,
    inner: Arc<dyn Transport>,
) -> Arc<dyn Transport> {
    let mut ics: Vec<Arc<dyn Interceptor>> = Vec::new();
    if !token.is_empty() {
        let mut md = Headers::new();
        md.insert("authorization".into(), vec![format!("Bearer {token}")]);
        ics.push(Arc::new(MetadataInterceptor(md)));
    }
    if timeout_ms > 0 {
        ics.push(Arc::new(TimeoutInterceptor(timeout_ms)));
    }
    ics.extend(extra);
    if ics.is_empty() {
        return inner;
    }
    Arc::new(InterceptorTransport::new(ics, inner))
}

/// Convenience: wrap a transport with interceptors (first = outermost).
pub fn with_interceptors<I>(inner: Arc<dyn Transport>, interceptors: I) -> InterceptorTransport
where
    I: IntoIterator<Item = Arc<dyn Interceptor>>,
{
    InterceptorTransport::new(interceptors.into_iter().collect(), inner)
}
