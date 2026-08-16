use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use super::{FlowConfig, ReceiverFlow, SenderFlow, run_receiver, run_sender};
use crate::sync::device::{DeviceId, DevicePublic, TrustedPeers};
use crate::sync::device_cli;
use crate::sync::http_client::{self, ReqwestClient};
use crate::sync::keystore::SecretStore;
use crate::sync::protocol::SyncItems;

// ─── CLI 壳（读 stdin 的密码/码输入）───────────────────────────────────────

/// `peri sync send --to <device_id>` 入口。
pub async fn run_send_cli(
    server: &str,
    keystore_path: Option<&Path>,
    target_id: &str,
) -> Result<()> {
    http_client::validate_server_url(server, false)?;
    let client = ReqwestClient::new(server)?;
    let backoff = http_client::ExponentialBackoff;
    let (local, store, peers) = load_cli_identity(keystore_path)?;
    let target_id = DeviceId::from_b64(target_id)?;
    let target = peers
        .get(&target_id)
        .ok_or_else(|| anyhow::anyhow!("device {target_id} is not in trusted peers"))?;
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    let cwd = std::env::current_dir()?;
    let items = all_items();
    let cfg = FlowConfig::default();
    let flow = SenderFlow {
        client: &client,
        backoff: &backoff,
        store: store.as_ref(),
        local: &local,
        target,
        home_dir: &home,
        cwd: &cwd,
        items: &items,
        cfg: &cfg,
    };
    let outcome = run_sender(flow, |code, _epoch, remaining| {
        print!(
            "\rSync code: {}   (rotates in {remaining}s)   ",
            code.display()
        );
        let _ = std::io::stdout().flush();
    })
    .await?;
    println!();
    println!("Sent {} parts", outcome.parts_uploaded);
    Ok(())
}

/// `peri sync receive` 入口（掩码输入同步码）。
pub async fn run_receive_cli(server: &str, keystore_path: Option<&Path>) -> Result<()> {
    http_client::validate_server_url(server, false)?;
    let client = ReqwestClient::new(server)?;
    let backoff = http_client::ExponentialBackoff;
    let (local, store, peers) = load_cli_identity(keystore_path)?;
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    let cwd = std::env::current_dir()?;
    println!("Enter the sync code shown on the sender screen:");
    let code = rpassword::read_password().context("failed to read sync code")?;
    let cfg = FlowConfig::default();
    let flow = ReceiverFlow {
        client: &client,
        backoff: &backoff,
        store: store.as_ref(),
        local: &local,
        peers: &peers,
        home_dir: &home,
        cwd: &cwd,
        cfg: &cfg,
    };
    let outcome = run_receiver(flow, &code).await?;
    println!("Synced {} files", outcome.files);
    Ok(())
}

/// 打开本地身份与 keystore（`peri sync device init` 之后）。
fn load_cli_identity(
    keystore_path: Option<&Path>,
) -> Result<(DevicePublic, Box<dyn SecretStore>, TrustedPeers)> {
    let paths = device_cli::default_paths()?;
    let identity = device_cli::load_identity(&paths)?;
    let store = device_cli::open_device_store(keystore_path, &identity)?;
    let peers = device_cli::load_peers(&paths)?;
    Ok((identity, store, peers))
}

fn all_items() -> SyncItems {
    SyncItems {
        settings: Some(Default::default()),
        skills: Some(Default::default()),
        mcp: Some(Default::default()),
        plugins: Some(Default::default()),
    }
}
