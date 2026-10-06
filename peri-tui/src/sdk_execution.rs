use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::acp_client::AcpTuiClient;
use anyhow::{Context, Result};
use peri_acp::host::execution_admission_jsonl::{JsonlSdkDispatcher, SdkDispatcherLaunch};
use peri_acp::transport::RequestTransport;

struct AcpExecutionReverse(AcpTuiClient);

#[async_trait::async_trait]
impl RequestTransport for AcpExecutionReverse {
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, peri_acp::transport::types::AcpError> {
        self.0.send_raw_request(method, params).await
    }
}

pub async fn launch_sdk_dispatcher() -> Result<Arc<dyn RequestTransport>> {
    launch_with_reverse(None).await
}

pub async fn launch_sdk_dispatcher_for_client(
    client: AcpTuiClient,
) -> Result<Arc<dyn RequestTransport>> {
    launch_with_reverse(Some(Arc::new(AcpExecutionReverse(client)))).await
}

async fn launch_with_reverse(
    reverse: Option<Arc<dyn RequestTransport>>,
) -> Result<Arc<dyn RequestTransport>> {
    let home = dirs_next::home_dir()
        .context("SDK persistent execution registry needs a home directory")?;
    let directory = home.join(".peri/execution");
    tokio::fs::create_dir_all(&directory).await?;
    let executable = trusted_bun_path()
        .context("Bun runtime required for SDK execution admission; no Rust scheduler fallback")?;
    let module = trusted_sdk_module()
        .context("SDK execution dispatcher module unavailable; no Rust scheduler fallback")?;
    Ok(Arc::new(
        JsonlSdkDispatcher::launch_with_reverse_transport(
            SdkDispatcherLaunch {
                executable,
                module,
                database: directory.join("registry.db"),
                instance_id: uuid::Uuid::now_v7().to_string(),
                generation_id: uuid::Uuid::now_v7().to_string(),
            },
            reverse,
        )
        .await?,
    ))
}

fn trusted_bun_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(if cfg!(windows) { "bun.exe" } else { "bun" }))
        .find(|candidate| candidate.is_file())
}

fn trusted_sdk_module() -> Option<PathBuf> {
    let installed = std::env::current_exe()
        .ok()?
        .parent()?
        .join("peri-sdk/execution/sidecar.js");
    let development = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("npm-packages/@peri-sdk/src/execution/sidecar.ts");
    [installed, development]
        .into_iter()
        .find(|candidate| candidate.is_absolute() && candidate.is_file())
}

#[cfg(test)]
#[path = "sdk_execution_test.rs"]
mod tests;
