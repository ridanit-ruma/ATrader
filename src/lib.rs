pub mod domain;
pub mod feed;
pub mod fx;
pub mod venue;
pub mod sim;
pub mod ledger;
pub mod broker;
pub mod store;
pub mod subs;
pub mod stats;

/// Install `ring` as rustls' process-wide crypto provider (reqwest is built without one).
/// Safe to call repeatedly.
pub fn init_tls() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
