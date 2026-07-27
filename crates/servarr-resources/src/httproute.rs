use kube::api::DynamicObject;
use serde_json::json;
use servarr_crds::{AppDefaults, RouteType, ServarrApp};

use crate::common;

pub fn build(app: &ServarrApp) -> Result<Option<DynamicObject>, String> {
    let Some(gateway) = app.spec.gateway.as_ref() else {
        return Ok(None);
    };
    if !gateway.is_enabled() {
        return Ok(None);
    }

    // Only build HTTPRoute when route_type is Http (don't build for apps like SshBastion
    // that default to Tcp). TLS also forces TCP routes, so skip HTTPRoute then too.
    if matches!(gateway.effective_route_type(&app.spec.app), RouteType::Tcp)
        || gateway.tls.as_ref().is_some_and(|t| t.enabled)
    {
        return Ok(None);
    }

    let defaults = AppDefaults::try_for_app(&app.spec.app)
        .inspect_err(|e| common::log_app_defaults_error(app, e))?;
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let first_port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);

    let name = common::app_name(app);
    let ns = common::app_namespace(app);

    let parent_refs: Vec<serde_json::Value> = gateway
        .parent_refs
        .iter()
        .map(|pr| {
            let mut ref_obj = json!({
                "name": pr.name,
            });
            if !pr.namespace.is_empty() {
                ref_obj["namespace"] = json!(pr.namespace);
            }
            if !pr.section_name.is_empty() {
                ref_obj["sectionName"] = json!(pr.section_name);
            }
            ref_obj
        })
        .collect();

    let hostnames: Vec<serde_json::Value> = gateway.hosts.iter().map(|h| json!(h)).collect();

    let route = json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": common::labels(app),
            "ownerReferences": [common::owner_reference(app)],
        },
        "spec": {
            "parentRefs": parent_refs,
            "hostnames": hostnames,
            "rules": [{
                "backendRefs": [{
                    "name": common::service_name(app),
                    "port": first_port,
                }],
            }],
        },
    });

    Ok(serde_json::from_value(route).ok())
}
