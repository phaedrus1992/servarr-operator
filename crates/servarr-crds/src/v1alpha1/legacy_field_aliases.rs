//! Generic post-processing for generated CRD schemas: duplicate an object-typed field's schema
//! under a legacy alias key wherever it appears, at any nesting depth.
//!
//! `spec.rs`'s `LEGACY_APP_TYPE_ALIASES` handles enum-value aliases via a custom `JsonSchema`
//! impl on `AppType` itself -- that works because the impl fully controls what `AppType`
//! contributes wherever it's nested. It does not work for an object-typed field like
//! `seerr_sync`: what's missing there is a second sibling *key* (`overseerrSync`) in the parent
//! struct's `properties` map, and no single field's `#[schemars(schema_with = ...)]` can add a
//! key beside itself. Instead, this walks the fully generated schema after `T::crd()` builds it
//! and duplicates every known aliased key's schema under its legacy name, recursively -- which
//! covers a field at any nesting depth (a struct's own field, or a field inside an array item's
//! struct) with one mechanism (#545).

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, JSONSchemaProps, JSONSchemaPropsOrArray,
};
use kube::CustomResourceExt;

/// `(current wire name, legacy wire name)` pairs for object-typed fields carrying a
/// `#[serde(alias = "...")]` that schemars cannot fold into the generated schema on its own.
/// Append here whenever an object-typed field gains a legacy alias.
const LEGACY_FIELD_ALIASES: &[(&str, &str)] = &[("seerrSync", "overseerrSync")];

/// Build `T`'s CRD the normal way, then duplicate every aliased property's schema under its
/// legacy key throughout the schema tree. Use this instead of calling `T::crd()` directly
/// wherever a CRD is generated for real use (printing, chart generation).
pub fn crd_with_legacy_field_aliases<T: CustomResourceExt>() -> CustomResourceDefinition {
    let mut crd = T::crd();
    for version in &mut crd.spec.versions {
        if let Some(schema) = version
            .schema
            .as_mut()
            .and_then(|s| s.open_api_v3_schema.as_mut())
        {
            apply_legacy_field_aliases(schema);
        }
    }
    crd
}

/// Recursively duplicate every aliased property's schema under its legacy key, in place,
/// throughout the whole schema tree (nested objects and array items).
fn apply_legacy_field_aliases(schema: &mut JSONSchemaProps) {
    if let Some(properties) = schema.properties.as_mut() {
        for (new_key, legacy_key) in LEGACY_FIELD_ALIASES {
            if let Some(value) = properties.get(*new_key).cloned() {
                properties.entry((*legacy_key).to_string()).or_insert(value);
            }
        }
        for prop_schema in properties.values_mut() {
            apply_legacy_field_aliases(prop_schema);
        }
    }
    match schema.items.as_mut() {
        Some(JSONSchemaPropsOrArray::Schema(item)) => apply_legacy_field_aliases(item),
        Some(JSONSchemaPropsOrArray::Schemas(items)) => {
            for item in items {
                apply_legacy_field_aliases(item);
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_with_properties(properties: Vec<(&str, JSONSchemaProps)>) -> JSONSchemaProps {
        JSONSchemaProps {
            properties: Some(
                properties
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn duplicates_top_level_aliased_property() {
        let inner = JSONSchemaProps {
            type_: Some("object".to_string()),
            ..Default::default()
        };
        let mut schema = schema_with_properties(vec![("seerrSync", inner.clone())]);

        apply_legacy_field_aliases(&mut schema);

        let props = schema.properties.expect("properties should be set");
        assert!(props.contains_key("seerrSync"));
        assert_eq!(props.get("overseerrSync"), Some(&inner));
    }

    #[test]
    fn duplicates_aliased_property_nested_inside_array_items() {
        let inner = JSONSchemaProps {
            type_: Some("object".to_string()),
            ..Default::default()
        };
        let item_schema = schema_with_properties(vec![("seerrSync", inner.clone())]);
        let mut schema = JSONSchemaProps {
            type_: Some("array".to_string()),
            items: Some(JSONSchemaPropsOrArray::Schema(Box::new(item_schema))),
            ..Default::default()
        };

        apply_legacy_field_aliases(&mut schema);

        let item_props = match schema.items.expect("items should be set") {
            JSONSchemaPropsOrArray::Schema(s) => {
                s.properties.expect("item properties should be set")
            }
            JSONSchemaPropsOrArray::Schemas(_) => panic!("expected a single item schema"),
        };
        assert!(item_props.contains_key("overseerrSync"));
    }

    #[test]
    fn does_not_add_alias_when_field_absent() {
        let mut schema = schema_with_properties(vec![("unrelated", JSONSchemaProps::default())]);

        apply_legacy_field_aliases(&mut schema);

        let props = schema.properties.expect("properties should be set");
        assert!(!props.contains_key("overseerrSync"));
    }

    #[test]
    fn does_not_overwrite_existing_legacy_key() {
        let inner = JSONSchemaProps {
            type_: Some("object".to_string()),
            ..Default::default()
        };
        let sentinel = JSONSchemaProps {
            type_: Some("string".to_string()),
            ..Default::default()
        };
        let mut schema = schema_with_properties(vec![
            ("seerrSync", inner),
            ("overseerrSync", sentinel.clone()),
        ]);

        apply_legacy_field_aliases(&mut schema);

        let props = schema.properties.expect("properties should be set");
        assert_eq!(props.get("overseerrSync"), Some(&sentinel));
    }
}
