//! Startup self-check: warn when an installed CRD is missing a field this operator build
//! expects, so a missed `servarr-crds` upgrade shows up as a clear warning instead of a
//! silently-pruned field on the next server-side apply (#543).

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, JSONSchemaProps,
};
use kube::{Api, Client};
use servarr_api::k8s::kube_err_summary;
use tracing::warn;

/// A field this operator expects an installed CRD to publish, identified by its dotted path
/// under `spec.versions[].schema.openAPIV3Schema.properties` (e.g. `["spec", "apiHealthCheck"]`
/// checks `.spec.apiHealthCheck`). Absence means the installed CRD predates this operator build.
struct Expectation {
    crd_name: &'static str,
    cr_version: &'static str,
    field_path: &'static [&'static str],
    added_in: &'static str,
}

const EXPECTATIONS: &[Expectation] = &[
    Expectation {
        crd_name: "servarrapps.servarr.dev",
        cr_version: "v1alpha1",
        field_path: &["spec", "apiHealthCheck"],
        added_in: "servarr-operator v1.3.0",
    },
    Expectation {
        crd_name: "mediastacks.servarr.dev",
        cr_version: "v1alpha1",
        field_path: &["spec", "apiHealthCheck"],
        added_in: "servarr-operator v1.3.0",
    },
];

/// Best-effort, read-only startup diagnostic: warn (never fail startup) if an installed CRD
/// looks stale relative to this operator build. A missing CRD, an RBAC denial reading
/// `customresourcedefinitions`, or a network hiccup just skips the check for that CRD — this is
/// a diagnostic aid, not a startup gate.
pub async fn check(client: &Client) {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    for exp in EXPECTATIONS {
        match api.get(exp.crd_name).await {
            Ok(crd) => {
                if let Some(reason) = staleness_reason(exp, &crd) {
                    warn!(
                        crd = exp.crd_name,
                        "{reason} — upgrade servarr-crds before (or together with) the \
                         operator; see docs/installation.md"
                    );
                }
            }
            Err(e) => {
                warn!(
                    crd = exp.crd_name,
                    error = %kube_err_summary(&e),
                    "could not read installed CRD to run the startup schema self-check; skipping"
                );
            }
        }
    }
}

/// Returns a human-readable reason the installed CRD is stale relative to `exp`, or `None` if
/// it looks current. Pure function, no I/O — kept separate from `check` so it's testable without
/// a mock API server.
fn staleness_reason(exp: &Expectation, crd: &CustomResourceDefinition) -> Option<String> {
    let version = crd
        .spec
        .versions
        .iter()
        .find(|v| v.name == exp.cr_version)?;

    let schema = version
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref())?;

    if !has_field(schema, exp.field_path) {
        return Some(format!(
            "installed CRD is missing field `.{}` (added in {}); CRDs appear stale",
            exp.field_path.join("."),
            exp.added_in
        ));
    }

    None
}

fn has_field(root: &JSONSchemaProps, path: &[&str]) -> bool {
    let mut current = root;
    for segment in path {
        match current.properties.as_ref().and_then(|p| p.get(*segment)) {
            Some(next) => current = next,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
        CustomResourceDefinitionSpec, CustomResourceDefinitionVersion, CustomResourceValidation,
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

    const EXP: Expectation = Expectation {
        crd_name: "servarrapps.servarr.dev",
        cr_version: "v1alpha1",
        field_path: &["spec", "apiHealthCheck"],
        added_in: "servarr-operator v1.3.0",
    };

    #[test]
    fn up_to_date_crd_has_no_staleness_reason() {
        let crd = crd_with_schema(
            "v1alpha1",
            Some(schema_with_spec_properties(&["apiHealthCheck", "app"])),
        );
        assert!(staleness_reason(&EXP, &crd).is_none());
    }

    #[test]
    fn crd_missing_expected_field_is_stale() {
        let crd = crd_with_schema("v1alpha1", Some(schema_with_spec_properties(&["app"])));
        let reason = staleness_reason(&EXP, &crd).expect("expected staleness reason");
        assert!(reason.contains("spec.apiHealthCheck"));
        assert!(reason.contains("servarr-operator v1.3.0"));
    }

    #[test]
    fn crd_missing_expected_version_is_not_flagged_by_this_check() {
        let crd = crd_with_schema("v1alpha", Some(schema_with_spec_properties(&["app"])));
        // No v1alpha1 version at all: staleness_reason can't find the version to inspect, so it
        // falls through the `?` and returns None (mismatch reported elsewhere, e.g. reconcile
        // errors) — this test documents that behavior rather than asserting a stronger warning.
        assert!(staleness_reason(&EXP, &crd).is_none());
    }

    #[test]
    fn crd_with_no_schema_published_is_not_flagged() {
        let crd = crd_with_schema("v1alpha1", None);
        assert!(staleness_reason(&EXP, &crd).is_none());
    }
}
