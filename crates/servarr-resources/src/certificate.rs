use kube::api::DynamicObject;
use serde_json::json;
use servarr_crds::ServarrApp;

use crate::common;

/// Build a cert-manager Certificate resource (cert-manager.io/v1).
///
/// Returns `Some` when the gateway is enabled and TLS is configured with
/// a cert_issuer. Uses DynamicObject since cert-manager types are not
/// in kube-rs / k8s-openapi.
pub fn build(app: &ServarrApp) -> Option<DynamicObject> {
    let gateway = app.spec.gateway.as_ref()?;
    if !gateway.enabled {
        return None;
    }

    let tls = gateway.tls.as_ref()?;
    if !tls.enabled || tls.cert_issuer.is_empty() {
        return None;
    }

    let name = common::app_name(app);
    let ns = common::app_namespace(app);

    let secret_name = tls
        .secret_name
        .clone()
        .unwrap_or_else(|| format!("{name}-tls"));

    let dns_names: Vec<serde_json::Value> = gateway.hosts.iter().map(|h| json!(h)).collect();

    let cert = json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Certificate",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": common::labels(app),
            "ownerReferences": [common::owner_reference(app)],
        },
        "spec": {
            "secretName": secret_name,
            "dnsNames": dns_names,
            "issuerRef": {
                "name": tls.cert_issuer,
                "kind": "ClusterIssuer",
            },
        },
    });

    serde_json::from_value(cert).ok()
}
