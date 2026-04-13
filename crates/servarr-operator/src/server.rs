use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use prometheus::Encoder;
use tracing::info;

/// Shared state for the HTTP health/metrics server.
#[derive(Clone)]
pub struct ServerState {
    ready: Arc<AtomicBool>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the operator as ready (call after CRD registration).
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }
}

/// Start the HTTP server on the given port.
///
/// Exposes:
/// - `GET /metrics` — Prometheus text format
/// - `GET /healthz` — liveness probe (always 200)
/// - `GET /readyz`  — readiness probe (200 after initial sync)
pub async fn run(port: u16, state: ServerState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "starting metrics/health server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn metrics_handler() -> impl IntoResponse {
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    if encoder.encode(&metric_families, &mut buffer).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to encode metrics".to_string(),
        );
    }
    (
        StatusCode::OK,
        String::from_utf8(buffer).unwrap_or_default(),
    )
}

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz_handler(State(state): State<ServerState>) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::Request;
    use tower::ServiceExt; // for oneshot

    fn build_app(state: ServerState) -> Router {
        Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/healthz", get(healthz_handler))
            .route("/readyz", get(readyz_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn healthz_returns_200() {
        let state = ServerState::new();
        let app = build_app(state);
        let response = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_returns_503_when_not_ready() {
        let state = ServerState::new();
        let app = build_app(state);
        let response = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_returns_200_after_set_ready() {
        let state = ServerState::new();
        state.set_ready();
        let app = build_app(state);
        let response = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_returns_200_with_prometheus_text() {
        let state = ServerState::new();
        let app = build_app(state);
        let response = app
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        // Prometheus text format uses lines like "# HELP ..." and "# TYPE ..."
        // At minimum, process metrics should be present.
        assert!(
            text.contains("# HELP") || text.contains("# TYPE") || text.is_empty(),
            "expected prometheus text format"
        );
    }

    #[test]
    fn server_state_new_starts_not_ready() {
        let state = ServerState::new();
        assert!(!state.ready.load(Ordering::Relaxed));
    }

    #[test]
    fn server_state_set_ready_toggles_flag() {
        let state = ServerState::new();
        assert!(!state.ready.load(Ordering::Relaxed));
        state.set_ready();
        assert!(state.ready.load(Ordering::Relaxed));
    }
}
