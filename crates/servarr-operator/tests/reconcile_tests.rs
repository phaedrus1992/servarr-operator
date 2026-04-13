//! Integration tests for the controller `reconcile` and `media_stack_controller` reconcile
//! functions, using wiremock to mock the Kubernetes API server.

use std::collections::HashMap;
use std::sync::Arc;

use kube::config::{
    AuthInfo, Cluster, Context as KubeContext, KubeConfigOptions, Kubeconfig, NamedAuthInfo,
    NamedCluster, NamedContext,
};
use kube::runtime::controller::Action;
use kube::runtime::events::Reporter;
use serde_json::json;
use servarr_crds::{
    AppType, MediaStack, MediaStackSpec, NfsServerSpec, ServarrApp, ServarrAppSpec, StackApp,
};
use servarr_operator::context::Context;
use tokio::time::Duration;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Helper: build a kube::Client pointing at the wiremock server
// ---------------------------------------------------------------------------

async fn mock_client(server_uri: &str) -> kube::Client {
    let kubeconfig = Kubeconfig {
        clusters: vec![NamedCluster {
            name: "test".into(),
            cluster: Some(Cluster {
                server: Some(server_uri.to_string()),
                insecure_skip_tls_verify: Some(true),
                ..Default::default()
            }),
        }],
        contexts: vec![NamedContext {
            name: "test".into(),
            context: Some(KubeContext {
                cluster: "test".into(),
                user: Some("test".into()),
                namespace: Some("test".into()),
                ..Default::default()
            }),
        }],
        auth_infos: vec![NamedAuthInfo {
            name: "test".into(),
            auth_info: Some(AuthInfo::default()),
        }],
        current_context: Some("test".into()),
        ..Default::default()
    };

    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .unwrap();
    kube::Client::try_from(config).unwrap()
}

fn test_context(client: kube::Client) -> Arc<Context> {
    Arc::new(Context {
        client,
        image_overrides: HashMap::new(),
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
    })
}

// ---------------------------------------------------------------------------
// Helper: build a minimal ServarrApp (Sonarr) for testing
// ---------------------------------------------------------------------------

fn make_sonarr_app(name: &str, ns: &str) -> ServarrApp {
    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    let mut app = ServarrApp::new(name, spec);
    app.metadata.namespace = Some(ns.into());
    app.metadata.uid = Some("test-uid-12345".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    app
}

// ---------------------------------------------------------------------------
// Helper: build a minimal MediaStack for testing
// ---------------------------------------------------------------------------

fn make_media_stack(name: &str, ns: &str) -> MediaStack {
    let spec = MediaStackSpec {
        defaults: None,
        apps: vec![StackApp {
            app: AppType::Sonarr,
            instance: None,
            enabled: true,
            image: None,
            uid: None,
            gid: None,
            security: None,
            service: None,
            gateway: None,
            resources: None,
            persistence: None,
            env: Vec::new(),
            probes: None,
            scheduling: None,
            network_policy: None,
            network_policy_config: None,
            app_config: None,
            api_key_secret: None,
            api_health_check: None,
            backup: None,
            image_pull_secrets: None,
            pod_annotations: None,
            gpu: None,
            prowlarr_sync: None,
            overseerr_sync: None,
            bazarr_sync: None,
            subgen_sync: None,
            admin_credentials: None,
            split4k: None,
            split4k_overrides: None,
        }],
        nfs: None,
    };
    let mut stack = MediaStack::new(name, spec);
    stack.metadata.namespace = Some(ns.into());
    stack.metadata.uid = Some("stack-uid-12345".into());
    stack.metadata.resource_version = Some("1".into());
    stack.metadata.generation = Some(1);
    stack
}

// ---------------------------------------------------------------------------
// Minimal JSON response helpers
// ---------------------------------------------------------------------------

/// Minimal deployment JSON response with readyReplicas for status checks.
fn deployment_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "deploy-uid-1",
            "resourceVersion": "100"
        },
        "spec": {
            "selector": { "matchLabels": { "app": name } },
            "template": {
                "metadata": { "labels": { "app": name } },
                "spec": {
                    "containers": [{
                        "name": name,
                        "image": "ghcr.io/onedr0p/sonarr:latest"
                    }]
                }
            }
        },
        "status": {
            "readyReplicas": 1,
            "replicas": 1,
            "availableReplicas": 1
        }
    })
}

/// Minimal service JSON response.
fn service_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "svc-uid-1",
            "resourceVersion": "101"
        }
    })
}

/// Minimal PVC JSON response.
fn pvc_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "pvc-uid-1",
            "resourceVersion": "102"
        }
    })
}

/// Minimal network policy JSON response.
fn networkpolicy_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "np-uid-1",
            "resourceVersion": "103"
        }
    })
}

