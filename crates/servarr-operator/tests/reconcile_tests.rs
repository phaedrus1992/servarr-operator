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
    AppDefaults, AppType, MediaStack, MediaStackSpec, NfsServerSpec, ServarrApp, ServarrAppSpec,
    StackApp,
};
use servarr_operator::context::Context;
use tokio::time::Duration;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Matches a request whose body contains `needle` -- used to fail only one specific Event
/// publish (by its `reason`) while leaving other POSTs to the same path/method unaffected.
struct BodyContains(&'static str);

impl wiremock::Match for BodyContains {
    fn matches(&self, request: &Request) -> bool {
        String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

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
    kube::Client::try_from(config).unwrap()
}

fn test_context(client: kube::Client) -> Arc<Context> {
    Arc::new(Context {
        client,
        image_overrides: HashMap::new(),
        legacy_image_override_apps: std::collections::HashSet::new(),
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
        app_api_base_override: None,
        event_publish_tasks: tokio_util::task::TaskTracker::new(),
    })
}

/// A context whose "seerr" image override is flagged as coming from the deprecated
/// `DEFAULT_IMAGE_OVERSEERR_*` fallback -- used to test that reconcile publishes a
/// Warning Event when that fallback is in effect (#534).
fn test_context_with_legacy_seerr_override(client: kube::Client) -> Arc<Context> {
    let mut image_overrides = HashMap::new();
    image_overrides.insert(
        "seerr".to_string(),
        servarr_crds::ImageSpec {
            repository: "linuxserver/overseerr".into(),
            tag: "1.35.0".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );
    let mut legacy_image_override_apps = std::collections::HashSet::new();
    legacy_image_override_apps.insert("seerr".to_string());

    Arc::new(Context {
        client,
        image_overrides,
        legacy_image_override_apps,
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
        app_api_base_override: None,
        event_publish_tasks: tokio_util::task::TaskTracker::new(),
    })
}

/// A context whose "sonarr" image override is present but differs from this operator
/// binary's compiled-in default -- simulating a `helm upgrade --reuse-values` that froze a
/// stale `defaultImages.sonarr` value from an older chart (#638).
fn test_context_with_stale_sonarr_override(client: kube::Client) -> Arc<Context> {
    let mut image_overrides = HashMap::new();
    image_overrides.insert(
        "sonarr".to_string(),
        servarr_crds::ImageSpec {
            repository: "linuxserver/sonarr".into(),
            tag: "3.0.0".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );
    Arc::new(Context {
        client,
        image_overrides,
        legacy_image_override_apps: std::collections::HashSet::new(),
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
        app_api_base_override: None,
        event_publish_tasks: tokio_util::task::TaskTracker::new(),
    })
}

/// A context whose "sonarr" image override exactly matches this operator binary's
/// compiled-in default -- must not trigger the staleness warning (#638). Reads the default
/// from `AppDefaults` rather than hardcoding a version so this test doesn't drift out of sync
/// with `image-defaults.toml`.
fn test_context_with_current_sonarr_override(client: kube::Client) -> Arc<Context> {
    let builtin =
        AppDefaults::try_for_app(&AppType::Sonarr).expect("sonarr must have image defaults");
    let mut image_overrides = HashMap::new();
    image_overrides.insert(
        "sonarr".to_string(),
        servarr_crds::ImageSpec {
            repository: builtin.image.repository,
            tag: builtin.image.tag,
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );
    Arc::new(Context {
        client,
        image_overrides,
        legacy_image_override_apps: std::collections::HashSet::new(),
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
        app_api_base_override: None,
        event_publish_tasks: tokio_util::task::TaskTracker::new(),
    })
}

/// A context whose "sonarr" image override sets only `repository` (as `DEFAULT_IMAGE_SONARR_REPO`
/// with no matching `_TAG` renders) and that repository matches this operator binary's builtin --
/// the actually-deployed image (after `ImageSpec::merge_with` fills the empty tag from the
/// builtin) is identical to the builtin, so this must not trigger the staleness warning (#638).
/// Comparing the *raw* override instead of the merged, effective image would report a phantom
/// tag mismatch here every reconcile.
fn test_context_with_partial_repo_only_sonarr_override(client: kube::Client) -> Arc<Context> {
    let builtin =
        AppDefaults::try_for_app(&AppType::Sonarr).expect("sonarr must have image defaults");
    let mut image_overrides = HashMap::new();
    image_overrides.insert(
        "sonarr".to_string(),
        servarr_crds::ImageSpec {
            repository: builtin.image.repository,
            tag: String::new(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );
    Arc::new(Context {
        client,
        image_overrides,
        legacy_image_override_apps: std::collections::HashSet::new(),
        reporter: Reporter {
            controller: "test-controller".into(),
            instance: None,
        },
        watch_namespace: Some("test".into()),
        app_api_base_override: None,
        event_publish_tasks: tokio_util::task::TaskTracker::new(),
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
            service_name: None,
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
            seerr_sync: None,
            bazarr_sync: None,
            subgen_sync: None,
            maintainerr_sync: None,
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
fn deployment_response(name: &str, ns: &str, app_type: &str) -> serde_json::Value {
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
            "selector": { "matchLabels": {
                "app.kubernetes.io/name": app_type,
                "app.kubernetes.io/instance": name
            } },
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

/// A Deployment whose selector uses the old, pre-rename label shape — the drift
/// the delete-then-recreate path exists to fix. `owner_uid` controls whether the
/// object claims `uid` as its ServarrApp owner (`Some`) or is a foreign name
/// collision (`None`); the controller must only delete the former.
fn stale_selector_deployment(name: &str, ns: &str, owner_uid: Option<&str>) -> serde_json::Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "deploy-uid-1",
            "resourceVersion": "100",
            "ownerReferences": match owner_uid {
                Some(uid) => json!([{
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "name": name,
                    "uid": uid
                }]),
                None => json!([]),
            }
        },
        "spec": {
            "selector": { "matchLabels": { "app": name } },
            "template": {
                "metadata": { "labels": { "app": name } },
                "spec": {
                    "containers": [{
                        "name": "sonarr",
                        "image": "ghcr.io/onedr0p/sonarr:latest"
                    }]
                }
            }
        },
        "status": { "readyReplicas": 1, "replicas": 1, "availableReplicas": 1 }
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
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr",
                "test",
                "sonarr",
            )),
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
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr",
                "test",
                "sonarr",
            )),
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

    // GET ServarrApps list (for gauge update + prowlarr/seerr sync checks)
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
// Test 1b: Seerr reconcile publishes a Warning Event when the image came from the
// deprecated DEFAULT_IMAGE_OVERSEERR_* fallback (#534)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_seerr_reconcile_warns_on_legacy_image_override() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_legacy_seerr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Seerr,
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-seerr", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-seerr".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path("/apis/apps/v1/namespaces/test/deployments/test-seerr"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-seerr",
                "test",
                "seerr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/apis/apps/v1/namespaces/test/deployments/test-seerr"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-seerr",
                "test",
                "seerr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-seerr"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-seerr", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(pvc_response("test-seerr-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-seerr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(networkpolicy_response("test-seerr", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-seerr/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(servarrapp_response("test-seerr", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let deprecated_override_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body).is_ok_and(|body| {
                body["reason"] == "DeprecatedImageOverride" && body["type"] == "Warning"
            })
    });
    assert!(
        deprecated_override_event,
        "reconcile should publish a Warning Event with reason DeprecatedImageOverride when \
         the app's image came from the legacy DEFAULT_IMAGE_OVERSEERR_* fallback (#534), \
         requests: {requests:?}"
    );
}

/// Regression test: an explicit `spec.image` always wins over the env-var fallback (see
/// deployment::build's merge order), so the deprecation Event must not fire for an app that
/// pins its own image -- publishing it there would be actively misleading.
#[tokio::test]
async fn test_seerr_reconcile_skips_legacy_override_warning_when_image_pinned() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_legacy_seerr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Seerr,
        image: Some(servarr_crds::ImageSpec {
            repository: "ghcr.io/seerr-team/seerr".into(),
            tag: "v3.4.1".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-seerr-pinned", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-seerr-pinned".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-seerr-pinned",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-seerr-pinned",
                "test",
                "seerr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-seerr-pinned",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-seerr-pinned",
                "test",
                "seerr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-seerr-pinned"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-seerr-pinned", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-seerr-pinned-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-seerr-pinned",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-seerr-pinned", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-seerr-pinned/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-seerr-pinned", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let deprecated_override_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|body| body["reason"] == "DeprecatedImageOverride")
    });
    assert!(
        !deprecated_override_event,
        "reconcile must not publish DeprecatedImageOverride when spec.image is explicitly \
         set -- the CR's own image always outranks the env fallback, so warning about the \
         fallback would be misleading, requests: {requests:?}"
    );
}

/// Regression test: a `DEFAULT_IMAGE_SONARR_*` override that differs from this
/// operator binary's compiled-in default publishes a Warning Event -- the general case of
/// #638, distinct from the Seerr-only rename detection above.
#[tokio::test]
async fn test_sonarr_reconcile_warns_on_stale_default_image() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_stale_sonarr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-sonarr".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(pvc_response("test-sonarr-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(networkpolicy_response("test-sonarr", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr/status",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(servarrapp_response("test-sonarr", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let stale_default_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body).is_ok_and(|body| {
                body["reason"] == "StaleDefaultImage" && body["type"] == "Warning"
            })
    });
    assert!(
        stale_default_event,
        "reconcile should publish a Warning Event with reason StaleDefaultImage when the \
         env-supplied image override differs from this operator's built-in default (#638), \
         requests: {requests:?}"
    );
}

/// Regression test: an image override that exactly matches this operator binary's built-in
/// default must not trigger the staleness warning -- there's nothing stale about it (#638).
#[tokio::test]
async fn test_sonarr_reconcile_skips_stale_warning_when_override_matches_builtin() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_current_sonarr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr-fresh", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-sonarr-fresh".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-fresh",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-fresh",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-fresh",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-fresh",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-fresh"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(service_response("test-sonarr-fresh", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-fresh-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-fresh",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-fresh", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-fresh/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-fresh", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let stale_default_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|body| body["reason"] == "StaleDefaultImage")
    });
    assert!(
        !stale_default_event,
        "reconcile must not publish StaleDefaultImage when the override already matches \
         this operator's built-in default -- there's nothing stale about it, \
         requests: {requests:?}"
    );
}

