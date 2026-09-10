use std::{collections::HashMap, process::Stdio};

use tokio::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout},
};

use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};

mod dispatcher;
pub use dispatcher::{run_dispatch_loop, DispatchState, MessageDispatcher};

/// LSP 传输层：管理子进程的 stdin/stdout/stderr 管道
pub struct LspTransport {
    child: Child,
    stdin: ChildStdin,
    stdout_reader: BufReader<ChildStdout>,
}

impl LspTransport {
    /// 启动 LSP 服务器子进程
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, LspError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| LspError::LaunchFailed {
            server: command.to_string(),
            reason: e.to_string(),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdin".to_string(),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdout".to_string(),
        })?;

        // 启动后立即检查进程是否存活（捕获参数错误等立即退出的情况）
        // 对参数无效等场景，进程退出极快，try_wait 通常能立即捕获
        if let Some(status) = child.try_wait().ok().flatten() {
            let code = status.code().unwrap_or(-1);
            let reason = format!("进程立即退出 (exit code: {code})，请检查命令和参数是否正确");
            return Err(LspError::LaunchFailed {
                server: command.to_string(),
                reason,
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout_reader: BufReader::new(stdout),
        })
    }

    /// 发送 JSON-RPC 请求
    pub async fn send_request(&mut self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let body = serde_json::to_string(request)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 发送 JSON-RPC 通知
    pub async fn send_notification(
        &mut self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        let body = serde_json::to_string(notification)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 读取单条 JSON-RPC 消息
    pub async fn read_message(&mut self) -> Result<Option<String>, LspError> {
        codec::decode_message(&mut self.stdout_reader).await
    }

    /// 检查子进程是否存活
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// 获取子进程 ID
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// 终止子进程
    pub async fn kill(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}
