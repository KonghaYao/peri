//! Sender mode: request pair code → wait for receiver → pack + encrypt → send

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::sync::{
    packer::{self, PackedData},
    protocol::WsMessage,
    scanner,
    ui::{println_overwrite, ProgressBar},
};

pub async fn run_sync_sender(server_url: &str) -> Result<()> {
    let home_dir = dirs_next::home_dir().context("Failed to get HOME directory")?;
    let cwd = std::env::current_dir()?;

    let url = format!("{server_url}/ws?role=sender");
    let (mut ws, _) = connect_async(&url)
        .await
        .context("Failed to connect to relay server")?;

    let msg = serde_json::to_string(&WsMessage::RequestPair)?;
    ws.send(Message::Text(msg.into())).await?;

    let pair_code = loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: WsMessage = serde_json::from_str(&text)?;
                match msg {
                    WsMessage::PairCreated { pair_code } => break pair_code,
                    WsMessage::Error { code, message } => {
                        anyhow::bail!("Pair error [{code}]: {message}")
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
    };

    println!("Pair code: {pair_code}");
    println!("Waiting for receiver...");

    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: WsMessage = serde_json::from_str(&text)?;
                if matches!(msg, WsMessage::PairJoined { .. }) {
                    println_overwrite("Receiver connected! Waiting for sync selection...");
                    break;
                }
                if matches!(msg, WsMessage::Error { .. }) {
                    anyhow::bail!("Pair error: {text}");
                }
            }
            Some(Err(e)) => {
                anyhow::bail!("WebSocket error: {e}")
            }
            None => anyhow::bail!("Connection closed"),
            _ => {}
        }
    }

    let sync_filter = loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: WsMessage = serde_json::from_str(&text)?;
                if let WsMessage::SyncConfig { items } = msg {
                    break items;
                }
            }
            Some(Err(e)) => {
                anyhow::bail!("WebSocket error: {e}")
            }
            None => anyhow::bail!("Connection closed"),
            _ => {}
        }
    };

    let sync_pkg = scanner::scan_all(&home_dir, &cwd, &sync_filter);

    let PackedData {
        chunks,
        encrypted_size,
    } = packer::pack(&sync_pkg, &pair_code)?;

    let total = chunks.len() as u64;
    println_overwrite(&format!(
        "Sending {} chunks ({} bytes)...",
        total, encrypted_size
    ));
    let pb = ProgressBar::new(total, "Sending");

    for (i, chunk) in chunks.iter().enumerate() {
        let msg = WsMessage::DataChunk {
            seq: chunk.seq,
            data: chunk.data.clone(),
        };
        ws.send(Message::Text(serde_json::to_string(&msg)?.into()))
            .await?;
        pb.update(i as u64 + 1);
    }

    let checksum = {
        let mut all = Vec::new();
        for chunk in &chunks {
            all.extend_from_slice(&chunk.data);
        }
        packer::compute_checksum(&all)
    };
    let msg = WsMessage::TransferComplete { checksum };
    ws.send(Message::Text(serde_json::to_string(&msg)?.into()))
        .await?;

    pb.finish();
    println_overwrite("Transfer complete!");

    let _ = ws.close(None).await;
    Ok(())
}