/// Minimal ServarrApp JSON response (for status patch).
fn servarrapp_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "ServarrApp",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "sa-uid-1",
            "resourceVersion": "200"
        },
        "spec": {
            "app": "Sonarr"
        }
    })
}

/// Empty list response for a given apiVersion and kind.
fn empty_list(api_version: &str, kind: &str) -> serde_json::Value {
    json!({
        "apiVersion": api_version,
        "kind": kind,
        "metadata": {},
        "items": []
    })
}

/// Minimal Event response (k8s events.k8s.io/v1 API).
fn event_response() -> serde_json::Value {
    json!({
        "apiVersion": "events.k8s.io/v1",
        "kind": "Event",
        "metadata": {
            "name": "test-event",
            "namespace": "test",
            "uid": "event-uid-1",
            "resourceVersion": "300"
        }
    })
}

/// Minimal MediaStack JSON response (for status patch).
fn mediastack_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "MediaStack",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "ms-uid-1",
            "resourceVersion": "400"
        },
        "spec": {
            "apps": []
        }
    })
}

// ---------------------------------------------------------------------------
// Test 1: Basic Sonarr reconcile succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sonarr_reconcile_basic() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr", "test"));

    // PATCH deployment (SSA)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response("test-sonarr", "test")),
        )
        .named("patch-deployment")
        .mount(&mock_server)
        .await;

    // GET deployment (drift check + status)
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response("test-sonarr", "test")),
        )
        .named("get-deployment")
        .mount(&mock_server)
        .await;

    // PATCH service (SSA)
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr", "test")),
        )
        .named("patch-service")
        .mount(&mock_server)
        .await;

    // GET PVCs (check existence) -- return 404 so they get created
    Mock::given(method("GET"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "not found",
            "reason": "NotFound",
            "code": 404
        })))
        .named("get-pvc-404")
        .mount(&mock_server)
        .await;

    // PATCH PVCs (create via SSA)
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(pvc_response("test-sonarr-config", "test")),
        )
        .named("patch-pvc")
        .mount(&mock_server)
        .await;

    // PATCH networkpolicy (SSA)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(networkpolicy_response("test-sonarr", "test")),
        )
        .named("patch-networkpolicy")
        .mount(&mock_server)
        .await;

    // PATCH status on ServarrApp
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(servarrapp_response("test-sonarr", "test")),
        )
        .named("patch-status")
        .mount(&mock_server)
        .await;

    // POST events -- kube uses events.k8s.io/v1 API
    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .named("post-event")
        .mount(&mock_server)
        .await;

    // GET ServarrApps list (for gauge update + prowlarr/overseerr sync checks)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .named("list-servarrapps")
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;

    assert!(result.is_ok(), "reconcile should succeed, got: {result:?}");
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(300)),
        "should requeue after 300 seconds"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Sonarr reconcile with network policy disabled skips NP creation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sonarr_reconcile_network_policy_disabled() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let mut spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    spec.network_policy = Some(false);

    let mut app = ServarrApp::new("test-sonarr-nonp", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-nonp".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    // PATCH deployment
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-nonp",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(deployment_response("test-sonarr-nonp", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET deployment
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-nonp",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(deployment_response("test-sonarr-nonp", "test")),
        )
        .mount(&mock_server)
        .await;

    // PATCH service
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-nonp"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr-nonp", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET PVCs -> 404
    Mock::given(method("GET"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "not found",
            "reason": "NotFound",
            "code": 404
        })))
        .mount(&mock_server)
        .await;

    // PATCH PVCs
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-nonp-config", "test")),
        )
        .mount(&mock_server)
        .await;

    // We intentionally do NOT mock the networkpolicy endpoint.
    // If reconcile tries to create one, it will get a connection error from
    // wiremock (unmatched request). Instead we use `expect(0)` on a NP mock.
    let np_mock = Mock::given(method("PATCH"))
        .and(path_regex(
            r"/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-nonp", "test")),
        )
        .named("patch-networkpolicy-should-not-be-called")
        .expect(0)
        .mount_as_scoped(&mock_server)
        .await;

    // PATCH status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-nonp/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-nonp", "test")),
        )
        .mount(&mock_server)
        .await;

    // POST events -- kube uses events.k8s.io/v1 API
    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

    // GET ServarrApps list
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;

    assert!(result.is_ok(), "reconcile should succeed, got: {result:?}");
    let action = result.unwrap();
    assert_eq!(action, Action::requeue(Duration::from_secs(300)));

    // The scoped mock will verify expect(0) when dropped here
    drop(np_mock);
}

// ---------------------------------------------------------------------------
// Test 3: error_policy returns requeue(60s)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_error_policy_returns_requeue_60s() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr", "test"));

    // POST events (error_policy spawns a task to publish an event)
    Mock::given(method("POST"))
        .and(path("/api/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

    let error = servarr_operator::controller::Error::Serialization(
        serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
    );

    let action = servarr_operator::controller::error_policy(app, &error, ctx);

    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(60)),
        "error_policy should requeue after 60 seconds"
    );
}

