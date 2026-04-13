/// Verify that the rustls CryptoProvider is available at runtime.
///
/// kube 3.0+ and reqwest 0.13+ require an explicit crypto provider
/// (ring or aws-lc-rs) to be compiled in. Without one the process
/// panics on the first TLS handshake. This test catches missing
/// feature flags at `cargo test` time rather than in production.
#[tokio::test]
async fn kube_client_config_initialises_tls() {
    // Building a kube client from a Config exercises the rustls TLS
    // stack. This will panic if no CryptoProvider is available, which
    // is exactly the failure mode we want to catch early.
    let config = kube::Config {
        cluster_url: "https://localhost:6443".parse().unwrap(),
        default_namespace: "default".into(),
        root_cert: None,
        connect_timeout: None,
        read_timeout: None,
        write_timeout: None,
        accept_invalid_certs: true,
        auth_info: Default::default(),
        proxy_url: None,
        tls_server_name: None,
        disable_compression: false,
        headers: vec![],
    };

    // Client::try_from exercises the full TLS client construction path.
    // It will fail (no real cluster) but must not *panic*.
    let result = kube::Client::try_from(config);
    assert!(
        result.is_ok(),
        "kube client construction panicked or failed to initialise TLS"
    );
}