/// Regression test: a repository-only override (`DEFAULT_IMAGE_SONARR_REPO` set, no matching
/// `_TAG`) that matches the builtin repository must not warn, even though the raw override's
/// `tag` field is empty and would naively look "different" from the builtin's non-empty tag.
/// `deployment::build` merges the empty tag from the builtin default before deploying, so the
/// actually-running image is not stale (#638).
#[tokio::test]
async fn test_sonarr_reconcile_skips_stale_warning_for_partial_repo_only_override() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_partial_repo_only_sonarr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr-partial", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-sonarr-partial".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-partial",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-partial",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-partial",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-partial",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-partial"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(service_response("test-sonarr-partial", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-partial-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-partial",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-partial", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-partial/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-partial", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let stale_default_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|body| body["reason"] == "StaleDefaultImage")
    });
    assert!(
        !stale_default_event,
        "reconcile must not publish StaleDefaultImage for a repository-only override whose \
         repository matches the builtin -- the merged, effective tag inherits the builtin and \
         is not stale, requests: {requests:?}"
    );
}

/// Regression test: if the Events API rejects the StaleDefaultImage publish, reconcile must
/// still succeed -- this is an advisory Event, not load-bearing, matching the existing
/// DeprecatedImageOverride precedent this block is modeled on (#638).
#[tokio::test]
async fn test_sonarr_reconcile_survives_stale_warning_event_publish_failure() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context_with_stale_sonarr_override(client);

    let spec = ServarrAppSpec {
        app: AppType::Sonarr,
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-sonarr-publish-fail", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-sonarr-publish-fail".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-publish-fail",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-publish-fail",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-publish-fail",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-publish-fail",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/services/test-sonarr-publish-fail",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(service_response("test-sonarr-publish-fail", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-publish-fail-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-publish-fail",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-publish-fail", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-publish-fail/status",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(servarrapp_response(
            "test-sonarr-publish-fail",
            "test",
        )))
        .mount(&mock_server)
        .await;

    // Only the StaleDefaultImage publish fails. The reconcile-completion event still
    // succeeds, so this test isolates the one failure mode it checks. Every event is
    // advisory since #746, so a second failure would not change the assertion below. It
    // would still muddy which publish the test actually exercises.
    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .and(BodyContains("StaleDefaultImage"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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
        "reconcile must succeed even when the advisory StaleDefaultImage Event fails to \
         publish, got: {result:?}"
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
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-nonp",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    // GET deployment
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-nonp",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-nonp",
                "test",
                "sonarr",
            )),
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