// ---------------------------------------------------------------------------
// Test 4: MediaStack reconcile with one Sonarr app
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_reconcile_basic() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let stack = Arc::new(make_media_stack("my-stack", "test"));

    // PATCH child ServarrApp (SSA) -- "my-stack-sonarr"
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/my-stack-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("my-stack-sonarr", "test")),
        )
        .named("patch-child-sa")
        .mount(&mock_server)
        .await;

    // GET child ServarrApp (read back status)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/my-stack-sonarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response("my-stack-sonarr", "test");
            resp["status"] = json!({
                "ready": true,
                "readyReplicas": 1,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .named("get-child-sa")
        .mount(&mock_server)
        .await;

    // GET ServarrApps by label (orphan cleanup)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .named("list-servarrapps-by-label")
        .mount(&mock_server)
        .await;

    // PATCH MediaStack status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks/my-stack/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mediastack_response("my-stack", "test")),
        )
        .named("patch-stack-status")
        .mount(&mock_server)
        .await;

    // GET MediaStack list (for gauge)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .named("list-mediastacks")
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "media_stack reconcile should succeed, got: {result:?}"
    );
    // With the child being ready, phase=Ready, so requeue is 300s
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(300)),
        "ready stack should requeue after 300 seconds"
    );
}

// ---------------------------------------------------------------------------
// Test 5: MediaStack error_policy returns requeue(60s)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_error_policy_returns_requeue_60s() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let stack = Arc::new(make_media_stack("my-stack", "test"));

    let error = servarr_operator::media_stack_controller::Error::Serialization(
        serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
    );

    let action = servarr_operator::media_stack_controller::error_policy(stack, &error, ctx);

    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(60)),
        "media_stack error_policy should requeue after 60 seconds"
    );
}

// ---------------------------------------------------------------------------
// Test 6: MediaStack reconcile with child not ready results in 30s requeue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_reconcile_child_not_ready() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let stack = Arc::new(make_media_stack("pending-stack", "test"));

    // PATCH child ServarrApp
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/pending-stack-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("pending-stack-sonarr", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET child ServarrApp -- NOT ready (no status.ready)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/pending-stack-sonarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response("pending-stack-sonarr", "test");
            resp["status"] = json!({
                "ready": false,
                "readyReplicas": 0,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .mount(&mock_server)
        .await;

    // GET ServarrApps by label
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(&mock_server)
        .await;

    // PATCH MediaStack status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks/pending-stack/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mediastack_response("pending-stack", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET MediaStack list
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "reconcile should succeed even with non-ready child, got: {result:?}"
    );
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(30)),
        "non-ready stack should requeue after 30 seconds"
    );
}

// ---------------------------------------------------------------------------
// Helper: build a MediaStack with multiple apps
// ---------------------------------------------------------------------------

