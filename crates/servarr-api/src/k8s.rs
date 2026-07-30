use k8s_openapi::api::core::v1::Secret;
use kube::Client;
use kube::api::Api;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("Secret {name} has no data")]
    NoData { name: String },
    #[error("Key {key} not found in secret {name}")]
    KeyNotFound { name: String, key: String },
    #[error("Value for key {key} in secret {name} is not valid UTF-8")]
    InvalidUtf8 { name: String, key: String },
}

impl SecretError {
    /// Returns a log-safe summary. The `Kube` variant delegates to [`kube_err_summary`]; the
    /// other variants already only carry curated secret/key names, never external response
    /// content, so their `Display` is safe as-is.
    pub fn log_summary(&self) -> String {
        match self {
            Self::Kube(e) => kube_err_summary(e),
            other => other.to_string(),
        }
    }
}

/// Returns a log-safe summary of a `kube::Error` that excludes the API server's free-text
/// message/reason, keeping only the HTTP status code when available.
///
/// `kube::Error::Api`'s `Status` can carry arbitrary API-server detail in `message`/`reason`
/// (resource names, RBAC denial text) — lower sensitivity than an upstream *arr app's response
/// body, but still infra detail that shouldn't land verbatim in a tenant-visible Condition. Every
/// other variant's `Display` is passed through unchanged: `kube::Error` gains variants across
/// minor versions, so matching a wildcard here keeps this function correct without needing to
/// track kube's variant list release to release.
///
/// **This is a log-only guarantee, not a tenant-facing one.** Several non-`Api` variants can
/// carry detail as sensitive as an API-server message: `Auth` (an exec-plugin credential failure
/// can embed a bearer token or the exec command/args), `Service`/`HyperError` (the API-server
/// endpoint URL and TLS subject/SAN), `InferConfig`/`InferKubeconfig` (kubeconfig file paths and
/// cluster URLs), and `SerdeError` (fragments of the raw API response body). For any caller whose
/// output reaches a tenant (admission rejection messages, status Conditions, Events), use
/// [`kube_err_public_summary`] instead.
pub fn kube_err_summary(e: &kube::Error) -> String {
    match e {
        kube::Error::Api(status) => format!("Kubernetes API error (status: {})", status.code),
        other => other.to_string(),
    }
}

/// Returns a tenant-safe summary of a `kube::Error`, for surfaces a tenant can read (admission
/// rejection messages, status Conditions, Events). Stricter than [`kube_err_summary`]: only the
/// `Api` variant's HTTP status code is exposed, and every other variant collapses to a fixed
/// generic string with no `Display` passthrough at all, since several non-`Api` variants can
/// carry secrets or infra endpoint detail (see `kube_err_summary`'s docs).
pub fn kube_err_public_summary(e: &kube::Error) -> String {
    match e {
        kube::Error::Api(status) => format!("Kubernetes API error (status: {})", status.code),
        _ => "Kubernetes client error".to_string(),
    }
}

/// Read a single key from a Kubernetes Secret.
///
/// The value is returned as a decoded UTF-8 string (Kubernetes stores
/// Secret data as base64-encoded bytes, but the kube client decodes
/// the base64 automatically).
pub async fn read_secret_key(
    client: &Client,
    namespace: &str,
    secret_name: &str,
    key: &str,
) -> Result<String, SecretError> {
    let api = Api::<Secret>::namespaced(client.clone(), namespace);
    let secret = api.get(secret_name).await?;

    let data = secret.data.ok_or_else(|| SecretError::NoData {
        name: secret_name.to_string(),
    })?;

    let bytes = data.get(key).ok_or_else(|| SecretError::KeyNotFound {
        name: secret_name.to_string(),
        key: key.to_string(),
    })?;

    String::from_utf8(bytes.0.clone()).map_err(|_| SecretError::InvalidUtf8 {
        name: secret_name.to_string(),
        key: key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_summary_kube_variant_drops_message_keeps_status_code() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = SecretError::Kube(kube::Error::Api(Box::new(status)));
        let summary = err.log_summary();
        assert!(
            summary.contains("403"),
            "summary should keep the status code: {summary}"
        );
        assert!(
            !summary.contains("super-secret-name"),
            "summary must not leak the raw API server message: {summary}"
        );
    }

    #[test]
    fn log_summary_no_data_passes_through_unchanged() {
        let err = SecretError::NoData {
            name: "my-secret".to_string(),
        };
        assert_eq!(err.log_summary(), err.to_string());
    }

    #[test]
    fn log_summary_key_not_found_passes_through_unchanged() {
        let err = SecretError::KeyNotFound {
            name: "my-secret".to_string(),
            key: "api-key".to_string(),
        };
        assert_eq!(err.log_summary(), err.to_string());
    }

    #[test]
    fn log_summary_invalid_utf8_passes_through_unchanged() {
        let err = SecretError::InvalidUtf8 {
            name: "my-secret".to_string(),
            key: "api-key".to_string(),
        };
        assert_eq!(err.log_summary(), err.to_string());
    }

    #[test]
    fn kube_err_summary_preserves_display_for_non_api_variants() {
        // `kube_err_summary` is a log-only guarantee: the wildcard arm keeps non-`Api` variants'
        // `Display` unchanged rather than tracking kube's variant list release to release. Some
        // of those variants (Auth, Service, InferConfig, SerdeError, ...) can carry detail as
        // sensitive as an API-server message — callers whose output reaches a tenant must use
        // `kube_err_public_summary` instead.
        let err = kube::Error::LinesCodecMaxLineLengthExceeded;
        let summary = kube_err_summary(&err);
        assert_eq!(summary, err.to_string());
    }

    #[test]
    fn kube_err_public_summary_keeps_status_code_for_api_variant() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = kube::Error::Api(Box::new(status));
        let summary = kube_err_public_summary(&err);
        assert!(
            summary.contains("403"),
            "summary should keep the status code: {summary}"
        );
        assert!(
            !summary.contains("super-secret-name"),
            "summary must not leak the raw API server message: {summary}"
        );
    }

    #[test]
    fn kube_err_public_summary_collapses_non_api_variants_with_no_display_passthrough() {
        // Unlike `kube_err_summary`, the tenant-facing summary must not pass through any
        // variant's `Display` — several non-`Api` variants (Auth, Service, InferConfig,
        // SerdeError) can carry secrets or infra endpoint detail.
        let err = kube::Error::LinesCodecMaxLineLengthExceeded;
        let summary = kube_err_public_summary(&err);
        assert_ne!(summary, err.to_string());
        assert!(!summary.to_lowercase().contains("line"));
    }
}