/// Verify `Error::public_summary` returns safe strings for every variant.
#[test]
fn test_error_public_summary_all_variants() {
    let ser_err = servarr_operator::controller::Error::Serialization(
        serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
    );
    assert!(
        ser_err.public_summary().starts_with("Serialization error"),
        "Serialization is built from operator-owned structs only, safe to pass through: {}",
        ser_err.public_summary()
    );

    let app_err = servarr_operator::controller::Error::AppDefaults(
        "no image defaults for app: sonarr".to_string(),
    );
    assert_eq!(
        app_err.public_summary(),
        "app defaults error: no image defaults for app: sonarr"
    );

    let kube_err =
        servarr_operator::controller::Error::Kube(kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        })));
    let summary = kube_err.public_summary();
    assert!(
        summary.contains("403"),
        "should keep status code: {summary}"
    );
    assert!(
        !summary.contains("super-secret-name"),
        "must not leak the raw API server message: {summary}"
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
                service_name: None,
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
                seerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                maintainerr_sync: None,
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
                service_name: None,
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
                seerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                maintainerr_sync: None,
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
                service_name: None,
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
                seerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                maintainerr_sync: None,
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
                service_name: None,
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
                seerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                maintainerr_sync: None,
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
                service_name: None,
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
                seerr_sync: None,
                bazarr_sync: None,
                subgen_sync: None,
                maintainerr_sync: None,
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

/// Mounts the MediaStack orphan-cleanup mock set shared by every orphan-cleanup test: PATCH +
/// GET for the real child (ready), a GET-by-label list returning the real child plus one
/// orphan, a MediaStack status PATCH, and the MediaStack-list gauge GET. Callers mount their
/// own PVC-detach and orphan-DELETE mocks on top, since those vary per test (#549).
async fn mount_orphan_stack_mocks(
    mock_server: &MockServer,
    stack_name: &str,
    child_name: &str,
    orphan_name: &str,
    ns: &str,
) {
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps/{child_name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(servarrapp_response(child_name, ns)))
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps/{child_name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut resp = servarrapp_response(child_name, ns);
            resp["status"] = json!({
                "ready": true,
                "readyReplicas": 1,
                "observedGeneration": 1,
                "conditions": []
            });
            resp
        }))
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/servarrapps"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrAppList",
            "metadata": {},
            "items": [
                {
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": child_name,
                        "namespace": ns,
                        "uid": "sa-uid-real",
                        "resourceVersion": "200"
                    },
                    "spec": { "app": "Sonarr" }
                },
                {
                    "apiVersion": "servarr.dev/v1alpha1",
                    "kind": "ServarrApp",
                    "metadata": {
                        "name": orphan_name,
                        "namespace": ns,
                        "uid": "sa-uid-orphan",
                        "resourceVersion": "201"
                    },
                    "spec": { "app": "Radarr" }
                }
            ]
        })))
        .named("list-servarrapps-with-orphan")
        .mount(mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/servarr.dev/v1alpha1/namespaces/{ns}/mediastacks/{stack_name}/status"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(mediastack_response(stack_name, ns)))
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
        .mount(mock_server)
        .await;
}