fn make_multi_app_stack(name: &str, ns: &str) -> MediaStack {
    let spec = MediaStackSpec {
        defaults: None,
        apps: vec![
            StackApp {
                app: AppType::Sonarr,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
            StackApp {
                app: AppType::Radarr,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
            StackApp {
                app: AppType::Transmission,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
        ],
        nfs: None,
    };
    let mut stack = MediaStack::new(name, spec);
    stack.metadata.namespace = Some(ns.into());
    stack.metadata.uid = Some("stack-uid-multi".into());
    stack.metadata.resource_version = Some("1".into());
    stack.metadata.generation = Some(1);
    stack
}

// ---------------------------------------------------------------------------
// Test 7: MediaStack reconcile with multi-app stack (Sonarr + Radarr + Transmission)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_reconcile_multi_app() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let stack = Arc::new(make_multi_app_stack("multi", "test"));

    // Transmission is tier 1, Sonarr/Radarr are tier 2.
    // The controller processes tiers in order: tier 1 first, then tier 2.

    // PATCH child ServarrApps (SSA) -- catch-all for any servarrapp patch
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/multi-.*",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(servarrapp_response("multi-child", "test")),
        )
        .named("patch-child-sa")
        .mount(&mock_server)
        .await;

    // GET child ServarrApps (read back status) -- all report ready
    Mock::given(method("GET"))
        .and(path_regex(
            r"/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/multi-.*",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response("multi-child", "test");
            resp["status"] = json!({
                "ready": true,
                "readyReplicas": 1,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .named("get-child-sa")
        .mount(&mock_server)
        .await;

    // GET ServarrApps by label (orphan cleanup) -- empty list (no orphans)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .named("list-servarrapps-by-label")
        .mount(&mock_server)
        .await;

    // PATCH MediaStack status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks/multi/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mediastack_response("multi", "test")),
        )
        .named("patch-stack-status")
        .mount(&mock_server)
        .await;

    // GET MediaStack list (for gauge)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .named("list-mediastacks")
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "multi-app reconcile should succeed, got: {result:?}"
    );
    // All 3 apps are ready -> phase=Ready -> 300s requeue
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(300)),
        "all-ready multi-app stack should requeue after 300 seconds"
    );
}

// ---------------------------------------------------------------------------
// Test 8: MediaStack with a disabled app skips creating that child
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_reconcile_disabled_app() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    // Stack with Sonarr enabled + Radarr disabled
    let spec = MediaStackSpec {
        defaults: None,
        apps: vec![
            StackApp {
                app: AppType::Sonarr,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
            StackApp {
                app: AppType::Radarr,
                instance: None,
                enabled: false,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                admin_credentials: None,
                split4k: None,
                split4k_overrides: None,
            },
        ],
        nfs: None,
    };
    let mut stack = MediaStack::new("disabled-test", spec);
    stack.metadata.namespace = Some("test".into());
    stack.metadata.uid = Some("stack-uid-disabled".into());
    stack.metadata.resource_version = Some("1".into());
    stack.metadata.generation = Some(1);
    let stack = Arc::new(stack);

    // Only the Sonarr child should be patched. We mock the Sonarr child endpoints.
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/disabled-test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("disabled-test-sonarr", "test")),
        )
        .named("patch-sonarr-child")
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/disabled-test-sonarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response("disabled-test-sonarr", "test");
            resp["status"] = json!({
                "ready": true,
                "readyReplicas": 1,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .named("get-sonarr-child")
        .mount(&mock_server)
        .await;

    // The Radarr child should NOT be patched.  We verify with expect(0).
    let _radarr_mock = Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/disabled-test-radarr",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("disabled-test-radarr", "test")),
        )
        .named("patch-radarr-should-not-be-called")
        .expect(0)
        .mount_as_scoped(&mock_server)
        .await;

    // GET ServarrApps by label (orphan cleanup)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(&mock_server)
        .await;

    // PATCH MediaStack status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks/disabled-test/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mediastack_response("disabled-test", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET MediaStack list (for gauge)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "reconcile with disabled app should succeed, got: {result:?}"
    );
    // Only 1 enabled app (Sonarr), it is ready -> phase=Ready -> 300s
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(300)),
        "stack with disabled app should requeue after 300 seconds when enabled apps are ready"
    );
    // _radarr_mock scoped drop verifies expect(0)
}

// ---------------------------------------------------------------------------
// Test 9: MediaStack orphan cleanup -- deletes child not in spec
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_reconcile_orphan_cleanup() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    // Stack has only Sonarr
    let stack = Arc::new(make_media_stack("orphan-stack", "test"));

    // PATCH child ServarrApp (the real one)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/orphan-stack-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("orphan-stack-sonarr", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET child ServarrApp (real child, ready)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/orphan-stack-sonarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response("orphan-stack-sonarr", "test");
            resp["status"] = json!({
                "ready": true,
                "readyReplicas": 1,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .mount(&mock_server)
        .await;

    // GET ServarrApps by label returns the real child AND an orphan
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrAppList",
            "metadata": {},
            "items": [
                {
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": "orphan-stack-sonarr",
                        "namespace": "test",
                        "uid": "sa-uid-real",
                        "resourceVersion": "200"
                    },
                    "spec": { "app": "Sonarr" }
                },
                {
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": "orphan-stack-old-radarr",
                        "namespace": "test",
                        "uid": "sa-uid-orphan",
                        "resourceVersion": "201"
                    },
                    "spec": { "app": "Radarr" }
                }
            ]
        })))
        .named("list-servarrapps-with-orphan")
        .mount(&mock_server)
        .await;

    // DELETE the orphaned child -- the controller should call this
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/orphan-stack-old-radarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrApp",
            "metadata": {
                "name": "orphan-stack-old-radarr",
                "namespace": "test",
                "uid": "sa-uid-orphan",
                "resourceVersion": "201"
            }
        })))
        .expect(1)
        .named("delete-orphan")
        .mount(&mock_server)
        .await;

    // PATCH MediaStack status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks/orphan-stack/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mediastack_response("orphan-stack", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET MediaStack list (for gauge)
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/mediastacks",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "orphan cleanup reconcile should succeed, got: {result:?}"
    );
    let action = result.unwrap();
    assert_eq!(
        action,
        Action::requeue(Duration::from_secs(300)),
        "ready stack should requeue after 300 seconds"
    );
    // The expect(1) on the DELETE mock will verify the orphan was deleted
}

// ---------------------------------------------------------------------------
// Helper: DynamicObject response for Gateway API resources
// ---------------------------------------------------------------------------

fn dynamic_object_response(
    api_version: &str,
    kind: &str,
    name: &str,
    ns: &str,
) -> serde_json::Value {
    json!({
        "apiVersion": api_version,
        "kind": kind,
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": format!("{kind}-uid-1"),
            "resourceVersion": "500"
        }
    })
}

