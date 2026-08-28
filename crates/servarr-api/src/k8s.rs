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

    /// Returns a tenant-safe summary. The `Kube` variant delegates to
    /// [`kube_err_public_summary`]; the other variants are safe as-is (see [`Self::log_summary`]).
    pub fn public_summary(&self) -> String {
        match self {
            Self::Kube(e) => kube_err_public_summary(e),
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
        // Operator log only — never tenant-visible. TenantSafeMessage routes through
        // kube_err_public_summary, which collapses non-Api variants to a generic string.
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
        kube::Error::Api(_) => kube_err_summary(e),
        _ => "Kubernetes client error".to_string(),
    }
}

/// Returns `true` when `e` means the API server reports the object as not found.
///
/// Checks `status.code == 404` **or** [`kube::core::Status::is_not_found`]. Neither alone is
/// enough: `kube-core`'s `is_not_found()` is effectively `reason == "NotFound"` — its
/// `code == 404` fallback only fires when `reason` isn't one of the library's known reason
/// strings, which "NotFound" always is, so a `Status` carrying `code: 404` with no `reason` set
/// (the plain, code-only case this predicate always matched, and what this crate's own error
/// paths and test doubles construct) would stop matching if this delegated to `is_not_found()`
/// alone. Checking `code == 404` directly keeps that case working; `is_not_found()` on top
/// additionally catches a `Status` that carries the reason but not the numeric code.
///
/// Shared low-level predicate: `servarr-operator`'s finalizer-cleanup path
/// (`ClassifyCleanupSeverity`) and its ordinary reconcile-path get-or-create/optional-skip
/// checks both need "was this not-found", but only the cleanup path also needs the
/// Terminal/Transient retry duality built on top of it. This function is the shared core; each
/// caller decides what a not-found result *means* for its own control flow.
pub fn is_kube_not_found(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(status) if status.code == 404 || status.is_not_found())
}