#[tokio::test]
async fn test_media_stack_reconcile_orphan_cleanup() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    // Stack has only Sonarr
    let stack = Arc::new(make_media_stack("orphan-stack", "test"));

    mount_orphan_stack_mocks(
        &mock_server,
        "orphan-stack",
        "orphan-stack-sonarr",
        "orphan-stack-old-radarr",
        "test",
    )
    .await;

    // DELETE the orphaned child -- the controller should call this. "kind": "Status" (not
    // "ServarrApp"): kube's `request_status` picks the response type by that field, and a body
    // claiming "ServarrApp" without the required `spec` fails to deserialize -- which used to
    // be silently swallowed as a `warn!` instead of surfacing via `StuckOrphans` (#722).
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/orphan-stack-old-radarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success"
        })))
        .expect(1)
        .named("delete-orphan")
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

    // A cleanly-deleted orphan (no detach failure) must report the healthy condition.
    let requests = mock_server.received_requests().await.unwrap_or_default();
    let status_body: serde_json::Value = requests
        .iter()
        .find(|r| {
            r.method == wiremock::http::Method::PATCH
                && r.url.path().ends_with("/mediastacks/orphan-stack/status")
        })
        .expect("MediaStack status PATCH should have been sent")
        .body_json()
        .expect("status patch body should be valid JSON");
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("status.conditions should be an array");
    let orphan_condition = conditions
        .iter()
        .find(|c| c["conditionType"] == "OrphanCleanupHealthy")
        .expect("OrphanCleanupHealthy condition should be set");
    assert_eq!(orphan_condition["status"], "True");
    assert_eq!(orphan_condition["reason"], "NoStuckOrphans");
}

// ---------------------------------------------------------------------------
// Test 9b: MediaStack orphan cleanup runs BEFORE applying children (#533)
// ---------------------------------------------------------------------------

/// Regression test for #533: when a stack app is renamed (e.g. Overseerr -> Seerr), the old
/// child ServarrApp becomes an orphan whose normalized `spec.app` collides with the new
/// child's under the admission webhook's duplicate-instance check. If the operator applies
/// the new child before deleting the stale orphan, every apply is rejected and the orphan
/// cleanup block (which would remove the collision) is never reached -- a permanent
/// requeue loop. The fix must run orphan cleanup before applying children, so this test
/// asserts that ordering directly against the mock server's received request log rather
/// than simulating the webhook rejection itself.
#[tokio::test]
async fn test_media_stack_reconcile_orphan_cleanup_runs_before_apply() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    // Stack has only Sonarr
    let stack = Arc::new(make_media_stack("migrate-stack", "test"));

    mount_orphan_stack_mocks(
        &mock_server,
        "migrate-stack",
        "migrate-stack-sonarr",
        "migrate-stack-old-radarr",
        "test",
    )
    .await;

    // PATCH the orphan's config PVC to strip its ownerReference (must happen before the
    // ServarrApp DELETE below).
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/persistentvolumeclaims/migrate-stack-old-radarr-config",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("migrate-stack-old-radarr-config", "test")),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // DELETE the orphaned child. "kind": "Status" (not "ServarrApp") -- see the equivalent
    // mock in `test_media_stack_reconcile_orphan_cleanup` for why (#722).
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/migrate-stack-old-radarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;
    assert!(result.is_ok(), "reconcile should succeed, got: {result:?}");

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let delete_orphan_idx = requests
        .iter()
        .position(|r| {
            r.method == wiremock::http::Method::DELETE
                && r.url
                    .path()
                    .ends_with("/servarrapps/migrate-stack-old-radarr")
        })
        .expect("orphan DELETE request should have been sent");
    let patch_child_idx = requests
        .iter()
        .position(|r| {
            r.method == wiremock::http::Method::PATCH
                && r.url.path().ends_with("/servarrapps/migrate-stack-sonarr")
        })
        .expect("child PATCH request should have been sent");

    assert!(
        delete_orphan_idx < patch_child_idx,
        "orphan cleanup (DELETE at request #{delete_orphan_idx}) must run before applying \
         children (PATCH at request #{patch_child_idx}) -- otherwise a webhook \
         duplicate-instance rejection on the apply call aborts reconcile before cleanup ever \
         runs, permanently wedging the stack (issue #533)"
    );

    // Regression test for the data-loss variant of #533 caught in review: the orphaned
    // ServarrApp owns its config PVC via an ownerReference (see
    // servarr_resources::common::metadata), so a plain cascading delete would destroy the
    // PVC along with it. Once cleanup runs before apply (the #533 fix above), that delete
    // actually succeeds on every rename -- where before it was blocked by the deadlock and
    // the PVC survived by accident. Rather than orphaning the *entire* child (which would
    // leave its Deployment/Service/Secrets permanently running and unmanaged -- caught in
    // review as its own regression), the fix strips only the PVC's ownerReference before a
    // normal cascading delete: the CR and everything else it owns is torn down as before,
    // but the PVC survives, unowned.
    let strip_pvc_owner_idx = requests
        .iter()
        .position(|r| {
            r.method == wiremock::http::Method::PATCH
                && r.url
                    .path()
                    .ends_with("/persistentvolumeclaims/migrate-stack-old-radarr-config")
        })
        .expect("PVC ownerReference-strip PATCH should have been sent");
    assert!(
        strip_pvc_owner_idx < delete_orphan_idx,
        "PVC ownership must be detached (request #{strip_pvc_owner_idx}) before the child \
         ServarrApp is deleted (request #{delete_orphan_idx}), or the cascade could destroy \
         the PVC before the detach patch lands"
    );
    let strip_body: serde_json::Value = requests[strip_pvc_owner_idx]
        .body_json()
        .expect("PVC patch body should be valid JSON");
    assert!(
        strip_body["metadata"]["ownerReferences"].is_null(),
        "PVC patch must null out ownerReferences to detach it from the deleted child, got: \
         {strip_body}"
    );

    let delete_body: serde_json::Value = requests[delete_orphan_idx]
        .body_json()
        .expect("DELETE request body should be valid JSON DeleteParams");
    assert!(
        delete_body.get("propagationPolicy").is_none(),
        "orphan ServarrApp delete must use the default (cascading) propagation policy -- \
         PropagationPolicy::Orphan on the whole CR would leave its Deployment/Service/Secrets \
         permanently running with no owner to ever clean them up, got body: {delete_body}"
    );
}