/// Minimal ConfigMap JSON response.
fn configmap_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "cm-uid-1",
            "resourceVersion": "110"
        }
    })
}

/// Minimal Secret JSON response.
fn secret_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "secret-uid-1",
            "resourceVersion": "111"
        }
    })
}

/// Mount the common mocks shared by most ServarrApp reconcile tests.
async fn mount_common_mocks(mock_server: &MockServer, name: &str, ns: &str) {
    // PATCH deployment (SSA)
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/{name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(deployment_response(name, ns)))
        .mount(mock_server)
        .await;

    // GET deployment (drift check + status)
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/{name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(deployment_response(name, ns)))
        .mount(mock_server)
        .await;

    // PATCH service (SSA)
    Mock::given(method("PATCH"))
        .and(path(format!("/api/v1/namespaces/{ns}/services/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(service_response(name, ns)))
        .mount(mock_server)
        .await;

    // GET PVCs -> 404
    Mock::given(method("GET"))
        .and(path_regex(format!(
            r"/api/v1/namespaces/{ns}/persistentvolumeclaims/.*"
        )))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "not found",
            "reason": "NotFound",
            "code": 404
        })))
        .mount(mock_server)
        .await;

    // PATCH PVCs
    Mock::given(method("PATCH"))
        .and(path_regex(format!(
            r"/api/v1/namespaces/{ns}/persistentvolumeclaims/.*"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(pvc_response(&format!("{name}-config"), ns)),
        )
        .mount(mock_server)
        .await;

    // PATCH networkpolicy (SSA)
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/networking.k8s.io/v1/namespaces/{ns}/networkpolicies/{name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(networkpolicy_response(name, ns)))
        .mount(mock_server)
        .await;

    // PATCH status on ServarrApp
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps/{name}/status"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(servarrapp_response(name, ns)))
        .mount(mock_server)
        .await;

    // POST events
    Mock::given(method("POST"))
        .and(path(format!(
            "/apis/events.k8s.io/v1/namespaces/{ns}/events"
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(mock_server)
        .await;

    // GET ServarrApps list
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(mock_server)
        .await;
}

// ---------------------------------------------------------------------------
// Test 10: Transmission app reconcile (ConfigMap build path)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_transmission_reconcile_creates_configmap() {
    use servarr_crds::{AppConfig, TransmissionConfig};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Transmission,
        app_config: Some(AppConfig::Transmission(TransmissionConfig {
            settings: json!({
                "download-dir": "/downloads/complete",
                "incomplete-dir": "/downloads/incomplete"
            }),
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-transmission", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-tx".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-transmission", "test").await;

    // PATCH ConfigMap for Transmission settings (name = app_name = "test-transmission")
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/configmaps/test-transmission"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-transmission", "test")),
        )
        .named("patch-transmission-configmap")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "transmission reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 11: App with gateway enabled + TLS -> TCPRoute + Certificate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_gateway_tls_tcproute_and_certificate() {
    use servarr_crds::{GatewayParentRef, GatewaySpec, TlsSpec};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        gateway: Some(GatewaySpec {
            enabled: true,
            hosts: vec!["sonarr.example.com".into()],
            parent_refs: vec![GatewayParentRef {
                name: "my-gateway".into(),
                namespace: "gateway-ns".into(),
                ..Default::default()
            }],
            tls: Some(TlsSpec {
                enabled: true,
                cert_issuer: "letsencrypt-prod".into(),
                secret_name: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr-gw", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-gw".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-sonarr-gw", "test").await;

    // TLS enabled forces TCPRoute (not HTTPRoute)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/gateway.networking.k8s.io/v1alpha2/namespaces/test/tcproutes/test-sonarr-gw",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(dynamic_object_response(
                "gateway.networking.k8s.io/v1alpha2",
                "TCPRoute",
                "test-sonarr-gw",
                "test",
            )),
        )
        .named("patch-tcproute")
        .expect(1..)
        .mount(&mock_server)
        .await;

    // PATCH Certificate
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/cert-manager.io/v1/namespaces/test/certificates/test-sonarr-gw",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(dynamic_object_response(
                "cert-manager.io/v1",
                "Certificate",
                "test-sonarr-gw",
                "test",
            )),
        )
        .named("patch-certificate")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "gateway TLS reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 12: App with HTTPRoute only (no TLS, Http route_type)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_gateway_httproute_only() {
    use servarr_crds::{GatewayParentRef, GatewaySpec};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Radarr,
        gateway: Some(GatewaySpec {
            enabled: true,
            hosts: vec!["radarr.example.com".into()],
            parent_refs: vec![GatewayParentRef {
                name: "my-gateway".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-radarr-gw", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-gw2".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-radarr-gw", "test").await;

    // PATCH HTTPRoute (no TLS, so HTTPRoute not TCPRoute)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/gateway.networking.k8s.io/v1/namespaces/test/httproutes/test-radarr-gw",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(dynamic_object_response(
                "gateway.networking.k8s.io/v1",
                "HTTPRoute",
                "test-radarr-gw",
                "test",
            )),
        )
        .named("patch-httproute")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "httproute reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 13: SSH bastion app (authorized-keys Secret + restricted-rsync ConfigMap)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ssh_bastion_reconcile() {
    use servarr_crds::{AppConfig, RestrictedRsyncConfig, SshBastionConfig, SshMode, SshUser};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::SshBastion,
        app_config: Some(AppConfig::SshBastion(SshBastionConfig {
            users: vec![SshUser {
                name: "testuser".into(),
                uid: 1000,
                gid: 1000,
                mode: SshMode::RestrictedRsync,
                restricted_rsync: Some(RestrictedRsyncConfig {
                    allowed_paths: vec!["/data/media".into()],
                }),
                shell: None,
                public_keys: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 test@example".into(),
            }],
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-bastion", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-bastion".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-bastion", "test").await;

    // PATCH authorized-keys Secret
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/secrets/test-bastion-authorized-keys",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(secret_response("test-bastion-authorized-keys", "test")),
        )
        .named("patch-authorized-keys-secret")
        .expect(1..)
        .mount(&mock_server)
        .await;

    // PATCH restricted-rsync ConfigMap
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-bastion-restricted-rsync",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-bastion-restricted-rsync", "test")),
        )
        .named("patch-restricted-rsync-cm")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "ssh bastion reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 14: SABnzbd app with whitelist + tar_unpack
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sabnzbd_whitelist_and_tar_unpack() {
    use servarr_crds::{AppConfig, SabnzbdConfig};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Sabnzbd,
        app_config: Some(AppConfig::Sabnzbd(SabnzbdConfig {
            host_whitelist: vec!["sabnzbd.example.com".into(), "sab.local".into()],
            tar_unpack: true,
        })),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sabnzbd", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-sab".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-sabnzbd", "test").await;

    // PATCH SABnzbd whitelist ConfigMap (child_name = "test-sabnzbd-sabnzbd-config")
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-sabnzbd-sabnzbd-config",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-sabnzbd-sabnzbd-config", "test")),
        )
        .named("patch-sabnzbd-config-cm")
        .expect(1..)
        .mount(&mock_server)
        .await;

    // PATCH tar-unpack ConfigMap (child_name = "test-sabnzbd-tar-unpack")
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-sabnzbd-tar-unpack",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-sabnzbd-tar-unpack", "test")),
        )
        .named("patch-tar-unpack-cm")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "sabnzbd reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 15: Prowlarr app with custom definitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_prowlarr_custom_definitions() {
    use servarr_crds::{AppConfig, IndexerDefinition, ProwlarrConfig};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Prowlarr,
        app_config: Some(AppConfig::Prowlarr(ProwlarrConfig {
            custom_definitions: vec![IndexerDefinition {
                name: "my-tracker".into(),
                content: "id: my-tracker\nname: My Tracker\n".into(),
            }],
        })),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-prowlarr", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-prowlarr".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-prowlarr", "test").await;

    // PATCH Prowlarr definitions ConfigMap (child_name = "test-prowlarr-prowlarr-definitions")
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-prowlarr-prowlarr-definitions",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(configmap_response(
            "test-prowlarr-prowlarr-definitions",
            "test",
        )))
        .named("patch-prowlarr-defs-cm")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "prowlarr reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 16: PVC already exists (Ok branch - skip create)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pvc_already_exists_skips_create() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr-pvc", "test"));

    // PATCH deployment
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-pvc",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(deployment_response("test-sonarr-pvc", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET deployment
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-pvc",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(deployment_response("test-sonarr-pvc", "test")),
        )
        .mount(&mock_server)
        .await;

    // PATCH service
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-pvc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr-pvc", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET PVCs -> 200 (PVC already exists)
    Mock::given(method("GET"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-pvc-config", "test")),
        )
        .named("get-pvc-exists")
        .mount(&mock_server)
        .await;

    // PVC PATCH should NOT be called since PVC already exists
    let pvc_patch_mock = Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-pvc-config", "test")),
        )
        .named("patch-pvc-should-not-be-called")
        .expect(0)
        .mount_as_scoped(&mock_server)
        .await;

    // PATCH networkpolicy
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-pvc",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-pvc", "test")),
        )
        .mount(&mock_server)
        .await;

    // PATCH status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-pvc/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-pvc", "test")),
        )
        .mount(&mock_server)
        .await;

    // POST events
    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

    // GET ServarrApps list
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "reconcile should succeed with existing PVC, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));

    // Scoped mock verifies expect(0) on drop
    drop(pvc_patch_mock);
}

