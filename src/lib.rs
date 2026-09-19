pub mod descriptor_pool;
pub mod protocol;
pub mod interceptors;
pub mod bridge_hyper;
pub mod bridge_reqwest;

// Server surface (protocol dispatch + hyper/axum adapters + registries). Gated
// behind the `server` feature: a client-only build links no HTTP server runtime.
#[cfg(feature = "server")]
pub mod dispatch;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "axum")]
pub mod server_axum;

pub mod google {
    pub mod api {
        include!("google/api/google.api.rs");
    }
}
pub mod connectrpc {
    pub mod conformance {
        pub mod v1 {
            include!("connectrpc/conformance/v1/mod.rs");
        }
    }
}
pub mod easyrpc {
    pub mod conformance {
        pub mod v1 {
            include!("easyrpc/conformance/v1/mod.rs");
        }
    }
}
