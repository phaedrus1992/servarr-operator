use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_FILTER: &str = "servarr_operator=info,kube=info";

pub fn init() {
    // The warning cannot be emitted here. The subscriber that carries it does not exist until
    // `.init()` runs below, so hold the error and log it after the subscriber is live.
    let (filter, rejected) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(_) if std::env::var_os("RUST_LOG").is_none() => (EnvFilter::new(DEFAULT_FILTER), None),
        Err(e) => (EnvFilter::new(DEFAULT_FILTER), Some(e.to_string())),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();

    if let Some(error) = rejected {
        tracing::warn!(
            %error,
            default_filter = DEFAULT_FILTER,
            "RUST_LOG is set but unusable, using the default filter"
        );
    }
}