/// Returns `true` when `e` means the request was rejected for who's asking, not what's asked --
/// a `401`/`403` API response, or a `kube::Error::Auth` (credential/exec-plugin) failure that
/// never even reached the API server to get a status code. Neither self-resolves on retry the
/// way a 5xx/network blip can; both need a human to fix a credential or RBAC grant.
///
/// Shared low-level predicate: `servarr-operator`'s orphan-cleanup PVC-detach path
/// (`DetachFailureCause`) and its finalizer-cleanup path (`ClassifyCleanupSeverity`'s
/// `RetryOutlook`) both need this exact "is this a permission problem" check, on top of which
/// each caller builds its own richer classification (the finalizer-cleanup path also treats
/// several deterministic non-`Api` variants -- `SerdeError`, `BuildRequest`, TLS/kubeconfig
/// setup, API discovery -- as needing a manual fix; the PVC-detach path doesn't, since a simple
/// PATCH call realistically only ever produces an `Api` or `Auth` error).
pub fn is_kube_permission_denied(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(status) if status.code == 401 || status.code == 403)
        || matches!(e, kube::Error::Auth(_))
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
    use crate::test_support::{SEED_TOKEN, is_tenant_safe_charset};
    use proptest::prelude::*;

    /// A `kube::Error::Api` whose `Status` message/reason carry `seed`, the way a
    /// real API-server message would (RBAC denial text, resource names).
    fn api_error_with_seeded_status(code: u16, seed: &str) -> kube::Error {
        kube::Error::Api(Box::new(kube::core::Status {
            code,
            message: format!("secrets \"{seed}\" is forbidden: User cannot get"),
            reason: format!("Forbidden: {seed}"),
            ..Default::default()
        }))
    }

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
        assert!(err.log_summary().contains("my-secret"));
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
    }

    #[test]
    fn public_summary_kube_variant_drops_message_keeps_status_code() {
        let status = kube::core::Status {
            code: 403,
            message: "secrets \"super-secret-name\" is forbidden: User cannot get".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = SecretError::Kube(kube::Error::Api(Box::new(status)));
        let summary = err.public_summary();
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
    fn public_summary_non_kube_variant_passes_through_unchanged() {
        let err = SecretError::NoData {
            name: "my-secret".to_string(),
        };
        assert_eq!(err.public_summary(), err.to_string());
    }

    proptest! {
        // The tenant-safe kube summary must keep the HTTP status code but never
        // reproduce the API server's free-text message/reason, which can carry
        // resource names and RBAC denial text.
        #[test]
        fn kube_err_public_summary_keeps_status_code_and_hides_seeded_message(
            code in any::<u16>(),
            seed in any::<String>(),
        ) {
            let seed = format!("{SEED_TOKEN}{seed}");
            let err = api_error_with_seeded_status(code, &seed);
            let summary = kube_err_public_summary(&err);
            prop_assert!(
                summary.contains(&code.to_string()),
                "status code must be preserved: {summary}"
            );
            prop_assert!(
                !summary.contains(&seed),
                "seeded API-server message leaked into summary: {summary}"
            );
            prop_assert!(is_tenant_safe_charset(&summary));
        }

        // `SecretError::public_summary` routes the `Kube` variant through
        // `kube_err_public_summary`: the seeded API-server message must not leak
        // while the status code is preserved.
        #[test]
        fn secret_error_public_summary_kube_variant_hides_seeded_message(
            code in any::<u16>(),
            seed in any::<String>(),
        ) {
            let seed = format!("{SEED_TOKEN}{seed}");
            let err = SecretError::Kube(api_error_with_seeded_status(code, &seed));
            let summary = err.public_summary();
            prop_assert!(summary.contains(&code.to_string()));
            prop_assert!(
                !summary.contains(&seed),
                "seeded API-server message leaked into summary: {summary}"
            );
            prop_assert!(is_tenant_safe_charset(&summary));
        }

        // The non-`Kube` `SecretError` variants carry only curated secret/key names
        // (operator-supplied, never external response content), so `public_summary`
        // passes their `Display` through unchanged — the summary is exactly the
        // curated name/key wrapped in the fixed format string, and nothing else can
        // be smuggled in alongside. Names/keys are constrained to the charset the
        // real K8s objects allow, matching how these variants are actually produced.
        #[test]
        fn secret_error_public_summary_non_kube_variants_pass_through_unchanged(
            name in "[a-z0-9._-]{1,32}",
            key in "[a-zA-Z0-9._-]{1,32}",
        ) {
            let errs = [
                SecretError::NoData {
                    name: name.clone(),
                },
                SecretError::KeyNotFound {
                    name: name.clone(),
                    key: key.clone(),
                },
                SecretError::InvalidUtf8 {
                    name: name.clone(),
                    key: key.clone(),
                },
            ];
            for err in errs {
                let summary = err.public_summary();
                prop_assert!(summary == err.to_string());
                prop_assert!(
                    summary.contains(&name),
                    "curated secret name must be exposed: {summary}"
                );
                prop_assert!(is_tenant_safe_charset(&summary));
            }
        }
    }

    // Every non-`Api` kube error variant must collapse to the fixed generic
    // string, even the ones carrying detail as sensitive as an API-server
    // message: `SerdeError` fragments the raw response body, `Auth` exec
    // failures embed the exec command/args, and `LinesCodecMaxLineLengthExceeded`
    // is a unit variant. The seed reaches each error's content, so a regression
    // to `Display` passthrough would leak it and fail the assertion. The output
    // is constant, so a single fixed seed exercises the same collapse guarantee
    // the property loop would.
    #[test]
    fn is_kube_not_found_true_for_404_api_error() {
        let err = kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            ..Default::default()
        }));
        assert!(is_kube_not_found(&err));
    }

    #[test]
    fn is_kube_not_found_false_for_non_404_api_error() {
        for code in [400, 403, 409, 500, 503] {
            let err = kube::Error::Api(Box::new(kube::core::Status {
                code,
                ..Default::default()
            }));
            assert!(
                !is_kube_not_found(&err),
                "status {code} must not be treated as not-found"
            );
        }
    }

    #[test]
    fn is_kube_not_found_false_for_non_api_variant() {
        assert!(!is_kube_not_found(
            &kube::Error::LinesCodecMaxLineLengthExceeded
        ));
    }

    #[test]
    fn is_kube_not_found_true_for_reason_only_not_found_with_no_404_code() {
        // A Status can carry reason == "NotFound" without the numeric code set (code is
        // suggested, not guaranteed) — this must still count as not-found.
        let err = kube::Error::Api(Box::new(kube::core::Status {
            code: 0,
            reason: "NotFound".to_string(),
            ..Default::default()
        }));
        assert!(is_kube_not_found(&err));
    }

    #[test]
    fn is_kube_permission_denied_true_for_401_and_403() {
        for code in [401, 403] {
            let err = kube::Error::Api(Box::new(kube::core::Status {
                code,
                ..Default::default()
            }));
            assert!(
                is_kube_permission_denied(&err),
                "status {code} should be treated as permission-denied"
            );
        }
    }

    #[test]
    fn is_kube_permission_denied_false_for_other_status_codes() {
        for code in [400, 404, 409, 500, 503] {
            let err = kube::Error::Api(Box::new(kube::core::Status {
                code,
                ..Default::default()
            }));
            assert!(
                !is_kube_permission_denied(&err),
                "status {code} must not be treated as permission-denied"
            );
        }
    }

    #[test]
    fn is_kube_permission_denied_true_for_auth_variant() {
        let err = kube::Error::Auth(kube::client::AuthError::AuthExecRun {
            cmd: "kubectl".into(),
            status: std::process::ExitStatus::default(),
            out: std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        });
        assert!(is_kube_permission_denied(&err));
    }

    #[test]
    fn kube_err_public_summary_collapses_non_api_variants() {
        let seed = SEED_TOKEN;
        // A NUL byte is never valid JSON, so the parse always fails and the
        // resulting serde error carries only position info.
        let serde_err = serde_json::from_str::<serde_json::Value>(&format!("{seed}\u{0}"))
            .expect_err("NUL is never valid JSON");
        let errs = vec![
            kube::Error::SerdeError(serde_err),
            kube::Error::LinesCodecMaxLineLengthExceeded,
            kube::Error::Auth(kube::client::AuthError::AuthExecRun {
                cmd: format!("kubectl --token={seed}"),
                status: std::process::ExitStatus::default(),
                out: std::process::Output {
                    status: std::process::ExitStatus::default(),
                    stdout: seed.as_bytes().to_vec(),
                    stderr: Vec::new(),
                },
            }),
        ];
        for err in errs {
            let summary = kube_err_public_summary(&err);
            assert_eq!(summary, "Kubernetes client error");
            assert!(!summary.contains(seed));
            assert!(is_tenant_safe_charset(&summary));
        }
    }
}
