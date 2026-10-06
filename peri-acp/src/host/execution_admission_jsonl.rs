use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::transport::{
    RequestTransport,
    router::RequestRouter,
    types::{AcpError, IncomingMessage, RequestId},
};

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub struct SdkDispatcherLaunch {
    pub executable: PathBuf,
    pub module: PathBuf,
    pub database: PathBuf,
    pub instance_id: String,
    pub generation_id: String,
}

pub struct JsonlSdkDispatcher {
    sender: mpsc::Sender<Value>,
    router: RequestRouter,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Readiness {
    protocol_version: u32,
    durability: String,
}

impl JsonlSdkDispatcher {
    pub async fn launch(config: SdkDispatcherLaunch) -> anyhow::Result<Self> {
        Self::launch_with_reverse_transport(config, None).await
    }

    pub async fn launch_with_reverse_transport(
        config: SdkDispatcherLaunch,
        reverse: Option<Arc<dyn RequestTransport>>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.executable.is_absolute(),
            "SDK runtime must be an absolute trusted launcher path"
        );
        anyhow::ensure!(
            config.module.is_absolute(),
            "SDK module must be an absolute trusted launcher path"
        );
        anyhow::ensure!(
            config.database.is_absolute(),
            "SDK registry database must be an absolute persistent path"
        );
        anyhow::ensure!(
            !config.instance_id.is_empty() && !config.generation_id.is_empty(),
            "SDK dispatcher identity required"
        );
        let module_directory = config
            .module
            .parent()
            .ok_or_else(|| anyhow::anyhow!("SDK module directory unavailable"))?;
        let mut child = Command::new(config.executable)
            .current_dir(module_directory)
            .arg(&config.module)
            .arg("--database")
            .arg(config.database)
            .arg("--instance-id")
            .arg(config.instance_id)
            .arg("--generation-id")
            .arg(config.generation_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("SDK stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("SDK stdout unavailable"))?;
        let (sender, mut receiver) = mpsc::channel::<Value>(64);
        let router = RequestRouter::new();
        let writer_router = router.clone();
        let writer = tokio::spawn(async move {
            while let Some(frame) = receiver.recv().await {
                let Ok(mut bytes) = serde_json::to_vec(&frame) else {
                    break;
                };
                if bytes.len() > MAX_FRAME_BYTES {
                    break;
                }
                bytes.push(b'\n');
                if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            writer_router.close();
        });
        let reader_router = router.clone();
        let response_sender = sender.downgrade();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut frame_buffer = Vec::new();
            let mut reverse_tasks = JoinSet::new();
            loop {
                tokio::select! {
                    _ = reader_router.wait_closed() => break,
                    _ = reverse_tasks.join_next(), if !reverse_tasks.is_empty() => {},
                    line = read_bounded_frame(&mut reader, &mut frame_buffer) => {
                        let Ok(Some(line)) = line else { break };
                        let Ok(frame) = serde_json::from_str::<Value>(&line) else { break };
                        if let Some(method) = frame.get("method").and_then(Value::as_str) {
                            if frame.get("id").is_none() { continue; }
                            let Some(id) = frame.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else { break };
                            let id = id.to_owned();
                            let method = method.to_owned();
                            let params = frame.get("params").cloned().unwrap_or(Value::Null);
                            let reverse = reverse.clone();
                            let response_sender = response_sender.clone();
                            reverse_tasks.spawn(async move {
                                let result = match (method.as_str(), reverse) {
                                    ("session/work/query" | "session/work/resolve" | "session/execute" | "session/execute/resolve", Some(reverse)) =>
                                        reverse.send_request(&method, params).await,
                                    _ => Err(AcpError::new(-32601, "SDK reverse execution capability unavailable")),
                                };
                                let frame = match result {
                                    Ok(result) => json!({"id":id,"result":result}),
                                    Err(error) => json!({"id":id,"error":error}),
                                };
                                if let Some(sender) = response_sender.upgrade() { let _ = sender.send(frame).await; }
                            });
                        } else {
                            let Some(id) = frame.get("id").and_then(Value::as_str) else { break };
                            let Ok(numeric_id) = id.parse::<i64>() else { break };
                            let result = decode_response(&line, id);
                            reader_router.dispatch(&IncomingMessage::Response { id: RequestId::Number(numeric_id), result });
                        }
                    }
                }
            }
            reader_router.close();
            reverse_tasks.abort_all();
            while reverse_tasks.join_next().await.is_some() {}
            writer.abort();
            let _ = writer.await;
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        let dispatcher = Self { sender, router };
        let ready = tokio::time::timeout(
            Duration::from_secs(10),
            dispatcher.send_request(
                "peri/execution/ready",
                json!({"protocolVersion": super::execution_admission::EXECUTION_PROTOCOL_VERSION}),
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("SDK execution dispatcher readiness timeout"))??;
        let readiness: Readiness = serde_json::from_value(ready)?;
        anyhow::ensure!(
            readiness.protocol_version == super::execution_admission::EXECUTION_PROTOCOL_VERSION
                && readiness.durability == "durable",
            "SDK persistent execution dispatcher capability required"
        );
        Ok(dispatcher)
    }
}

async fn read_bounded_frame<Reader: AsyncBufRead + Unpin>(
    reader: &mut Reader,
    frame_buffer: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if frame_buffer.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "SDK truncated frame",
                ))
            };
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(chunk.len(), |position| position + 1);
        let payload_bytes = newline.unwrap_or(consumed);
        if frame_buffer.len().saturating_add(payload_bytes) > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SDK frame exceeds limit",
            ));
        }
        frame_buffer.extend_from_slice(&chunk[..payload_bytes]);
        reader.consume(consumed);
        if newline.is_some() {
            return String::from_utf8(std::mem::take(frame_buffer))
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    }
}

fn decode_response(line: &str, expected_id: &str) -> Result<Value, AcpError> {
    let response: Value =
        serde_json::from_str(line).map_err(|_| AcpError::new(-32603, "SDK malformed response"))?;
    if response.get("id").and_then(Value::as_str) != Some(expected_id) {
        return Err(AcpError::new(-32603, "SDK response correlation mismatch"));
    }
    match (response.get("result"), response.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or_else(|| AcpError::new(-32603, "SDK malformed error response"))?;
            Err(AcpError::new(
                code,
                "SDK execution dispatcher rejected request",
            ))
        }
        _ => Err(AcpError::new(-32603, "SDK response lacks a unique outcome")),
    }
}

#[async_trait]
impl RequestTransport for JsonlSdkDispatcher {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let pending = self
            .router
            .register()
            .map_err(|_| AcpError::new(-32603, "SDK execution outcome unknown"))?;
        let frame = json!({"id":pending.id().to_string(),"method":method,"params":params});
        if self.sender.send(frame).await.is_err() {
            self.router.close();
            return Err(AcpError::new(-32603, "SDK execution outcome unknown"));
        }
        let result = if method == "peri/execution/activate" {
            pending.wait().await
        } else {
            match tokio::time::timeout(Duration::from_secs(30), pending.wait()).await {
                Ok(result) => result,
                Err(_) => {
                    self.router.close();
                    Err(AcpError::new(-32603, "SDK execution outcome unknown"))
                }
            }
        };
        result.map_err(|error| {
            if error.code == -32603 {
                AcpError::new(-32603, "SDK execution outcome unknown")
            } else {
                error
            }
        })
    }
}

#[cfg(test)]
#[path = "execution_admission_jsonl_test.rs"]
mod tests;

#[cfg(test)]
#[path = "execution_admission_jsonl_sqlite_test.rs"]
mod sqlite_tests;