// ---------------------------------------------------------------------------
// Test 9c: MediaStack orphan cleanup skips the child delete when PVC detach fails (#562)
// ---------------------------------------------------------------------------

/// Regression test for #562: if the PVC ownerReference-detach PATCH fails with anything
/// other than 404, the orphaned child ServarrApp must NOT be deleted this reconcile.
/// Deleting it anyway would let Kubernetes' cascading GC take the still-owned PVC down
/// with it -- silently destroying the user's config data, which is exactly what the
/// detach step exists to prevent. The child stays as an orphan and gets retried on the
/// next reconcile instead.
#[tokio::test]
async fn test_media_stack_reconcile_orphan_cleanup_skips_delete_when_pvc_detach_fails() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    // Stack has only Sonarr
    let stack = Arc::new(make_media_stack("detach-fail-stack", "test"));

    mount_orphan_stack_mocks(
        &mock_server,
        "detach-fail-stack",
        "detach-fail-stack-sonarr",
        "detach-fail-stack-old-radarr",
        "test",
    )
    .await;

    // PATCH the orphan's config PVC to strip its ownerReference -- fails with a non-404
    // error, simulating a transient API problem.
    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/persistentvolumeclaims/detach-fail-stack-old-radarr-config",
        ))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "internal error",
            "reason": "InternalError",
            "code": 500
        })))
        .expect(1)
        .named("patch-pvc-detach-fails")
        .mount(&mock_server)
        .await;

    // The orphaned child must NOT be deleted -- expect(0) is verified when mock_server drops.
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/detach-fail-stack-old-radarr",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "servarr.dev/v1alpha1",
            "kind": "ServarrApp",
            "metadata": {
                "name": "detach-fail-stack-old-radarr",
                "namespace": "test",
                "uid": "sa-uid-orphan",
                "resourceVersion": "201"
            }
        })))
        .expect(0)
        .named("delete-orphan-must-not-happen")
        .mount(&mock_server)
        .await;

    let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

    assert!(
        result.is_ok(),
        "reconcile should still succeed even when one orphan's PVC detach fails, got: {result:?}"
    );
    // _pvc_mock / DELETE mock drop verifies expect(1) / expect(0) above: the PVC detach
    // was attempted exactly once, and the child delete never happened.

    // The stuck orphan must be visible on MediaStack status, not just in pod logs, and a
    // transient failure must be labeled as such so on-call knows it may self-resolve on
    // retry (#610).
    let requests = mock_server.received_requests().await.unwrap_or_default();
    let status_body: serde_json::Value = requests
        .iter()
        .find(|r| {
            r.method == wiremock::http::Method::PATCH
                && r.url
                    .path()
                    .ends_with("/mediastacks/detach-fail-stack/status")
        })
        .expect("MediaStack status PATCH should have been sent")
        .body_json()
        .expect("status patch body should be valid JSON");
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("status.conditions should be an array");
    let orphan_condition = conditions
        .iter()
        .find(|c| c["conditionType"] == "OrphanCleanupHealthy")
        .expect("OrphanCleanupHealthy condition should be set");
    assert_eq!(orphan_condition["status"], "False");
    assert_eq!(orphan_condition["reason"], "PvcDetachFailed");
    let message = orphan_condition["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("detach-fail-stack-old-radarr"),
        "condition message should name the stuck orphan, got: {orphan_condition}"
    );
    assert!(
        message.contains("may self-resolve") && !message.contains("will not self-resolve"),
        "a 500 detach failure should be labeled transient, not permission-denied, got: \
         {orphan_condition}"
    );
}

