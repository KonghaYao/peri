//! ACP 子进程管理（F6 改造，§4.1）：spawn（进程组）/ kill（进程组）/ stdin 写 /
//! stdout 读 / wait 监控。
//!
//! 进程面（不含会话逻辑，session_id 仅为标签）：
//! - **spawn**：`process_group(0)`（Unix，子进程自建进程组，pgid = 子进程 pid，
//!   macOS 支持）+ `kill_on_drop(true)`（§7.5 兜底语义）+ stderr 独立读任务
//!   （仅日志计数，防阻塞）；
//! - **stdout 读取任务**：逐行读取（JSON-RPC 行协议）→ sessionId 提取（§3.3
//!   双格式，见 [`crate::error::extract_session_id`]）→ [`ChildOutput::Frame`]；
//!   无法提取 sessionId 的帧丢弃并上报 [`ChildOutput::DroppedNoSessionId`]
//!   （本地缺口计数，§3.3）；不再做 pending/id 匹配（响应匹配归 server 侧）；
//! - **kill**：组级 `SIGTERM(-pgid)` → 宽限 `grace` → 组级 `SIGKILL(-pgid)`；
//!   已退出 → 立即成功（幂等）；
//! - **wait**：stdout EOF 后 `wait()` → 状态迁移 `Exited(code)` → 经通道上报
//!   （hub 组装 `instance/process_exit`）；
//! - **stdin 写**：`write_line` 写原样 JSON 行 + flush（§4.4 L2 的 instance 侧
//!   语义；进程已退出 → 写失败上报）。
//!
//! 进程组 kill 用 `kill(-pgid, sig)`（`libc` 符号）。`libc` crate 未预填
//! （见 f6-instance.md §12，由主管统一处理），本模块以自声明 FFI 落地同一
//! libSystem/libc 符号，后续可无感替换为 `libc::kill`。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

use crate::error::extract_session_id;

/// JSON-RPC response 判定（§6.1 同源：有 `id`、无 `method`）。
///
/// response 无 sessionId 字段，归属由 instance 已知（本进程唯一 hub session），
/// 见 [`run_stdout_reader`] 的兜底分支。
fn is_json_rpc_response(v: &serde_json::Value) -> bool {
    v.get("jsonrpc").is_some() && v.get("id").is_some() && v.get("method").is_none()
}

// ---------------------------------------------------------------------------
// 进程组 kill 原语（§4.1：libc::kill）
// ---------------------------------------------------------------------------

/// Unix 进程组信号原语（`libc::kill(-pgid, sig)`）。
#[cfg(unix)]
pub mod sys {
    pub use libc::{kill, SIGKILL, SIGTERM};

    /// 向进程组发送信号：`kill(-pgid, sig)`。返回是否成功（失败含 ESRCH——
    /// 组已不存在，幂等忽略）。
    pub fn kill_group(pgid: i32, sig: i32) -> bool {
        // SAFETY: kill(2) 无内存安全问题；参数为合法 i32 信号号与进程组 id。
        unsafe { kill(-pgid, sig) == 0 }
    }
}

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// 子进程运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// 运行中（stdout 未 EOF）。
    Running,
    /// 已退出（stdout EOF 后 wait 完成）。
    Exited(Option<i32>),
}

/// stdout 读取任务产出的 ACP 帧事件（dumb 透传，§3.3）。
#[derive(Debug, Clone)]
pub struct ChildEvent {
    pub session_id: String,
    /// 原始 ACP 帧（不透明 JSON）。
    pub frame: serde_json::Value,
}

/// 子进程生命周期事件（stdout 帧 / 退出 / 缺口计数）。
#[derive(Debug)]
pub enum ChildOutput {
    /// 可提取 sessionId 的帧（hub 转发调度）。
    Frame(ChildEvent),
    /// stdout EOF 后 wait 完成（hub 组装 `instance/process_exit`）。
    Exit { session_id: String, code: i32 },
    /// 无法提取 sessionId 的帧（已丢弃，§3.3 本地缺口计数）。
    DroppedNoSessionId,
}

/// 内部共享态（spawn 返回的 [`AcpProcess`] 为 Arc 封装）。
struct AcpInner {
    process: Mutex<Option<Child>>,
    stdin: Mutex<Option<BufWriter<tokio::process::ChildStdin>>>,
    session_id: String,
    /// 进程组 id（= 子进程 pid，`process_group(0)` 语义）。
    pgid: i32,
    state: StdMutex<ProcessState>,
}

