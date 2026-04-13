use anyhow::Result;
use clap::{Parser, Subcommand};
use servarr_operator::{controller, media_stack_controller, server, telemetry, webhook};
use tracing::{error, info};

const METRICS_PORT: u16 = 8080;

#[derive(Parser)]
#[command(
    name = "servarr-operator",
    about = "Servarr Operator — Kubernetes operator for *arr media apps"
)]
struct Cli {
    /// Path to kubeconfig file. Overrides KUBECONFIG env var and ~/.kube/config.
    /// Ignored when running in-cluster.
    #[arg(long, value_name = "PATH")]
    kubeconfig: Option<std::path::PathBuf>,

    /// Kubeconfig context to use. Overrides current-context in the kubeconfig.
    /// Ignored when running in-cluster.
    #[arg(long, value_name = "NAME")]
    context: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the ServarrApp CRD YAML to stdout.
    Crd,
}

async fn build_client(
    kubeconfig: Option<std::path::PathBuf>,
    context: Option<String>,
) -> anyhow::Result<kube::Client> {
    if kubeconfig.is_none() && context.is_none() {
        return Ok(kube::Client::try_default().await?);
    }
    let options = kube::config::KubeConfigOptions {
        context,
        cluster: None,
        user: None,
    };
    let config = match kubeconfig {
        Some(path) => {
            let kb = kube::config::Kubeconfig::read_from(path)?;
            kube::Config::from_custom_kubeconfig(kb, &options).await?
        }
        None => kube::Config::from_kubeconfig(&options).await?,
    };
    Ok(kube::Client::try_from(config)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Crd) => {
            controller::print_crd()?;
            media_stack_controller::print_crd()?;
            return Ok(());
        }
        None => {}
    }

    let client = build_client(cli.kubeconfig, cli.context).await?;

    let state = server::ServerState::new();

    // Optionally start the webhook server if WEBHOOK_ENABLED=true
    let webhook_enabled = std::env::var("WEBHOOK_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if webhook_enabled {
        let webhook_config = webhook::WebhookConfig::default();
        info!(port = webhook_config.port, "webhook server enabled");
        let webhook_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = webhook::run(webhook_client, webhook_config).await {
                error!(%e, "webhook server failed");
            }
        });
    }

    // Run the metrics/health server and both controllers concurrently.
    // If any exits, shut down.
    let state2 = state.clone();
    tokio::select! {
        res = server::run(METRICS_PORT, state.clone()) => {
            error!("metrics server exited: {res:?}");
            res
        }
        res = controller::run(client.clone(), state) => {
            res
        }
        res = media_stack_controller::run(client, state2) => {
            res
        }
    }
}
