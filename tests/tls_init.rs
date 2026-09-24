//! Runs in its own process, so nothing else has installed a TLS provider first.

#[test]
fn fx_cache_constructs_in_a_fresh_process() {
    let _ = atrader::fx::FxCache::new();
}