/// ACP 子进程句柄（进程面；spawn 后经 `Arc` 共享，session 管理在 hub）。
pub struct AcpProcess {
    inner: Arc<AcpInner>,
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

/// 启动 ACP 子进程（进程组 + kill_on_drop + 双读任务）。
///
/// - `cmd`：启动命令（第一个元素为可执行文件）；
/// - `cwd`：工作目录；
/// - `env`：附加环境变量（§9.6 白名单由 hub 校验，此处仅透传）；
/// - `tx`：stdout 帧 / 退出 / 缺口事件的汇聚通道。
///
/// 返回 (句柄, 事件接收端) 中的句柄；事件经 `tx` 送达调用方（hub 侧统一汇聚）。
pub async fn spawn(
    cmd: &[String],
    cwd: &str,
    env: Option<&HashMap<String, String>>,
    session_id: &str,
    tx: mpsc::UnboundedSender<ChildOutput>,
) -> anyhow::Result<Arc<AcpProcess>> {
    if cmd.is_empty() {
        anyhow::bail!("cmd 为空");
    }
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        // 子进程自建进程组（pgid = 子进程 pid）；组级 kill 覆盖整棵进程树
        // （ACP + 孙进程），防 kill ACP 后孙进程成孤儿（§7.5/§8）。
        command.process_group(0);
    }
    if let Some(envs) = env {
        command.envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    let mut child = command.spawn().context("spawn ACP 子进程失败")?;

    let stdin = Mutex::new(child.stdin.take().map(BufWriter::new));
    let pgid = child.id().expect("spawn 成功必有 pid") as i32;
    let inner = Arc::new(AcpInner {
        process: Mutex::new(Some(child)),
        stdin,
        session_id: session_id.to_string(),
        pgid,
        state: StdMutex::new(ProcessState::Running),
    });

    let inner_read = inner.clone();
    tokio::spawn(async move {
        run_stdout_reader(inner_read, tx).await;
    });

    let inner_err = inner.clone();
    tokio::spawn(async move {
        run_stderr_reader(inner_err).await;
    });

    tracing::info!(target: "acp_hub::instance", session_id, pgid, "ACP 子进程已启动（进程组）");
    Ok(Arc::new(AcpProcess { inner }))
}

/// stdout 读任务：逐行解析 → sessionId 提取 → 帧事件；EOF → wait → 退出上报。
async fn run_stdout_reader(inner: Arc<AcpInner>, tx: mpsc::UnboundedSender<ChildOutput>) {
    let mut stdout = {
        let mut process = inner.process.lock().await;
        match process.as_mut().and_then(|c| c.stdout.take()) {
            Some(o) => BufReader::new(o),
            None => return,
        }
    };

    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line).await {
            Ok(0) => break, // stdout 关闭 → 子进程（可能）退出
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(target: "acp_hub::instance", session_id = %inner.session_id,
                            "ACP 输出非 JSON 行（丢弃，仅计数）: {e}");
                        if tx.send(ChildOutput::DroppedNoSessionId).is_err() {
                            return;
                        }
                        continue;
                    }
                };
                match extract_session_id(&parsed) {
                    Some(_sid) => {
                        // 信封 session_id = **进程归属**（hub session id，spawn
                        // 时确立，§4.5.1）：instance 本地记账（epoch/seq/缓冲）与
                        // server 侧 epoch 校验均按该键；帧内 sessionId（ACP
                        // 内部 id）**原样保留**，供 server 可信 binding 校验
                        // （§6.2/§495：acp_session_id → hub session_id，不
                        // 一致即丢弃）。曾误把信封改为 ACP 帧内 id，导致
                        // instance 查表丢弃 + server relay binding_missing
                        // 双端点事件回流断点。
                        if tx
                            .send(ChildOutput::Frame(ChildEvent {
                                session_id: inner.session_id.clone(),
                                frame: parsed,
                            }))
                            .is_err()
                        {
                            return;
                        }
                    }
                    None => {
                        // JSON-RPC response（有 id、无 method，§6.1 判定）没有
                        // sessionId 字段（L3 确认经 rpcId 匹配，§4.4；create 序列
                        // initialize/session/new 的响应在 binding 建立前到达，
                        // server 侧经 pending_rpc 匹配，§6.2）。本进程归属唯一
                        // hub session（§4.5：spawn 时确立），以 inner.session_id
                        // 兜底归属转发——否则 server 永远收不到 response（t03
                        // initialize timeout 根因）。
                        if is_json_rpc_response(&parsed) {
                            if tx
                                .send(ChildOutput::Frame(ChildEvent {
                                    session_id: inner.session_id.clone(),
                                    frame: parsed,
                                }))
                                .is_err()
                            {
                                return;
                            }
                        } else {
                            tracing::debug!(target: "acp_hub::instance", session_id = %inner.session_id,
                                "帧无法提取 sessionId（丢弃，缺口计数）");
                            if tx.send(ChildOutput::DroppedNoSessionId).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!(target: "acp_hub::instance", session_id = %inner.session_id,
                    "ACP stdout 读取错误: {e}");
                break;
            }
        }
    }

    // stdout EOF → wait（消费 &mut Child；kill 路径互斥于同一锁）
    let code = {
        let mut process = inner.process.lock().await;
        match process.as_mut() {
            Some(c) => c.wait().await.ok().and_then(|s| s.code()),
            None => None,
        }
    };
    {
        let mut state = inner.state.lock().expect("state mutex poisoned");
        *state = ProcessState::Exited(code);
    }
    tracing::info!(target: "acp_hub::instance", session_id = %inner.session_id, code, "ACP 子进程退出");
    let _ = tx.send(ChildOutput::Exit {
        session_id: inner.session_id.clone(),
        code: code.unwrap_or(-1),
    });
}

