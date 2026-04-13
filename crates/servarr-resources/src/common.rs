use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::Resource;
use kube::api::ObjectMeta;
use servarr_crds::ServarrApp;
use std::collections::BTreeMap;

pub const MANAGER: &str = "servarr-operator";

pub fn app_name(app: &ServarrApp) -> String {
    app.metadata
        .name
        .clone()
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn name_for(app: &ServarrApp, suffix: &str) -> String {
    child_name(app, suffix)
}

pub fn app_namespace(app: &ServarrApp) -> String {
    app.metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string())
}

pub fn namespace(app: &ServarrApp) -> String {
    app_namespace(app)
}

pub fn labels(app: &ServarrApp) -> BTreeMap<String, String> {
    let name = app_name(app);
    let app_type = app.spec.app.to_string();
    let mut labels = BTreeMap::from([
        ("app.kubernetes.io/name".into(), app_type.clone()),
        ("app.kubernetes.io/instance".into(), name),
        ("app.kubernetes.io/managed-by".into(), MANAGER.into()),
        ("servarr.dev/app".into(), app_type),
    ]);
    if let Some(ref instance) = app.spec.instance {
        labels.insert("servarr.dev/instance".into(), instance.clone());
    }
    labels
}

pub fn selector_labels(app: &ServarrApp) -> BTreeMap<String, String> {
    let name = app_name(app);
    let app_type = app.spec.app.to_string();
    BTreeMap::from([
        ("app.kubernetes.io/name".into(), app_type),
        ("app.kubernetes.io/instance".into(), name),
    ])
}

pub fn owner_reference(app: &ServarrApp) -> OwnerReference {
    app.controller_owner_ref(&()).unwrap()
}

pub fn owner_ref(app: &ServarrApp) -> OwnerReference {
    owner_reference(app)
}

pub fn child_name(app: &ServarrApp, suffix: &str) -> String {
    let name = app_name(app);
    if suffix.is_empty() {
        name
    } else {
        format!("{name}-{suffix}")
    }
}

pub fn metadata(app: &ServarrApp, suffix: &str) -> ObjectMeta {
    ObjectMeta {
        name: Some(child_name(app, suffix)),
        namespace: Some(app_namespace(app)),
        labels: Some(labels(app)),
        owner_references: Some(vec![owner_reference(app)]),
        ..Default::default()
    }
}
