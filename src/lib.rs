pub mod domain;
pub mod feed;
pub mod fx;
pub mod venue;
pub mod sim;
pub mod ledger;
pub mod market;
pub mod persist;
pub mod app;
pub mod broker;
pub mod cli;
pub mod store;
pub mod subs;
pub mod tools;
pub mod stats;

/// Install `ring` as rustls' process-wide crypto provider (reqwest is built without one).
/// Safe to call repeatedly.
pub fn init_tls() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// `NAME`, or else the contents of the file named by `NAME_FILE` (how systemd credentials arrive).
/// Blank counts as unset; an unreadable file is logged and counts as unset.
pub fn secret_env(name: &str) -> Option<String> {
    let direct = std::env::var(name).ok();
    let value = match direct.filter(|v| !v.trim().is_empty()) {
        Some(v) => v,
        None => {
            let path = std::env::var(format!("{name}_FILE")).ok()?;
            match std::fs::read_to_string(&path) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, path, "cannot read {name}_FILE");
                    return None;
                }
            }
        }
    };
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

pub mod candles;
pub mod screen;
pub mod indicators;
pub mod performance;
pub mod fundamentals;
pub mod alerts;
pub mod web;

#[cfg(test)]
mod tests {
    #[test]
    fn secrets_come_from_the_variable_or_its_file() {
        let dir = std::env::temp_dir().join(format!("atrader-secret-{}", std::process::id()));
        std::fs::write(&dir, "from-file\n").unwrap();
        // SAFETY: this test is the only user of these variable names.
        unsafe {
            std::env::set_var("ATRADER_TEST_SECRET_FILE", &dir);
            std::env::set_var("ATRADER_TEST_SECRET", " ");
        }
        assert_eq!(super::secret_env("ATRADER_TEST_SECRET").as_deref(), Some("from-file"));
        unsafe { std::env::set_var("ATRADER_TEST_SECRET", "direct") };
        assert_eq!(super::secret_env("ATRADER_TEST_SECRET").as_deref(), Some("direct"));
        assert_eq!(super::secret_env("ATRADER_TEST_MISSING"), None);
        std::fs::remove_file(dir).unwrap();
    }
}
