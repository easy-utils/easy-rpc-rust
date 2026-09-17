//! Interceptor transport: wraps a `Transport` with a chain of interceptors.
//!
//! Interceptors are the ONE cross-cutting extension point (auth/metadata,
//! deadlines, retry, logging). Because they wrap a `Transport`, they are
//! independent of WHICH adapter is underneath — swapping `ReqwestTransport`
//! for `HyperClient` (or a user transport) keeps every interceptor unchanged.
use std::sync::Arc;

use crate::protocol::{Interceptor, Request, Response, RPCError, Stream, Transport};

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

/// Convenience: wrap a transport with interceptors (first = outermost).
pub fn with_interceptors<I>(inner: Arc<dyn Transport>, interceptors: I) -> InterceptorTransport
where
    I: IntoIterator<Item = Arc<dyn Interceptor>>,
{
    InterceptorTransport::new(interceptors.into_iter().collect(), inner)
}