// ---------------------------------------------------------------------------
// Test 9d: MediaStack orphan cleanup labels a 401/403 detach failure as a permission
// denial that needs a manual fix (#610)
// ---------------------------------------------------------------------------

/// Follow-up to #562: a `401`/`403` on the PVC ownerReference-detach PATCH means the request
/// was rejected for who's asking, not what's asked -- it will never clear on its own the way a
/// transient 5xx/network error might. The stuck orphan must still not be deleted (same
/// invariant as the transient case), but the status Condition should tell on-call this needs a
/// manual fix rather than "wait for the next reconcile". Covers both status codes in one test
/// via a table, since they exercise the same classification branch (#610 review: a prior draft
/// classified only 403, silently mislabeling a 401 credential failure as self-resolving).
#[tokio::test]
async fn test_media_stack_reconcile_orphan_cleanup_labels_401_and_403_as_permission_denied() {
    for (status_code, stack_suffix) in [(401u16, "401"), (403u16, "403")] {
        let mock_server = MockServer::start().await;
        let client = mock_client(&mock_server.uri()).await;
        let ctx = test_context(client);

        let stack_name = format!("perm-fail-stack-{stack_suffix}");
        let child_name = format!("{stack_name}-sonarr");
        let orphan_name = format!("{stack_name}-old-radarr");
        let stack = Arc::new(make_media_stack(&stack_name, "test"));

        mount_orphan_stack_mocks(&mock_server, &stack_name, &child_name, &orphan_name, "test")
            .await;

        // PATCH the orphan's config PVC to strip its ownerReference -- fails with 401/403. The
        // seeded API-server message must never leak into the tenant-visible condition (only the
        // status code and static prose should).
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/api/v1/namespaces/test/persistentvolumeclaims/{orphan_name}-config"
            )))
            .respond_with(ResponseTemplate::new(status_code).set_body_json(json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "SEEDED_API_SERVER_MESSAGE_MUST_NOT_LEAK",
                "reason": "Forbidden",
                "code": status_code
            })))
            .expect(1)
            .named("patch-pvc-detach-forbidden")
            .mount(&mock_server)
            .await;

        // The orphaned child must NOT be deleted -- expect(0) is verified when mock_server drops.
        Mock::given(method("DELETE"))
            .and(path(format!(
                "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/{orphan_name}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "servarr.dev/v1alpha1",
                "kind": "ServarrApp",
                "metadata": {
                    "name": orphan_name,
                    "namespace": "test",
                    "uid": "sa-uid-orphan",
                    "resourceVersion": "201"
                }
            })))
            .expect(0)
            .named("delete-orphan-must-not-happen")
            .mount(&mock_server)
            .await;

        let result = servarr_operator::media_stack_controller::reconcile(stack, ctx).await;

        assert!(
            result.is_ok(),
            "reconcile should still succeed even when one orphan's PVC detach is forbidden \
             (status {status_code}), got: {result:?}"
        );

        let requests = mock_server.received_requests().await.unwrap_or_default();
        let status_body: serde_json::Value = requests
            .iter()
            .find(|r| {
                r.method == wiremock::http::Method::PATCH
                    && r.url
                        .path()
                        .ends_with(&format!("/mediastacks/{stack_name}/status"))
            })
            .expect("MediaStack status PATCH should have been sent")
            .body_json()
            .expect("status patch body should be valid JSON");
        let conditions = status_body["status"]["conditions"]
            .as_array()
            .expect("status.conditions should be an array");
        let orphan_condition = conditions
            .iter()
            .find(|c| c["conditionType"] == "OrphanCleanupHealthy")
            .expect("OrphanCleanupHealthy condition should be set");
        assert_eq!(orphan_condition["status"], "False");
        assert_eq!(orphan_condition["reason"], "PvcDetachFailed");
        let message = orphan_condition["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&orphan_name),
            "condition message should name the stuck orphan (status {status_code}), got: \
             {orphan_condition}"
        );
        assert!(
            message.contains("will not self-resolve"),
            "a {status_code} detach failure should be labeled as a permission denial that \
             needs a manual fix, got: {orphan_condition}"
        );
        assert!(
            !message.contains("SEEDED_API_SERVER_MESSAGE_MUST_NOT_LEAK"),
            "the raw API-server message must never reach the tenant-visible condition \
             (status {status_code}), got: {orphan_condition}"
        );
    }
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
async fn mount_common_mocks(mock_server: &MockServer, name: &str, ns: &str, app_type: &str) {
    // PATCH deployment (SSA)
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/{name}"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(name, ns, app_type)),
        )
        .mount(mock_server)
        .await;

    // GET deployment (selector-drift pre-check + template-drift check + status)
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/{name}"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(name, ns, app_type)),
        )
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

    mount_common_mocks(&mock_server, "test-transmission", "test", "transmission").await;

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