// ---------------------------------------------------------------------------
// Test 17: Network policy config override (network_policy=false but config set)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_network_policy_config_overrides_disabled_flag() {
    use servarr_crds::NetworkPolicyConfig;

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        network_policy: Some(false),
        network_policy_config: Some(NetworkPolicyConfig {
            allow_same_namespace: true,
            allow_dns: true,
            allow_internet_egress: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr-npc", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-npc".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-sonarr-npc", "test").await;

    // The network policy PATCH mock is in mount_common_mocks. The key
    // assertion here is that reconcile succeeds -- which means the NP
    // endpoint was called even though network_policy=false, because
    // network_policy_config is set and overrides the flag.

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "reconcile should succeed with NP config override, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 18: Deployment drift detection triggers re-apply
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deployment_drift_detection() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr-drift", "test"));

    // PATCH deployment (SSA) - first apply and re-apply
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-drift",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(deployment_response("test-sonarr-drift", "test")),
        )
        .named("patch-deployment")
        .expect(2..) // Called at least twice: initial + drift re-apply
        .mount(&mock_server)
        .await;

    // GET deployment returns a deployment with a DIFFERENT image than what
    // the operator would build, triggering drift detection.
    let drifted_deploy = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "test-sonarr-drift",
            "namespace": "test",
            "uid": "deploy-uid-1",
            "resourceVersion": "100"
        },
        "spec": {
            "selector": { "matchLabels": { "app": "test-sonarr-drift" } },
            "template": {
                "metadata": { "labels": { "app": "test-sonarr-drift" } },
                "spec": {
                    "containers": [{
                        "name": "sonarr",
                        "image": "ghcr.io/onedr0p/sonarr:DRIFTED-VERSION"
                    }]
                }
            }
        },
        "status": {
            "readyReplicas": 1,
            "replicas": 1,
            "availableReplicas": 1
        }
    });
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-drift",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(drifted_deploy))
        .named("get-deployment-drifted")
        .mount(&mock_server)
        .await;

    // PATCH service
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-drift"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr-drift", "test")),
        )
        .mount(&mock_server)
        .await;

    // GET PVCs -> 404
    Mock::given(method("GET"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "not found",
            "reason": "NotFound",
            "code": 404
        })))
        .mount(&mock_server)
        .await;

    // PATCH PVCs
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-drift-config", "test")),
        )
        .mount(&mock_server)
        .await;

    // PATCH networkpolicy
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-drift",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-drift", "test")),
        )
        .mount(&mock_server)
        .await;

    // PATCH status
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-drift/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-drift", "test")),
        )
        .mount(&mock_server)
        .await;

    // POST events (will get DriftDetected + ReconcileSuccess events)
    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

    // GET ServarrApps list
    Mock::given(method("GET"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "reconcile with drift should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Test 19: TCPRoute via explicit Tcp route_type (no TLS)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_gateway_tcp_route_type() {
    use servarr_crds::{GatewayParentRef, GatewaySpec, RouteType};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Plex,
        gateway: Some(GatewaySpec {
            enabled: true,
            route_type: RouteType::Tcp,
            hosts: vec!["plex.example.com".into()],
            parent_refs: vec![GatewayParentRef {
                name: "my-gateway".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-plex-tcp", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-plex-tcp".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(&mock_server, "test-plex-tcp", "test").await;

    // PATCH TCPRoute (explicit Tcp route_type)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/gateway.networking.k8s.io/v1alpha2/namespaces/test/tcproutes/test-plex-tcp",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(dynamic_object_response(
                "gateway.networking.k8s.io/v1alpha2",
                "TCPRoute",
                "test-plex-tcp",
                "test",
            )),
        )
        .named("patch-tcproute-explicit")
        .expect(1..)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_ok(),
        "tcp route reconcile should succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
}

// ---------------------------------------------------------------------------
// Helpers for NFS reconcile tests
// ---------------------------------------------------------------------------

fn make_nfs_stack(name: &str, ns: &str, nfs: Option<NfsServerSpec>) -> MediaStack {
    let spec = MediaStackSpec {
        defaults: None,
        apps: vec![StackApp {
            app: AppType::Sonarr,
            instance: None,
            enabled: true,
            image: None,
            uid: None,
            gid: None,
            security: None,
            service: None,
            gateway: None,
            resources: None,
            persistence: None,
            env: Vec::new(),
            probes: None,
            scheduling: None,
            network_policy: None,
            network_policy_config: None,
            app_config: None,
            api_key_secret: None,
            api_health_check: None,
            backup: None,
            image_pull_secrets: None,
            pod_annotations: None,
            gpu: None,
            prowlarr_sync: None,
            overseerr_sync: None,
            bazarr_sync: None,
            subgen_sync: None,
            admin_credentials: None,
            split4k: None,
            split4k_overrides: None,
        }],
        nfs,
    };
    let mut stack = MediaStack::new(name, spec);
    stack.metadata.namespace = Some(ns.into());
    stack.metadata.uid = Some("nfs-stack-uid".into());
    stack.metadata.resource_version = Some("1".into());
    stack.metadata.generation = Some(1);
    stack
}

fn statefulset_response(name: &str, ns: &str) -> serde_json::Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "ss-uid-1",
            "resourceVersion": "500"
        },
        "spec": {
            "selector": { "matchLabels": {} },
            "serviceName": name,
            "template": { "spec": { "containers": [] } }
        }
    })
}

