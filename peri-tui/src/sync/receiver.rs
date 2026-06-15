//! Receiver mode: enter pair code → select items → receive + decrypt → write

use std::io::Write;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::info;

use crate::sync::{
    crypto,
    protocol::WsMessage,
    ui::{build_default_items, confirm_sync, println_overwrite, select_sync_items},
    writer,
};

pub async fn run_sync_receiver(server_url: &str) -> Result<()> {
    let home_dir = dirs_next::home_dir().context("Failed to get HOME directory")?;
    let cwd = std::env::current_dir()?;

    print!("Enter pair code: ");
    std::io::stdout().flush()?;
    let mut pair_code = String::new();
    std::io::stdin().read_line(&mut pair_code)?;
    let pair_code = pair_code.trim().to_string();
    if pair_code.is_empty() {
        anyhow::bail!("Pair code cannot be empty");
    }

    let url = format!("{server_url}/ws?role=receiver&code={pair_code}");
    let (mut ws, _) = connect_async(&url)
        .await
        .context("Failed to connect to relay server")?;
    info!("Connected to {server_url}");

    let msg = WsMessage::JoinPair {
        pair_code: pair_code.clone(),
    };
    ws.send(Message::Text(serde_json::to_string(&msg)?.into()))
        .await?;

    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: WsMessage = serde_json::from_str(&text)?;
                match msg {
                    WsMessage::PairJoined { .. } => {
                        println_overwrite("Connected! Select items to sync:");
                        break;
                    }
                    WsMessage::Error { code, message } => {
                        anyhow::bail!("Join error [{code}]: {message}");
                    }
                    _ => {}
                }
            }
            Some(Err(e)) => {
                anyhow::bail!("WebSocket error: {e}")
            }
            None => anyhow::bail!("Connection closed"),
            _ => {}
        }
    }

    let mut items = build_default_items();
    let selected = select_sync_items(&mut items)?;

    if !confirm_sync(&selected)? {
        println_overwrite("Sync cancelled");
        let _ = ws.close(None).await;
        return Ok(());
    }

    let msg = WsMessage::SyncConfig { items: selected };
    ws.send(Message::Text(serde_json::to_string(&msg)?.into()))
        .await?;

    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut received = 0u64;
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: WsMessage = serde_json::from_str(&text)?;
                match msg {
                    WsMessage::DataChunk { seq, data } => {
                        received += 1;
                        chunks.push((seq, data));
                        print!("\rReceiving: {} chunks", received);
                        std::io::stdout().flush()?;
                    }
                    WsMessage::TransferComplete { checksum: _ } => {
                        println_overwrite("Receive complete, decrypting...");
                        break;
                    }
                    WsMessage::Error { code, message } => {
                        anyhow::bail!("Transfer error [{code}]: {message}");
                    }
                    _ => {}
                }
            }
            Some(Err(e)) => {
                anyhow::bail!("WebSocket error: {e}")
            }
            None => anyhow::bail!("Connection closed"),
            _ => {}
        }
    }

    chunks.sort_by_key(|(seq, _)| *seq);
    let encrypted: Vec<u8> = chunks.into_iter().flat_map(|(_, data)| data).collect();

    let key = crypto::derive_key(&pair_code);
    let decrypted =
        crypto::decrypt(&encrypted, &key).context("Decryption failed — pair code may not match")?;
    let package: crate::sync::protocol::SyncPackage =
        rmp_serde::from_slice(&decrypted).context("Unpack failed — data format mismatch")?;

    println_overwrite("Writing files...");
    writer::write_sync_items(&home_dir, &cwd, &package.items)?;

    println_overwrite("Sync complete!");
    let _ = ws.close(None).await;
    Ok(())
}