/// #542: when both adminCredentials and legacy appConfig.transmission.auth are set,
/// deployment::build silently drops the legacy env vars (#536) -- reconcile must still
/// surface that as a Warning Event, not just an operator-log line.
#[tokio::test]
async fn test_transmission_reconcile_warns_on_legacy_auth_with_admin_credentials() {
    use servarr_crds::{AdminCredentialsSpec, AppConfig, TransmissionAuth, TransmissionConfig};

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Transmission,
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "test-transmission-admin".into(),
        }),
        app_config: Some(AppConfig::Transmission(TransmissionConfig {
            auth: Some(TransmissionAuth {
                secret_name: "test-transmission-legacy-auth".into(),
            }),
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-transmission-both-auth", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-tx-both-auth".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(
        &mock_server,
        "test-transmission-both-auth",
        "test",
        "transmission",
    )
    .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-transmission-both-auth",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-transmission-both-auth", "test")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(result.is_ok(), "reconcile should succeed, got: {result:?}");

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let deprecated_auth_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body).is_ok_and(|body| {
                body["reason"] == "DeprecatedTransmissionAuth" && body["type"] == "Warning"
            })
    });
    assert!(
        deprecated_auth_event,
        "reconcile should publish a Warning Event with reason DeprecatedTransmissionAuth \
         when both adminCredentials and the legacy appConfig.transmission.auth are set, \
         requests: {requests:?}"
    );
}

/// adminCredentials alone (the supported, non-deprecated shape) must not trigger the
/// deprecation Event -- only the presence of the legacy auth block alongside it does.
#[tokio::test]
async fn test_transmission_reconcile_skips_legacy_auth_warning_without_legacy_block() {
    use servarr_crds::AdminCredentialsSpec;

    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let spec = ServarrAppSpec {
        app: AppType::Transmission,
        admin_credentials: Some(AdminCredentialsSpec {
            secret_name: "test-transmission-admin-only".into(),
        }),
        ..Default::default()
    };
    let mut app = ServarrApp::new("test-transmission-admin-only", spec);
    app.metadata.namespace = Some("test".into());
    app.metadata.uid = Some("test-uid-tx-admin-only".into());
    app.metadata.resource_version = Some("1".into());
    app.metadata.generation = Some(1);
    let app = Arc::new(app);

    mount_common_mocks(
        &mock_server,
        "test-transmission-admin-only",
        "test",
        "transmission",
    )
    .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/configmaps/test-transmission-admin-only",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(configmap_response("test-transmission-admin-only", "test")),
        )
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(result.is_ok(), "reconcile should succeed, got: {result:?}");

    let requests = mock_server.received_requests().await.unwrap_or_default();
    let deprecated_auth_event = requests.iter().any(|r| {
        r.method == wiremock::http::Method::POST
            && r.url.path() == "/apis/events.k8s.io/v1/namespaces/test/events"
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|body| body["reason"] == "DeprecatedTransmissionAuth")
    });
    assert!(
        !deprecated_auth_event,
        "reconcile must not publish DeprecatedTransmissionAuth when only adminCredentials \
         is set -- that's the supported, non-deprecated shape, requests: {requests:?}"
    );
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
            enabled: Some(true),
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

    mount_common_mocks(&mock_server, "test-sonarr-gw", "test", "sonarr").await;

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
            enabled: Some(true),
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

    mount_common_mocks(&mock_server, "test-radarr-gw", "test", "radarr").await;

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

    mount_common_mocks(&mock_server, "test-bastion", "test", "ssh-bastion").await;

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

    mount_common_mocks(&mock_server, "test-sabnzbd", "test", "sabnzbd").await;

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

    mount_common_mocks(&mock_server, "test-prowlarr", "test", "prowlarr").await;

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
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-pvc",
                "test",
                "sonarr",
            )),
        )
        .mount(&mock_server)
        .await;

    // GET deployment
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-pvc",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-pvc",
                "test",
                "sonarr",
            )),
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

    mount_common_mocks(&mock_server, "test-sonarr-npc", "test", "sonarr").await;

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
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-drift",
                "test",
                "sonarr",
            )),
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
            "selector": { "matchLabels": {
                "app.kubernetes.io/name": "sonarr",
                "app.kubernetes.io/instance": "test-sonarr-drift"
            } },
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
// Test 18b: Deployment selector drift (immutable field) triggers delete-then-
// recreate instead of a rejected patch. Regression test for issue #44: an
// AppType rename changes the app.kubernetes.io/name selector label, and
// Deployment.spec.selector is immutable on apps/v1 — a live Deployment whose
// selector no longer matches the desired one must be deleted and recreated,
// not patched.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deployment_selector_drift_triggers_delete_then_recreate() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr-seldrift", "test"));

    // GET deployment returns a live object whose selector uses the old,
    // pre-rename label shape — simulating an app whose AppType label value
    // changed since it was first deployed.
    let stale_selector_deploy =
        stale_selector_deployment("test-sonarr-seldrift", "test", Some("test-uid-12345"));
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-seldrift",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(stale_selector_deploy))
        .named("get-deployment-stale-selector")
        .mount(&mock_server)
        .await;

    // DELETE deployment — must be called exactly once, before the recreate PATCH.
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-seldrift",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": { "name": "test-sonarr-seldrift", "namespace": "test" }
        })))
        .named("delete-deployment")
        .expect(1)
        .mount(&mock_server)
        .await;

    // PATCH deployment (SSA) — recreates it with the current selector.
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-seldrift",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-seldrift",
                "test",
                "sonarr",
            )),
        )
        .named("patch-deployment")
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/api/v1/namespaces/test/services/test-sonarr-seldrift",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(service_response("test-sonarr-seldrift", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-seldrift-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-seldrift",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-seldrift", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-seldrift/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-seldrift", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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
        "reconcile with selector drift should delete-then-recreate and succeed, got: {result:?}"
    );
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(300)));
    // wiremock's `.expect(1)` on the DELETE mock is verified when `mock_server` drops at the
    // end of this test — a missing or duplicate DELETE call fails the test at that point.
}

