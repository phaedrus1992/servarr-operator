use anyhow::Result;
use clap::{Parser, Subcommand};
use servarr_operator::{
    context, controller, crd_check, env, media_stack_controller, server, telemetry, webhook,
};
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

    // Best-effort startup diagnostic: warn if the installed CRDs are stale relative to this
    // operator build (#543). Never blocks startup. Cluster-scoped installs only — the RBAC
    // this needs (get on cluster-scoped CustomResourceDefinitions) is granted on the
    // ClusterRole, and a namespace-scoped Role can never grant it, so skip rather than log a
    // permanent Forbidden warning on every namespace-scoped startup.
    if context::watch_all_namespaces() {
        crd_check::check(&client).await;
    }

    let state = server::ServerState::new();

    // Optionally start the webhook server if WEBHOOK_ENABLED=true.
    //
    // #732: a value like `on` or `y` expresses an intent to enable the webhook. Treating it as
    // `false` disables validating admission, and the only other signal is the absence of a log
    // line. Refuse to start instead, and say which variable is wrong.
    let webhook_enabled = env::var_bool_strict("WEBHOOK_ENABLED", false)?;

    let webhook_config = if webhook_enabled {
        let config = webhook::WebhookConfig::from_env()?;
        info!(port = config.port, "webhook server enabled");
        Some(config)
    } else {
        None
    };

    // The operator was told to run a webhook, so a webhook that cannot run is fatal.
    //
    // `from_env` proves the TLS variables name paths. It cannot prove those paths hold a
    // loadable certificate, and it cannot bind the port — both happen inside `webhook::run`.
    // A detached task that only logs its error leaves `/readyz` at 200 and the pod Ready, while
    // `failurePolicy: Fail` rejects every ServarrApp write in the cluster. Restarting with a
    // stated reason beats a healthy-looking pod that blocks the whole cluster (#733).
    let webhook_client = client.clone();
    let webhook = async move {
        match webhook_config {
            Some(config) => webhook::run(webhook_client, config).await,
            // The webhook is disabled, so this branch must never win the select below.
            None => std::future::pending().await,
        }
    };

    // Run the metrics/health server, the webhook, and both controllers concurrently.
    // If any exits, shut down.
    let state2 = state.clone();
    tokio::select! {
        res = webhook => {
            error!("webhook server exited: {res:?}");
            res
        }
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
