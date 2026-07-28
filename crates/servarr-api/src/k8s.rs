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
    /// Returns a log-safe summary that excludes internal Kubernetes API
    /// detail. `NoData`/`KeyNotFound`/`InvalidUtf8` are exempt: they name
    /// only the secret/key name the caller's own spec already specified
    /// (`apiKeySecret`, `adminCredentials.secretName`), so surfacing them
    /// verbatim leaks nothing. `Kube` is not — `kube::Error`'s `Display` can
    /// carry arbitrary API server response detail.
    pub fn log_summary(&self) -> String {
        match self {
            Self::Kube(_) => "Kubernetes API error".to_string(),
            Self::NoData { .. } | Self::KeyNotFound { .. } | Self::InvalidUtf8 { .. } => {
                self.to_string()
            }
        }
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
    fn log_summary_key_not_found_matches_display() {
        let err = SecretError::KeyNotFound {
            name: "my-secret".into(),
            key: "api-key".into(),
        };
        assert_eq!(err.log_summary(), err.to_string());
        assert!(err.log_summary().contains("my-secret"));
    }
}
