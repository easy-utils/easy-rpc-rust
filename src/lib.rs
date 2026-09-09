pub mod protocol;
pub mod bridge_hyper;
pub mod bridge_reqwest;
pub mod google {
    pub mod api {
        include!("google/api/google.api.rs");
    }
}
pub mod easyrpc {
    pub mod conformance {
        pub mod v1 {
            include!("easyrpc/conformance/v1/mod.rs");
        }
    }
}
