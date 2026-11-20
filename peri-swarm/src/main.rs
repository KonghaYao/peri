//! `peri-swarm` process entry (S6b).
//!
//! Parses the deployment environment, opens the remote session store, and runs
//! the gateway plus the Rivet registry until SIGINT. A missing or invalid remote
//! configuration is a startup failure; the process never degrades to a local
//! SQLite store.

use std::process::ExitCode;

use peri_swarm::bootstrap::{run, BootstrapConfig};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match start().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // stderr is not filtered by RUST_LOG: a failed start must stay
            // visible even when logging is turned down.
            eprintln!("peri-swarm: {error}");
            tracing::error!(error = %error, "peri-swarm failed");
            ExitCode::FAILURE
        }
    }
}

async fn start() -> Result<(), peri_swarm::bootstrap::BootstrapError> {
    let config = BootstrapConfig::from_env()?;
    run(config).await
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("peri_swarm=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
