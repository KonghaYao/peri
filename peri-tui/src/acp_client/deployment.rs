//! One exit path for embedded ACP deployments, including failed startup/prompt operations.

use std::future::Future;

use peri_acp::host::{AcpHostHandle, AcpHostShutdownReport};

use super::AcpTuiClient;

/// Non-Clone deployment handle; client clones remain transport clients, not host task owners.
pub struct AcpDeployment {
    client: AcpTuiClient,
    host: AcpHostHandle,
}

impl AcpDeployment {
    pub fn new(client: AcpTuiClient, host: AcpHostHandle) -> Self {
        Self { client, host }
    }

    pub async fn shutdown(&mut self) -> AcpHostShutdownReport {
        self.client.close();
        self.host.shutdown().await
    }

    /// All ordinary Result exits close transport and join the same host task.
    /// Cancelling this future retains the host in this deployment for a later shutdown call.
    pub async fn run<T>(
        &mut self,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        finish_operation(operation, &self.client, self.host.shutdown()).await
    }
}

impl Drop for AcpDeployment {
    fn drop(&mut self) {
        self.client.close();
    }
}

async fn finish_operation<T>(
    operation: impl Future<Output = anyhow::Result<T>>,
    client: &AcpTuiClient,
    shutdown: impl Future<Output = AcpHostShutdownReport>,
) -> anyhow::Result<T> {
    let result = operation.await;
    client.close();
    let report = shutdown.await;
    match report {
        AcpHostShutdownReport::Complete => result,
        AcpHostShutdownReport::TelemetryFailed(report) => {
            // Telemetry remains an observer: report its fixed terminal failure,
            // but do not turn a successful prompt into a business error.
            tracing::warn!(?report, "embedded ACP telemetry shutdown failed");
            result
        }
        report @ (AcpHostShutdownReport::Incomplete | AcpHostShutdownReport::TaskFailed { .. }) => {
            let context = format!("ACP deployment shutdown did not complete: {report:?}");
            match result {
                Ok(_) => Err(anyhow::anyhow!(context)),
                Err(error) => Err(error.context(context)),
            }
        }
    }
}

#[cfg(test)]
#[path = "deployment_test.rs"]
mod tests;
