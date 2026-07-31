use crate::client::ApiError;
use crate::k8s::{SecretError, kube_err_public_summary};

/// A message that is safe to surface to a tenant (status Conditions, Events).
///
/// This is the only type that can become a tenant-visible Condition message. It can only be
/// produced by an explicit, reviewable call to [`TenantSafeMessage::new`] or by one of the
/// [`From`] impls below, each of which routes through a sanitizer — a raw `kube::Error`,
/// `SecretError`, or `ApiError` cannot reach a Condition without being sanitized first.
///
/// ```compile_fail
/// use servarr_api::TenantSafeMessage;
/// // A raw String must not silently convert into a TenantSafeMessage:
/// let _ = TenantSafeMessage::from("raw untrusted string".to_string());
/// // Same for &str:
/// let _ = TenantSafeMessage::from("raw untrusted string");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantSafeMessage(String);

impl TenantSafeMessage {
    /// Construct a tenant-safe message from an explicit call site.
    ///
    /// This is the single un-sanitized construction path, reserved for:
    /// (a) static operator-authored text,
    /// (b) values the tenant owns (resource/secret names, namespace names, typed enum variants), or
    /// (c) output already produced by a sanitizer (`public_summary()` / `log_summary()` /
    ///     `kube_err_public_summary()`).
    ///
    /// Never pass the raw `Display` of a `kube::Error` / `reqwest::Error` / `ApiError`, an error
    /// carrying external content, or an API response body — those must go through a sanitizer and
    /// the corresponding [`From`] impl instead.
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }

    /// Wrap an already-sanitized string. Private so every `From` impl is an explicit,
    /// reviewable sanitizer call and a raw `String` can never `.into()` a `TenantSafeMessage`.
    fn from_sanitized(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for TenantSafeMessage {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantSafeMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<kube::Error> for TenantSafeMessage {
    fn from(e: kube::Error) -> Self {
        Self::from_sanitized(kube_err_public_summary(&e))
    }
}

impl From<SecretError> for TenantSafeMessage {
    fn from(e: SecretError) -> Self {
        Self::from_sanitized(e.public_summary())
    }
}

impl From<ApiError> for TenantSafeMessage {
    fn from(e: ApiError) -> Self {
        Self::from_sanitized(e.log_summary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_kube_api_error_keeps_status_code_not_body() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = kube::Error::Api(Box::new(status));
        let expected = kube_err_public_summary(&err);
        let msg = TenantSafeMessage::from(err);
        assert_eq!(msg.as_ref(), expected);
        assert!(
            msg.as_ref().contains("403"),
            "message should keep the status code: {msg}"
        );
        assert!(
            !msg.as_ref().contains("super-secret-name"),
            "message must not leak the API server's message: {msg}"
        );
    }

    #[test]
    fn from_secret_error_matches_public_summary() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = SecretError::Kube(kube::Error::Api(Box::new(status)));
        let expected = err.public_summary();
        let msg = TenantSafeMessage::from(err);
        assert_eq!(msg.as_ref(), expected);
        assert!(
            !msg.as_ref().contains("super-secret-name"),
            "message must not leak the API server's message: {msg}"
        );
    }

    #[test]
    fn from_api_error_matches_log_summary_and_drops_body() {
        let err = ApiError::ApiResponse {
            status: 401,
            body: "secret-leak-body".to_string(),
        };
        let expected = err.log_summary();
        let msg = TenantSafeMessage::from(err);
        assert_eq!(msg.as_ref(), expected);
        assert!(
            !msg.as_ref().contains("secret-leak-body"),
            "message must not contain the API response body: {msg}"
        );
    }
}
