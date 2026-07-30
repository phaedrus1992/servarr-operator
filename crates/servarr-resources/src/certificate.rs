use kube::api::DynamicObject;
use serde_json::json;
use servarr_crds::ServarrApp;

use crate::common;

/// Build a cert-manager Certificate resource (cert-manager.io/v1).
///
/// Returns `Ok(Some(_))` when the gateway is enabled and TLS is configured with
/// a cert_issuer, `Ok(None)` when not applicable, `Err` if the constructed
/// document fails to deserialize into a `DynamicObject`. Uses DynamicObject
/// since cert-manager types are not in kube-rs / k8s-openapi.
pub fn build(app: &ServarrApp) -> Result<Option<DynamicObject>, String> {
    let Some(gateway) = app.spec.gateway.as_ref() else {
        return Ok(None);
    };
    if !gateway.is_enabled() {
        return Ok(None);
    }

    let Some(tls) = gateway.tls.as_ref() else {
        return Ok(None);
    };
    if !tls.enabled || tls.cert_issuer.is_empty() {
        return Ok(None);
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

    Ok(Some(serde_json::from_value(cert).map_err(|e| {
        format!("failed to build Certificate: {e}")
    })?))
}
