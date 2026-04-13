use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("servarr_operator=info,kube=info")),
        )
        .with(fmt::layer().json())
        .init();
}
