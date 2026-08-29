use kube::Client;
use kube::config::{
    AuthInfo, Cluster, Context as KubeContext, KubeConfigOptions, Kubeconfig, NamedAuthInfo,
    NamedCluster, NamedContext,
};

/// Builds a `kube::Client` pointed at a `wiremock::MockServer`, for tests that need to exercise
/// real API-call error paths (status codes, response bodies) without a live cluster.
pub(crate) async fn build_mock_client(server_uri: &str) -> Client {
    let kubeconfig = Kubeconfig {
        clusters: vec![NamedCluster {
            name: "test".into(),
            cluster: Some(Cluster {
                server: Some(server_uri.to_string()),
                insecure_skip_tls_verify: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }],
        contexts: vec![NamedContext {
            name: "test".into(),
            context: Some(KubeContext {
                cluster: "test".into(),
                user: Some("test".into()),
                namespace: Some("test".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        auth_infos: vec![NamedAuthInfo {
            name: "test".into(),
            auth_info: Some(AuthInfo::default()),
            ..Default::default()
        }],
        current_context: Some("test".into()),
        ..Default::default()
    };

    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .unwrap();
    Client::try_from(config).unwrap()
}
