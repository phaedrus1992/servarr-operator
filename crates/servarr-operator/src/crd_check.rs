//! Startup self-check: warn when an installed CRD is missing a field this operator build
//! expects, so a missed `servarr-crds` upgrade shows up as a clear warning instead of a
//! silently-pruned field on the next server-side apply (#543).

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{Api, Client};
use servarr_api::k8s::kube_err_summary;
use tracing::warn;

/// `v1.3.0` added `spec.apiHealthCheck` to both CRDs (the Transmission self-heal trigger) — a
/// good sentinel: absent means the installed CRD predates this operator build.
const EXPECTED_CRD_NAMES: [&str; 2] = ["servarrapps.servarr.dev", "mediastacks.servarr.dev"];
const CR_VERSION: &str = "v1alpha1";
const ADDED_IN: &str = "servarr-operator v1.3.0";

/// Best-effort, read-only startup diagnostic: warn (never fail startup) if an installed CRD
/// looks stale relative to this operator build. A missing CRD, an RBAC denial reading
/// `customresourcedefinitions`, or a network hiccup just skips the check for that CRD — this is
/// a diagnostic aid, not a startup gate.
pub async fn check(client: &Client) {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    for crd_name in EXPECTED_CRD_NAMES {
        match api.get(crd_name).await {
            Ok(crd) => {
                if let Some(reason) = staleness_reason(&crd) {
                    warn!(
                        crd = crd_name,
                        "{reason} — upgrade servarr-crds before (or together with) the \
                         operator; see docs/installation.md"
                    );
                }
            }
            Err(e) => {
                warn!(
                    crd = crd_name,
                    error = %kube_err_summary(&e),
                    "could not read installed CRD to run the startup schema self-check; skipping"
                );
            }
        }
    }
}

/// Returns a human-readable reason the installed CRD is stale, or `None` if it looks current.
/// Pure function, no I/O — kept separate from `check` so it's testable without a mock API server.
fn staleness_reason(crd: &CustomResourceDefinition) -> Option<String> {
    let version = crd.spec.versions.iter().find(|v| v.name == CR_VERSION)?;
    let schema = version
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref())?;

    let has_api_health_check = schema
        .properties
        .as_ref()
        .and_then(|p| p.get("spec"))
        .and_then(|spec| spec.properties.as_ref())
        .is_some_and(|spec_props| spec_props.contains_key("apiHealthCheck"));

    if has_api_health_check {
        None
    } else {
        Some(format!(
            "installed CRD is missing field `.spec.apiHealthCheck` (added in {ADDED_IN}); \
             CRDs appear stale"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
        CustomResourceDefinitionSpec, CustomResourceDefinitionVersion, CustomResourceValidation,
        JSONSchemaProps,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    fn schema_with_spec_properties(props: &[&str]) -> JSONSchemaProps {
        let spec_properties = props
            .iter()
            .map(|p| (p.to_string(), JSONSchemaProps::default()))
            .collect::<BTreeMap<_, _>>();
        JSONSchemaProps {
            properties: Some(BTreeMap::from([(
                "spec".to_string(),
                JSONSchemaProps {
                    properties: Some(spec_properties),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    fn crd_with_schema(version: &str, schema: Option<JSONSchemaProps>) -> CustomResourceDefinition {
        CustomResourceDefinition {
            metadata: ObjectMeta::default(),
            spec: CustomResourceDefinitionSpec {
                versions: vec![CustomResourceDefinitionVersion {
                    name: version.into(),
                    served: true,
                    storage: true,
                    schema: schema.map(|s| CustomResourceValidation {
                        open_api_v3_schema: Some(s),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        }
    }

    #[test]
    fn up_to_date_crd_has_no_staleness_reason() {
        let crd = crd_with_schema(
            "v1alpha1",
            Some(schema_with_spec_properties(&["apiHealthCheck", "app"])),
        );
        assert!(staleness_reason(&crd).is_none());
    }

    #[test]
    fn crd_missing_expected_field_is_stale() {
        let crd = crd_with_schema("v1alpha1", Some(schema_with_spec_properties(&["app"])));
        let reason = staleness_reason(&crd).expect("expected staleness reason");
        assert!(reason.contains("spec.apiHealthCheck"));
        assert!(reason.contains("servarr-operator v1.3.0"));
    }

    #[test]
    fn crd_missing_expected_version_is_not_flagged_by_this_check() {
        let crd = crd_with_schema("v1alpha", Some(schema_with_spec_properties(&["app"])));
        // No v1alpha1 version at all: staleness_reason can't find the version to inspect, so it
        // falls through the `?` and returns None (mismatch reported elsewhere, e.g. reconcile
        // errors) — this test documents that behavior rather than asserting a stronger warning.
        assert!(staleness_reason(&crd).is_none());
    }

    #[test]
    fn crd_with_no_schema_published_is_not_flagged() {
        let crd = crd_with_schema("v1alpha1", None);
        assert!(staleness_reason(&crd).is_none());
    }
}