/// stderr 读任务：仅计数（行数/字节），不记正文（§9.3 脱敏），防管道阻塞。
async fn run_stderr_reader(inner: Arc<AcpInner>) {
    let mut stderr = {
        let mut process = inner.process.lock().await;
        match process.as_mut().and_then(|c| c.stderr.take()) {
            Some(e) => BufReader::new(e),
            None => return,
        }
    };
    let mut bytes: u64 = 0;
    let mut lines: u64 = 0;
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                bytes += n as u64;
                lines += buf[..n].iter().filter(|b| **b == b'\n').count() as u64;
            }
            Err(_) => break,
        }
    }
    tracing::debug!(target: "acp_hub::instance", session_id = %inner.session_id, bytes, lines,
        "ACP stderr 关闭（仅计数，正文不记录）");
}

// ---------------------------------------------------------------------------
// AcpProcess 方法
// ---------------------------------------------------------------------------

impl AcpProcess {
    /// 关联 session_id（标签用途）。
    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    /// 进程组 id（= 子进程 pid）。
    pub fn pgid(&self) -> i32 {
        self.inner.pgid
    }

    /// 当前运行状态（拷贝）。
    pub fn state(&self) -> ProcessState {
        *self
            .inner
            .state
            .lock()
            .expect("state mutex poisoned")
    }

    /// 向 ACP 子进程写入一条 JSON-RPC 行（原样 + flush，§4.4 L2）。
    ///
    /// 进程已退出或管道已关闭 → `Err`（hub 据此上报失败语义）。
    pub async fn write_line(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        {
            let state = self.inner.state.lock().expect("state mutex poisoned");
            if matches!(*state, ProcessState::Exited(_)) {
                anyhow::bail!("ACP 进程已退出");
            }
        }
        let line = serde_json::to_string(value)?;
        let mut stdin = self.inner.stdin.lock().await;
        let Some(w) = stdin.as_mut() else {
            anyhow::bail!("ACP stdin 不可用");
        };
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    /// 组级 kill：`SIGTERM(-pgid)` → 宽限 `grace` → `SIGKILL(-pgid)`（§4.1）。
    ///
    /// 幂等：已退出（或进程组不存在，ESRCH）→ 立即成功。stdout 读任务随后
    /// wait 完成并上报退出。
    pub async fn kill(&self, grace: Duration) -> anyhow::Result<()> {
        {
            let state = self.inner.state.lock().expect("state mutex poisoned");
            if matches!(*state, ProcessState::Exited(_)) {
                return Ok(());
            }
        }
        let pgid = self.inner.pgid;
        if !sys::kill_group(pgid, sys::SIGTERM) {
            // ESRCH 等：进程组已不存在，视为已达成（幂等）。
            return Ok(());
        }
        tokio::time::sleep(grace).await;
        sys::kill_group(pgid, sys::SIGKILL);
        tracing::info!(target: "acp_hub::instance", session_id = %self.inner.session_id, pgid,
            grace_ms = grace.as_millis(), "ACP 进程组 kill 完成");
        Ok(())
    }
}
