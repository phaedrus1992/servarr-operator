use kube::api::DynamicObject;
use serde_json::json;
use servarr_crds::{AppDefaults, ServarrApp};

use crate::common;

pub fn build(app: &ServarrApp) -> Option<DynamicObject> {
    let gateway = app.spec.gateway.as_ref()?;
    if !gateway.enabled {
        return None;
    }

    let defaults = AppDefaults::for_app(&app.spec.app);
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
                    "name": name,
                    "port": first_port,
                }],
            }],
        },
    });

    serde_json::from_value(route).ok()
}
