//! `peri-swarm` process entry (S6b).
//!
//! Parses the deployment environment, opens the remote session store, and runs
//! the gateway plus the Rivet registry until SIGINT. A missing or invalid remote
//! configuration is a startup failure; the process never degrades to a local
//! SQLite store.
//!
//! OpenTelemetry is initialized first. The guard installs the process
//! subscriber (or keeps the pre-telemetry fmt subscriber when OTLP is unset) and
//! stays alive until `start()` has drained the gateway and closed both stores,
//! so the final flush happens after the store close.

use std::process::ExitCode;

use peri_swarm::bootstrap::{run_with_telemetry, BootstrapConfig};
use peri_swarm::telemetry::{self, HttpTelemetry};

#[tokio::main]
async fn main() -> ExitCode {
    let telemetry = telemetry::init();
    match start(telemetry.http_telemetry()).await {
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

async fn start(
    telemetry: Option<HttpTelemetry>,
) -> Result<(), peri_swarm::bootstrap::BootstrapError> {
    let config = BootstrapConfig::from_env()?;
    run_with_telemetry(config, telemetry).await
}