#[tokio::test]
async fn test_deployment_get_non_404_error_surfaces() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr-geterr", "test"));

    // Pre-deployment finalizer checks list ServarrApps; empty list = no sync to lock onto.
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

    // A non-404 GET failure on the Deployment must surface as a reconcile error, not be
    // silently swallowed by the selector-drift pre-check (silent-failure-hunter finding).
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-geterr",
        ))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "internal server error",
            "reason": "InternalError",
            "code": 500
        })))
        .named("get-deployment-500")
        .mount(&mock_server)
        .await;

    // The SSA patch must never run when the pre-check GET fails: the error propagates first.
    // (Regression guard: the old `if let Ok(existing)` swallowed the 500 and would have
    // reached this patch, tripping `.expect(0)`.)
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-geterr",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-geterr",
                "test",
                "sonarr",
            )),
        )
        .named("patch-deployment")
        .expect(0)
        .mount(&mock_server)
        .await;

    let result = servarr_operator::controller::reconcile(app, ctx).await;
    assert!(
        result.is_err(),
        "reconcile must propagate a non-404 Deployment GET error, got: {result:?}"
    );
}

#[tokio::test]
async fn test_foreign_deployment_not_deleted_on_selector_mismatch() {
    let mock_server = MockServer::start().await;
    let client = mock_client(&mock_server.uri()).await;
    let ctx = test_context(client);

    let app = Arc::new(make_sonarr_app("test-sonarr-foreign", "test"));

    // GET deployment returns an object with a mismatched selector but NO owner reference
    // pointing at this ServarrApp (uid "test-uid-12345") — a foreign name collision.
    // The delete gate must refuse to tear it down (security-audit finding).
    let foreign_deploy = stale_selector_deployment("test-sonarr-foreign", "test", None);
    Mock::given(method("GET"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-foreign",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(foreign_deploy))
        .named("get-deployment-foreign")
        .mount(&mock_server)
        .await;

    // The foreign Deployment must NOT be deleted — assert zero DELETE calls.
    Mock::given(method("DELETE"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-foreign",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-foreign",
                "test",
                "sonarr",
            )),
        )
        .named("delete-deployment-foreign")
        .expect(0)
        .mount(&mock_server)
        .await;

    // Reconcile proceeds to SSA-patch the Deployment (the pre-existing path) and continues.
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/apps/v1/namespaces/test/deployments/test-sonarr-foreign",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(deployment_response(
                "test-sonarr-foreign",
                "test",
                "sonarr",
            )),
        )
        .named("patch-deployment")
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/api/v1/namespaces/test/services/test-sonarr-foreign"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(service_response("test-sonarr-foreign", "test")),
        )
        .mount(&mock_server)
        .await;

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

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/api/v1/namespaces/test/persistentvolumeclaims/.*",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pvc_response("test-sonarr-foreign-config", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/networking.k8s.io/v1/namespaces/test/networkpolicies/test-sonarr-foreign",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(networkpolicy_response("test-sonarr-foreign", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(
            "/apis/servarr.dev/v1alpha1/namespaces/test/servarrapps/test-sonarr-foreign/status",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(servarrapp_response("test-sonarr-foreign", "test")),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/apis/events.k8s.io/v1/namespaces/test/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(event_response()))
        .mount(&mock_server)
        .await;

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
        "reconcile with a foreign Deployment must not delete it and should succeed, got: {result:?}"
    );
    // wiremock's `.expect(0)` on the DELETE mock is verified when `mock_server` drops.
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
            enabled: Some(true),
            route_type: Some(RouteType::Tcp),
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

    mount_common_mocks(&mock_server, "test-plex-tcp", "test", "plex").await;

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
            service_name: None,
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
            seerr_sync: None,
            bazarr_sync: None,
            subgen_sync: None,
            maintainerr_sync: None,
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
