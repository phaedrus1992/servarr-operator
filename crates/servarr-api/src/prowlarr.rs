use serde::{Deserialize, Serialize};

use crate::client::ApiError;

fn map_sdk_err<E: std::fmt::Debug>(e: E) -> ApiError {
    ApiError::ApiResponse {
        status: 0,
        body: format!("{e:?}"),
    }
}

/// Client for the Prowlarr v1 application management API.
///
/// Prowlarr manages indexer proxies ("applications") that sync indexers to
/// downstream *arr apps (Sonarr, Radarr, Lidarr). This client wraps the
/// prowlarr SDK crate.
#[derive(Debug, Clone)]
pub struct ProwlarrClient {
    config: prowlarr::apis::configuration::Configuration,
}

/// An application registration in Prowlarr.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrApp {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub sync_level: String,
    #[serde(default)]
    pub implementation: String,
    #[serde(default)]
    pub config_contract: String,
    #[serde(default)]
    pub fields: Vec<ProwlarrAppField>,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// A field in a Prowlarr application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrAppField {
    pub name: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

// --- Conversion helpers between our types and SDK types ---

fn sdk_to_app(r: prowlarr::models::ApplicationResource) -> ProwlarrApp {
    let fields = r
        .fields
        .and_then(|outer| outer)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| {
            let name = f.name?.unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let value = f.value.and_then(|v| v).unwrap_or(serde_json::Value::Null);
            Some(ProwlarrAppField { name, value })
        })
        .collect();

    let sync_level = r.sync_level.map(|s| s.to_string()).unwrap_or_default();

    let tags = r
        .tags
        .and_then(|outer| outer)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t as i64)
        .collect();

    ProwlarrApp {
        id: r.id.unwrap_or(0) as i64,
        name: r.name.and_then(|n| n).unwrap_or_default(),
        sync_level,
        implementation: r.implementation.and_then(|i| i).unwrap_or_default(),
        config_contract: r.config_contract.and_then(|c| c).unwrap_or_default(),
        fields,
        tags,
    }
}

fn app_to_sdk(app: &ProwlarrApp) -> prowlarr::models::ApplicationResource {
    let fields: Vec<prowlarr::models::Field> = app
        .fields
        .iter()
        .map(|f| {
            let mut field = prowlarr::models::Field::new();
            field.name = Some(Some(f.name.clone()));
            field.value = Some(Some(f.value.clone()));
            field
        })
        .collect();

    let sync_level = match app.sync_level.as_str() {
        "disabled" => Some(prowlarr::models::ApplicationSyncLevel::Disabled),
        "addOnly" => Some(prowlarr::models::ApplicationSyncLevel::AddOnly),
        "fullSync" => Some(prowlarr::models::ApplicationSyncLevel::FullSync),
        _ => Some(prowlarr::models::ApplicationSyncLevel::FullSync),
    };

    let tags: Vec<i32> = app.tags.iter().map(|&t| t as i32).collect();

    let mut resource = prowlarr::models::ApplicationResource::new();
    resource.id = if app.id != 0 {
        Some(app.id as i32)
    } else {
        None
    };
    resource.name = Some(Some(app.name.clone()));
    resource.sync_level = sync_level;
    resource.implementation = Some(Some(app.implementation.clone()));
    resource.config_contract = Some(Some(app.config_contract.clone()));
    resource.fields = Some(Some(fields));
    resource.tags = Some(Some(tags));
    resource
}

