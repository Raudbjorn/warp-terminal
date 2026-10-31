use std::sync::Once;

pub mod autocomplete;
mod config;
mod graphql;
mod logger;
mod model;
mod provider;
mod request;
mod response;
mod server;
mod store;

pub use config::{Config, load_dotenv};
pub use server::run;

static TLS_PROVIDER_INIT: Once = Once::new();

pub fn install_tls_provider() {
    TLS_PROVIDER_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
