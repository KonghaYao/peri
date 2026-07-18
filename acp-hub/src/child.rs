//! 子进程管理：spawn、通信、kill
//!
//! ChildHandle 封装一个 ACP 子进程的完整生命周期。
//! spawn_child() 返回 (ChildHandle, mpsc receiver)，
//! receiver 用于接收子进程主动推送的通知消息。

use anyhow::Context;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// 子进程句柄——管理与一个 ACP 子进程的通信
pub struct ChildHandle {
    process: Mutex<Child>,
    stdin: Mutex<BufWriter<tokio::process::ChildStdin>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    session_id: String,
}

/// 从子进程发往 Hub 的消息（通知或请求）
pub type ChildMessage = serde_json::Value;

/// 启动一个 ACP 子进程，返回句柄和消息接收器
///
/// # Arguments
/// * `cmd` - 启动命令（第一个元素是可执行文件，后续是参数）
/// * `cwd` - 子进程工作目录
/// * `session_id` - 关联的 session_id
pub async fn spawn_child(
    cmd: &[String],
    cwd: &str,
    session_id: &str,
) -> anyhow::Result<(ChildHandle, mpsc::UnboundedReceiver<ChildMessage>)> {
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("无法启动子进程")?;

    let stdin = Mutex::new(BufWriter::new(child.stdin.take().unwrap()));
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pending_clone = pending.clone();

    // 后台任务：持续读取子进程 stdout，解析 JSON-RPC 行
    //   - 有 id 且匹配 pending → 完成 oneshot
    //   - 否则 → 通过 channel 转发给 Hub
    let sid = session_id.to_string();
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line).await {
                Ok(0) => {
                    // stdout 关闭 → 子进程退出
                    tracing::info!(target: "acp_hub::child", session_id = sid, "子进程 stdout 关闭");
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(target: "acp_hub::child", "无法解析子进程输出: {}", e);
                            continue;
                        }
                    };
                    // 有 id → 匹配 pending 请求
                    if let Some(id) = parsed.get("id").and_then(|v| v.as_i64()) {
                        let mut pending = pending_clone.lock().await;
                        if let Some(sender) = pending.remove(&id) {
                            let _ = sender.send(parsed);
                            continue;
                        }
                    }
                    // 通知 / 不带 id 的响应 → 转发
                    if tx.send(parsed).is_err() {
                        break; // Hub 已关闭
                    }
                }
                Err(e) => {
                    tracing::error!(target: "acp_hub::child", session_id = sid, "子进程 stdout 读取错误: {}", e);
                    break;
                }
            }
        }
    });

    Ok((
        ChildHandle {
            process: Mutex::new(child),
            stdin,
            next_id: AtomicI64::new(1),
            pending,
            session_id: session_id.to_string(),
        },
        rx,
    ))
}

impl ChildHandle {
    /// 向子进程发送 JSON-RPC 请求，等待响应
    pub async fn send_request(
        &self,
        method: &str,
        params: &serde_json::Value,
        timeout_secs: u64,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_json(&request).await?;

        let response = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
            .await
            .map_err(|_| anyhow::anyhow!("子进程 {} 请求超时 ({}s)", method, timeout_secs))??;

        if let Some(err) = response.get("error") {
            anyhow::bail!(
                "子进程返回错误 [{}]: {}",
                err.get("code").and_then(|v| v.as_i64()).unwrap_or(0),
                err.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            );
        }
        Ok(response)
    }

    /// 向子进程发送 JSON-RPC 通知（无响应）
    pub async fn send_notification(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_json(&notif).await
    }

    async fn write_json(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(value)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 强制杀死子进程
    pub async fn kill(&self) -> anyhow::Result<()> {
        let mut process = self.process.lock().await;
        process.kill().await.context("无法杀死子进程")
    }

    /// 等待子进程退出，返回退出状态
    pub async fn wait(&self) -> anyhow::Result<std::process::ExitStatus> {
        let mut process = self.process.lock().await;
        process.wait().await.context("等待子进程退出失败")
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}
