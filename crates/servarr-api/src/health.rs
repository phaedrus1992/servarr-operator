use crate::client::ApiError;

/// Uniform health-check interface for all API clients.
///
/// Each client implements this by calling its respective health or status
/// endpoint. The operator uses this to report application readiness.
pub trait HealthCheck: Send + Sync {
    /// Returns `Ok(true)` if the application is healthy, `Ok(false)` if it
    /// responded but reported an unhealthy state, or `Err` on connection failure.
    fn is_healthy(&self) -> impl std::future::Future<Output = Result<bool, ApiError>> + Send;
}