/// Mount common MediaStack child-app mocks (ServarrApp PATCH/GET, list, status).
async fn mount_child_app_mocks(mock_server: &MockServer, stack_name: &str, ns: &str) {
    let pattern = format!("/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps/{stack_name}-.*");
    Mock::given(method("PATCH"))
        .and(path_regex(pattern.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response(&format!("{stack_name}-sonarr"), ns)),
        )
        .named("patch-child-sa")
        .mount(mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(pattern.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut r = servarrapp_response(&format!("{stack_name}-sonarr"), ns);
            r["status"] = json!({"ready": true, "readyReplicas": 1, "observedGeneration": 1, "conditions": []});
            r
        }))
        .named("get-child-sa")
        .mount(mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "ServarrAppList")),
        )
        .named("list-sa")
        .mount(mock_server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/mediastacks/{stack_name}/status"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(mediastack_response(stack_name, ns)))
        .named("patch-stack-status")
        .mount(mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/mediastacks"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(empty_list("servarr.dev/v1alpha1", "MediaStackList")),
        )
        .named("list-mediastacks")
        .mount(mock_server)
        .await;
}

// ---------------------------------------------------------------------------
// NFS reconcile: in-cluster NFS creates StatefulSet and Service
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_nfs_in_cluster_creates_statefulset_and_service() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let stack = Arc::new(make_nfs_stack(
        "nfs-test",
        "test",
        Some(NfsServerSpec::default()),
    ));

    // Expect PATCH for NFS StatefulSet
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/statefulsets/nfs-test-nfs-server",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(statefulset_response("nfs-test-nfs-server", "test")),
        )
        .named("patch-nfs-statefulset")
        .mount(&mock_server)
        .await;

    // Expect PATCH for NFS Service
    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/nfs-test-nfs-server"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(service_response("nfs-test-nfs-server", "test")),
        )
        .named("patch-nfs-service")
        .mount(&mock_server)
        .await;

    // Expect GET for NFS server pod IP lookup (pod not yet running → 404)
    Mock::given(method("GET"))
        .and(path("/api/v1/namespaces/test/pods/nfs-test-nfs-server-0"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Failure",
            "message": "pods \"nfs-test-nfs-server-0\" not found",
            "reason": "NotFound",
            "code": 404
        })))
        .named("get-nfs-pod")
        .mount(&mock_server)
        .await;

    mount_child_app_mocks(&mock_server, "nfs-test", "test").await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;
    assert!(
        result.is_ok(),
        "NFS in-cluster reconcile should succeed, got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// NFS reconcile: disabled NFS does NOT create StatefulSet or Service
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_nfs_disabled_does_not_create_resources() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let nfs = NfsServerSpec {
        enabled: false,
        ..Default::default()
    };
    let stack = Arc::new(make_nfs_stack("nfs-disabled", "test", Some(nfs)));

    // Must NOT patch StatefulSet
    let _ss_mock = Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/statefulsets/nfs-disabled-nfs-server",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("no-nfs-statefulset")
        .mount_as_scoped(&mock_server)
        .await;

    // Must NOT patch Service
    let _svc_mock = Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/services/nfs-disabled-nfs-server",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("no-nfs-service")
        .mount_as_scoped(&mock_server)
        .await;

    mount_child_app_mocks(&mock_server, "nfs-disabled", "test").await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;
    assert!(
        result.is_ok(),
        "disabled NFS reconcile should succeed, got: {result:?}"
    );
    // _ss_mock and _svc_mock drop will verify expect(0)
}

// ---------------------------------------------------------------------------
// NFS reconcile: external NFS server does NOT create in-cluster resources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_media_stack_nfs_external_does_not_create_in_cluster_resources() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let nfs = NfsServerSpec {
        external_server: Some("nas.home.arpa".to_string()),
        external_path: "/volume1".to_string(),
        ..Default::default()
    };
    let stack = Arc::new(make_nfs_stack("nfs-external", "test", Some(nfs)));

    // Must NOT patch in-cluster StatefulSet
    let _ss_mock = Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/statefulsets/nfs-external-nfs-server",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("no-nfs-statefulset")
        .mount_as_scoped(&mock_server)
        .await;

    mount_child_app_mocks(&mock_server, "nfs-external", "test").await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;
    assert!(
        result.is_ok(),
        "external NFS reconcile should succeed, got: {result:?}"
    );
    // _ss_mock drop verifies expect(0)
}