impl ProwlarrClient {
    /// Create a new Prowlarr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://prowlarr:9696`).
    /// `api_key` is sent as the `X-Api-Key` header.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let mut config = prowlarr::apis::configuration::Configuration::new();
        config.base_path = base_url.trim_end_matches('/').to_string();
        config.api_key = Some(prowlarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });
        Ok(Self { config })
    }

    /// GET `/api/v1/applications` — list all registered applications.
    pub async fn list_applications(&self) -> Result<Vec<ProwlarrApp>, ApiError> {
        prowlarr::apis::application_api::list_applications(&self.config)
            .await
            .map(|v| v.into_iter().map(sdk_to_app).collect())
            .map_err(map_sdk_err)
    }

    /// POST `/api/v1/applications` — add a new application.
    pub async fn add_application(&self, app: &ProwlarrApp) -> Result<ProwlarrApp, ApiError> {
        let resource = app_to_sdk(app);
        prowlarr::apis::application_api::create_applications(&self.config, None, Some(resource))
            .await
            .map(sdk_to_app)
            .map_err(map_sdk_err)
    }

    /// PUT `/api/v1/applications/{id}` — update an existing application.
    pub async fn update_application(
        &self,
        id: i64,
        app: &ProwlarrApp,
    ) -> Result<ProwlarrApp, ApiError> {
        let resource = app_to_sdk(app);
        prowlarr::apis::application_api::update_applications(
            &self.config,
            &id.to_string(),
            None,
            Some(resource),
        )
        .await
        .map(sdk_to_app)
        .map_err(map_sdk_err)
    }

    /// DELETE `/api/v1/applications/{id}` — remove an application.
    pub async fn delete_application(&self, id: i64) -> Result<(), ApiError> {
        prowlarr::apis::application_api::delete_applications(&self.config, id as i32)
            .await
            .map_err(map_sdk_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prowlarr_client_new_constructs() {
        let client = ProwlarrClient::new("http://localhost:9696", "test-key");
        assert!(client.is_ok());
    }

    #[test]
    fn sdk_to_app_maps_all_fields() {
        let mut resource = prowlarr::models::ApplicationResource::new();
        resource.id = Some(42);
        resource.name = Some(Some("My App".to_string()));
        resource.sync_level = Some(prowlarr::models::ApplicationSyncLevel::FullSync);
        resource.implementation = Some(Some("Sonarr".to_string()));
        resource.config_contract = Some(Some("SonarrSettings".to_string()));
        resource.tags = Some(Some(vec![1, 2, 3]));

        // Build a field
        let mut field = prowlarr::models::Field::new();
        field.name = Some(Some("baseUrl".to_string()));
        field.value = Some(Some(serde_json::json!("http://sonarr:8989")));
        resource.fields = Some(Some(vec![field]));

        let app = sdk_to_app(resource);

        assert_eq!(app.id, 42);
        assert_eq!(app.name, "My App");
        assert_eq!(app.sync_level, "fullSync");
        assert_eq!(app.implementation, "Sonarr");
        assert_eq!(app.config_contract, "SonarrSettings");
        assert_eq!(app.tags, vec![1, 2, 3]);
        assert_eq!(app.fields.len(), 1);
        assert_eq!(app.fields[0].name, "baseUrl");
        assert_eq!(app.fields[0].value, serde_json::json!("http://sonarr:8989"));
    }

    #[test]
    fn sdk_to_app_handles_none_fields() {
        let resource = prowlarr::models::ApplicationResource::new();
        let app = sdk_to_app(resource);

        assert_eq!(app.id, 0);
        assert_eq!(app.name, "");
        assert_eq!(app.sync_level, "");
        assert_eq!(app.implementation, "");
        assert_eq!(app.config_contract, "");
        assert!(app.fields.is_empty());
        assert!(app.tags.is_empty());
    }

    #[test]
    fn sdk_to_app_filters_empty_field_names() {
        let mut resource = prowlarr::models::ApplicationResource::new();
        let mut empty_field = prowlarr::models::Field::new();
        empty_field.name = Some(Some(String::new()));
        empty_field.value = Some(Some(serde_json::json!("ignored")));

        let mut valid_field = prowlarr::models::Field::new();
        valid_field.name = Some(Some("apiKey".to_string()));
        valid_field.value = Some(Some(serde_json::json!("secret")));

        resource.fields = Some(Some(vec![empty_field, valid_field]));
        let app = sdk_to_app(resource);

        assert_eq!(app.fields.len(), 1);
        assert_eq!(app.fields[0].name, "apiKey");
    }

    #[test]
    fn app_to_sdk_maps_all_fields() {
        let app = ProwlarrApp {
            id: 10,
            name: "Test App".to_string(),
            sync_level: "fullSync".to_string(),
            implementation: "Radarr".to_string(),
            config_contract: "RadarrSettings".to_string(),
            fields: vec![ProwlarrAppField {
                name: "baseUrl".to_string(),
                value: serde_json::json!("http://radarr:7878"),
            }],
            tags: vec![5, 10],
        };

        let resource = app_to_sdk(&app);

        assert_eq!(resource.id, Some(10));
        assert_eq!(resource.name, Some(Some("Test App".to_string())));
        assert_eq!(
            resource.sync_level,
            Some(prowlarr::models::ApplicationSyncLevel::FullSync)
        );
        assert_eq!(resource.implementation, Some(Some("Radarr".to_string())));
        assert_eq!(
            resource.config_contract,
            Some(Some("RadarrSettings".to_string()))
        );
        assert_eq!(resource.tags, Some(Some(vec![5, 10])));

        let fields = resource.fields.unwrap().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, Some(Some("baseUrl".to_string())));
        assert_eq!(
            fields[0].value,
            Some(Some(serde_json::json!("http://radarr:7878")))
        );
    }

    #[test]
    fn app_to_sdk_zero_id_becomes_none() {
        let app = ProwlarrApp {
            id: 0,
            name: "New App".to_string(),
            sync_level: "addOnly".to_string(),
            implementation: String::new(),
            config_contract: String::new(),
            fields: vec![],
            tags: vec![],
        };

        let resource = app_to_sdk(&app);
        assert_eq!(resource.id, None);
        assert_eq!(
            resource.sync_level,
            Some(prowlarr::models::ApplicationSyncLevel::AddOnly)
        );
    }

    #[test]
    fn app_to_sdk_disabled_sync_level() {
        let app = ProwlarrApp {
            id: 1,
            name: "App".to_string(),
            sync_level: "disabled".to_string(),
            implementation: String::new(),
            config_contract: String::new(),
            fields: vec![],
            tags: vec![],
        };

        let resource = app_to_sdk(&app);
        assert_eq!(
            resource.sync_level,
            Some(prowlarr::models::ApplicationSyncLevel::Disabled)
        );
    }

    #[test]
    fn app_to_sdk_unknown_sync_level_defaults_to_full_sync() {
        let app = ProwlarrApp {
            id: 1,
            name: "App".to_string(),
            sync_level: "unknown_value".to_string(),
            implementation: String::new(),
            config_contract: String::new(),
            fields: vec![],
            tags: vec![],
        };

        let resource = app_to_sdk(&app);
        assert_eq!(
            resource.sync_level,
            Some(prowlarr::models::ApplicationSyncLevel::FullSync)
        );
    }
}
