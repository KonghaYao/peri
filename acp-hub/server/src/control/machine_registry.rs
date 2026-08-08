//! machine 注册表（架构 §7.1/§4.5/§7.5，设计稿 `f5-channel-control.md` §11）。
//!
//! machine 生命周期（REGISTERED → ONLINE ⇄ OFFLINE）+ 指令下发（spawn/kill/
//! forward_rpc）+ ack 跟踪（oneshot 回填 + 超时）+ hello 幂等替换（fencing）。
//! 判定性时间戳由 server 权威时钟（§4.7：`last_heartbeat` 用 [`Instant`]）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

use acp_hub_proto::frame::Frame;
use acp_hub_proto::machine::{
    MachineForwardAck, MachineHeartbeat, MachineHello, MachineKill, MachineKillAck,
    MachineProcessExit, MachineSpawn, MachineSpawnAck,
};

use crate::channel::OutboundMsg;
use crate::control::{SessionRegistry, SessionState};

/// machine 生命周期状态（§7.1 图）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineState {
    /// hello 成功（含双向认证，§9.2 步骤 2）。
    Registered,
    /// 心跳活跃。
    Online,
    /// 心跳超时（默认 30s）/ 连接断开。
    Offline,
}

impl MachineState {
    /// 是否可接收指令（spawn/kill/forward_rpc）。
    pub fn can_serve(self) -> bool {
        matches!(self, MachineState::Registered | MachineState::Online)
    }

    /// 状态标签（脱敏日志）。
    pub fn as_str(self) -> &'static str {
        match self {
            MachineState::Registered => "registered",
            MachineState::Online => "online",
            MachineState::Offline => "offline",
        }
    }
}

/// machine 在线连接句柄（fencing 后失效）。
#[derive(Debug, Clone)]
pub struct MachineConn {
    /// 连接发送通道（gateway 的 ws 发送队列）。
    pub tx: mpsc::Sender<OutboundMsg>,
}

/// ack 回填（§4.5：spawn_ack/kill_ack/forward_ack 按 command_id 路由）。
#[derive(Debug, Clone, PartialEq)]
pub enum MachineAck {
    /// `machine/spawn_ack`。
    Spawn(MachineSpawnAck),
    /// `machine/kill_ack`。
    Kill(MachineKillAck),
    /// `machine/forward_ack`（下行 JSON-RPC 转发确认，L1+L2，§4.4）。
    Forward(MachineForwardAck),
    /// `machine/process_exit`（无 command_id，按 session 路由）。
    ProcessExit(MachineProcessExit),
}

/// hello 处理产物（补推协调输入，§4.5/§7.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloOutcome {
    /// 是否 fencing 了旧连接（§4.5 幂等替换）。
    pub fenced_previous: bool,
    /// `buffer_lost` 上报（daemon 崩溃缓冲丢失，§7.5）。
    pub buffer_lost: bool,
    /// machine 声称存活的 session 清单（§8.3 对账输入）。
    pub alive_sessions: Vec<String>,
    /// per-session 流纪元映射（§4.5.1）。
    pub session_epochs: HashMap<String, u64>,
}

/// 指令下发结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnOutcome {
    /// spawn_ack 已回填。
    Acked(MachineSpawnAck),
}

/// 指令下发结果。
#[derive(Debug, Clone, PartialEq)]
pub enum KillOutcome {
    /// kill_ack 已回填。
    Acked(MachineKillAck),
}

/// machine 注册表错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineError {
    /// machine 未登记（hello 未到达）。
    #[error("machine not registered: {0}")]
    UnknownMachine(String),
    /// machine 离线（OFFLINE，§7.1）。
    #[error("machine offline")]
    Offline,
    /// 下发超时（spawn 10s / kill 10s / forward 10s，§6.2/§16）→ AGENT_UNAVAILABLE(retryable)。
    #[error("command timeout")]
    Timeout,
    /// machine 侧转发确认失败（`machine/forward_ack` ok=false：ACP 进程已
    /// 退出/管道关闭等）→ AGENT_UNAVAILABLE(retryable)。
    #[error("forward rejected: {0}")]
    ForwardRejected(String),
    /// 连接已 fencing/关闭。
    #[error("machine connection gone")]
    ConnectionGone,
}

/// machine 条目（进程内状态）。
struct MachineEntry {
    state: MachineState,
    token_id: String,
    hostname: String,
    conn: Option<mpsc::Sender<OutboundMsg>>,
    last_heartbeat: Instant,
    /// command_id → ack oneshot（spawn/kill ack 跟踪，§4.5）。
    pending_acks: HashMap<String, oneshot::Sender<MachineAck>>,
    /// hello 上报的 per-session 流纪元（§4.5.1；relay 入站校验输入）。
    session_epochs: HashMap<String, u64>,
    /// 最近 hello 的 alive_sessions（对账）。
    alive_sessions: Vec<String>,
    /// 最近 hello 的 buffer_lost。
    buffer_lost: bool,
}

/// machine 生命周期注册表（§7.1）。
#[derive(Clone)]
pub struct MachineRegistry {
    inner: Arc<MachineInner>,
}

struct MachineInner {
    machines: RwLock<HashMap<String, MachineEntry>>,
    /// 离线判定超时（§16 默认 30s）。
    offline_timeout: Duration,
    /// spawn/kill 下发超时（§6.2/§16 默认 10s）。
    cmd_timeout: Duration,
    sessions: SessionRegistry,
}

impl MachineRegistry {
    /// 以离线超时与指令超时构建（§16：30s / 10s）。
    pub fn new(offline_timeout: Duration, cmd_timeout: Duration, sessions: SessionRegistry) -> Self {
        MachineRegistry {
            inner: Arc::new(MachineInner {
                machines: RwLock::new(HashMap::new()),
                offline_timeout,
                cmd_timeout,
                sessions,
            }),
        }
    }

    /// hello 处理（认证在 gateway 完成，§9.2；§4.5 幂等替换）：同 machine_id
    /// 新连接 → 旧连接 fencing（旧连接事件丢弃、关闭）；注册/替换连接与
    /// 对账输入。返回 [`HelloOutcome`]（补推协调与孤儿清理钩子输入，§7.5）。
    pub async fn on_hello(
        &self,
        machine_id: &str,
        token_id: &str,
        conn: MachineConn,
        hello: &MachineHello,
    ) -> HelloOutcome {
        let mut machines = self.inner.machines.write().await;
        let fenced = if let Some(old) = machines.get(machine_id) {
            if let Some(old_tx) = &old.conn {
                // fencing：旧连接关闭（1011 通用失败；旧连接事件经 gateway
                // 侧连接结束路径丢弃，§4.5）。
                let _ = old_tx.send(OutboundMsg::Close(1011)).await;
            }
            true
        } else {
            false
        };
        let epochs = hello.stream_epochs.clone().unwrap_or_default();
        // M1 hello 无存活清单字段（§4.5 表 machine/hello 无 alive_sessions；
        // 存活清单经 machine/heartbeat 上报，§8.3 对账在其后由心跳驱动）。
        let alive: Vec<String> = Vec::new();
        let buffer_lost = hello.buffer_lost.unwrap_or(false);
        machines.insert(
            machine_id.to_string(),
            MachineEntry {
                state: MachineState::Online,
                token_id: token_id.to_string(),
                hostname: hello.hostname.clone(),
                conn: Some(conn.tx),
                last_heartbeat: Instant::now(),
                pending_acks: HashMap::new(),
                session_epochs: epochs.clone(),
                alive_sessions: alive.clone(),
                buffer_lost,
            },
        );
        let (token_id, hostname, buffer_lost) = {
            let entry = machines.get(machine_id).expect("just inserted");
            (
                entry.token_id.clone(),
                entry.hostname.clone(),
                entry.buffer_lost,
            )
        };
        drop(machines);
        info!(
            machine_id, token_id, hostname, fenced, buffer_lost,
            alive_sessions = alive.len(),
            "machine hello registered (idempotent replace)"
        );
        HelloOutcome {
            fenced_previous: fenced,
            buffer_lost,
            alive_sessions: alive,
            session_epochs: epochs,
        }
    }

    /// 心跳更新（§7.1：5s；alive_sessions 供对账，§8.3）。
    ///
    /// alive_sessions 变化且非空时触发对账（§8.3 步骤 5：意外存活 → kill
    /// 裁决 §7.5、pending_close 补发 §7.6）——M1 hello 无存活清单字段
    /// （§4.5 表），对账的 alive 输入唯一来源是心跳；spawn 后台任务避免
    /// kill（每 session 等 ack 最多 10s）阻塞 gateway 帧循环。
    pub async fn on_heartbeat(
        &self,
        machine_id: &str,
        hb: &MachineHeartbeat,
    ) -> Result<(), MachineError> {
        let changed = {
            let mut machines = self.inner.machines.write().await;
            let Some(entry) = machines.get_mut(machine_id) else {
                return Err(MachineError::UnknownMachine(machine_id.to_string()));
            };
            entry.last_heartbeat = Instant::now();
            entry.state = MachineState::Online;
            let changed = entry.alive_sessions != hb.alive_sessions;
            entry.alive_sessions = hb.alive_sessions.clone();
            changed
        };
        debug!(machine_id, load = hb.load, "machine heartbeat");
        if changed && !hb.alive_sessions.is_empty() {
            let me = self.clone();
            let mid = machine_id.to_string();
            let alive = hb.alive_sessions.clone();
            tokio::spawn(async move {
                me.reconcile_and_kill(&mid, &alive).await;
            });
        }
        Ok(())
    }

    /// 离线判定 tick（与心跳同 tick）：`offline_timeout` 无心跳 → OFFLINE；
    /// 返回本次离线集合（由 hub 联动 `RelayEventHandler::on_machine_disconnect`，
    /// §7.1 离线即刻生效）。
    ///
    /// **连接句柄保留**：心跳超时只代表判定离线，TCP 连接可能仍存活；若清
    /// 空 `conn`，机器心跳恢复 → ONLINE 后仍不可服务（conn 无恢复路径），
    /// 而机器不会重连（连接健康）——服务瘫痪。真正断开由
    /// [`Self::on_disconnect`]（连接结束路径）清句柄。
    pub async fn sweep_offline(&self, now: Instant) -> Vec<String> {
        let mut machines = self.inner.machines.write().await;
        let mut offline: Vec<String> = Vec::new();
        for (id, entry) in machines.iter_mut() {
            if entry.state == MachineState::Offline {
                continue;
            }
            if now.duration_since(entry.last_heartbeat) >= self.inner.offline_timeout {
                entry.state = MachineState::Offline;
                offline.push(id.clone());
                warn!(
                    machine_id = id, timeout_ms = self.inner.offline_timeout.as_millis() as u64,
                    "machine offline (heartbeat timeout)"
                );
            }
        }
        offline
    }

    /// 连接断开（gateway 连接结束路径）：仅当断开的是**当前登记连接**时置
    /// OFFLINE（§7.1 图：连接断开 → OFFLINE）+ 清连接句柄。
    ///
    /// hello fencing（§4.5 幂等替换）后，旧连接滞后断开不得触碰新连接状态
    /// ——以 `conn` 句柄比对（`same_channel`）识别陈旧断开，返回 false。
    /// 返回值 = 是否曾在线（供 gateway 决定是否触发断链清理，§8.2）。
    pub async fn on_disconnect(&self, machine_id: &str, conn: &MachineConn) -> bool {
        let mut machines = self.inner.machines.write().await;
        let Some(entry) = machines.get_mut(machine_id) else {
            return false;
        };
        let is_current = entry
            .conn
            .as_ref()
            .is_some_and(|tx| tx.same_channel(&conn.tx));
        if !is_current {
            // 陈旧断开（fencing 后旧连接退出）：状态已被新 hello 替换。
            return false;
        }
        let was_online = entry.state != MachineState::Offline;
        entry.state = MachineState::Offline;
        entry.conn = None;
        was_online
    }

    /// 指令下发（§4.5）：发送 + ack 表登记 + 超时（spawn/kill 10s，§16）。
    /// 超时 → [`MachineError::Timeout`]（→ AGENT_UNAVAILABLE retryable）；
    /// OFFLINE → [`MachineError::Offline`]。
    pub async fn send_spawn(
        &self,
        machine_id: &str,
        cmd: MachineSpawn,
    ) -> Result<SpawnOutcome, MachineError> {
        let ack = self
            .send_command(machine_id, cmd.command_id.clone(), Frame::MachineSpawn(cmd))
            .await?;
        match ack {
            MachineAck::Spawn(s) => Ok(SpawnOutcome::Acked(s)),
            _ => Err(MachineError::ConnectionGone),
        }
    }

    /// 指令下发（§4.5）：kill + ack 跟踪（kill_ack；幂等——已死成功返回）。
    pub async fn send_kill(
        &self,
        machine_id: &str,
        cmd: MachineKill,
    ) -> Result<KillOutcome, MachineError> {
        let ack = self
            .send_command(machine_id, cmd.command_id.clone(), Frame::MachineKill(cmd))
            .await?;
        match ack {
            MachineAck::Kill(k) => Ok(KillOutcome::Acked(k)),
            _ => Err(MachineError::ConnectionGone),
        }
    }

    /// 指令下发统一路径：登记 ack oneshot → 发送 → 等 ack（超时
    /// `cmd_timeout`）。
    async fn send_command(
        &self,
        machine_id: &str,
        command_id: String,
        frame: Frame,
    ) -> Result<MachineAck, MachineError> {
        let (reply, rx) = oneshot::channel();
        let tx = {
            let mut machines = self.inner.machines.write().await;
            let Some(entry) = machines.get_mut(machine_id) else {
                return Err(MachineError::UnknownMachine(machine_id.to_string()));
            };
            if !entry.state.can_serve() {
                return Err(MachineError::Offline);
            }
            let Some(conn) = entry.conn.clone() else {
                return Err(MachineError::Offline);
            };
            if entry.pending_acks.contains_key(&command_id) {
                // 重发（session_id 幂等键，§4.5）：登记覆盖旧 oneshot。
                warn!(machine_id, command_id, "duplicate in-flight command ack slot replaced");
            }
            entry
                .pending_acks
                .insert(command_id.clone(), reply);
            conn
        };
        if tx.send(OutboundMsg::Frame(frame)).await.is_err() {
            self.drop_pending_ack(machine_id, &command_id).await;
            return Err(MachineError::ConnectionGone);
        }
        match tokio::time::timeout(self.inner.cmd_timeout, rx).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) => {
                self.drop_pending_ack(machine_id, &command_id).await;
                Err(MachineError::ConnectionGone)
            }
            Err(_) => {
                self.drop_pending_ack(machine_id, &command_id).await;
                Err(MachineError::Timeout)
            }
        }
    }

    /// ack 路由（spawn_ack/kill_ack，§4.5）：按 command_id 回填 oneshot。
    /// 返回是否匹配到在途命令。
    pub async fn on_ack(&self, machine_id: &str, command_id: &str, ack: MachineAck) -> bool {
        let mut machines = self.inner.machines.write().await;
        let Some(entry) = machines.get_mut(machine_id) else {
            return false;
        };
        match entry.pending_acks.remove(command_id) {
            Some(tx) => {
                let _ = tx.send(ack);
                true
            }
            None => false,
        }
    }

    /// JSON-RPC 透传（initialize/session/new/prompt/cancel/resolve 出站；
    /// L1+L2 合并确认由 `machine/forward_ack` 承载，§4.4 M1 合并）。
    ///
    /// 【冲突 1 裁决】下行载体由裸 JSON-RPC 文本改为 `machine/forward` 帧
    /// （M1 machine 帧集新增；machine 写 ACP stdin 成功回 forward_ack）。
    /// `command_id` 取消息 `id`（rpcId，server 生成，`hub-{n}` 全局单调），
    /// ack 路由与 spawn/kill 同表。
    pub async fn forward_rpc(
        &self,
        machine_id: &str,
        session_id: &str,
        msg: &serde_json::Value,
    ) -> Result<(), MachineError> {
        let command_id = msg
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or(MachineError::ConnectionGone)?;
        let frame = Frame::MachineForward(acp_hub_proto::machine::MachineForward {
            command_id: command_id.to_string(),
            session_id: session_id.to_string(),
            frame: msg.clone(),
        });
        let ack = self
            .send_command(machine_id, command_id.to_string(), frame)
            .await?;
        match ack {
            MachineAck::Forward(f) if f.ok => Ok(()),
            MachineAck::Forward(f) => Err(MachineError::ForwardRejected(
                f.error.unwrap_or_default(),
            )),
            _ => Err(MachineError::ConnectionGone),
        }
    }

    /// hello 上报的 per-session 流纪元查询（relay 入站 epoch 校验，§4.5.1）。
    pub async fn session_epoch(&self, machine_id: &str, acp_session_id: &str) -> Option<u64> {
        self.inner
            .machines
            .read()
            .await
            .get(machine_id)
            .and_then(|e| e.session_epochs.get(acp_session_id).copied())
    }

    /// machine 状态查询（诊断/测试）。
    pub async fn state(&self, machine_id: &str) -> Option<MachineState> {
        self.inner
            .machines
            .read()
            .await
            .get(machine_id)
            .map(|e| e.state)
    }

    /// 孤儿进程清理钩子（§7.5）：hello 后对「server 已标记终态/未登记但
    /// machine 声称存活」的 session 补发 kill（实际进程清理在 machine 侧；
    /// server 只负责下发与 ack 跟踪）。
    ///
    /// 返回已下发 kill 的 session 清单。
    pub async fn cleanup_orphans(&self, machine_id: &str, outcome: &HelloOutcome) -> Vec<String> {
        let report = match self
            .inner
            .sessions
            .reconcile_alive(machine_id, &outcome.alive_sessions)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(machine_id, error = ?e, "orphan cleanup reconcile failed");
                return Vec::new();
            }
        };
        self.kill_sessions(machine_id, &report.to_kill).await
    }

    /// 心跳驱动的对账（§8.3 步骤 5）：alive_sessions 与 Registry 比对 →
    /// 摘要日志 + to_kill 逐个补发 kill（§7.5 意外存活裁决 + §7.6
    /// pending_close 补发）。
    pub async fn reconcile_and_kill(&self, machine_id: &str, alive: &[String]) {
        let report = match self
            .inner
            .sessions
            .reconcile_alive(machine_id, alive)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(machine_id, error = ?e, "heartbeat reconciliation failed");
                return;
            }
        };
        self.kill_sessions(machine_id, &report.to_kill).await;
    }

    /// kill 裁决下发（§7.5/§7.6）：对 `to_kill` 逐个补发 `machine/kill`
    /// （幂等，已死成功返回），成功后会话置 Closed（「Registry 标记已清理」）。
    async fn kill_sessions(&self, machine_id: &str, to_kill: &[String]) -> Vec<String> {
        let mut killed = Vec::new();
        for sid in to_kill {
            let command_id = uuid::Uuid::new_v4().to_string();
            let cmd = MachineKill {
                command_id: command_id.clone(),
                session_id: sid.clone(),
                grace: None,
            };
            match self.send_kill(machine_id, cmd).await {
                Ok(_) => {
                    killed.push(sid.clone());
                    // 意外存活/终态清理完成 → 会话置 Closed（§7.5「Registry
                    // 标记已清理」；pending_close 集合在 transition(Closed)
                    // 中清除，§7.6）。
                    if let Ok(()) = self
                        .inner
                        .sessions
                        .transition(sid, SessionState::Closed)
                        .await
                    {
                        // noop
                    }
                }
                Err(e) => warn!(machine_id, session_id = sid, error = ?e, "orphan kill failed"),
            }
        }
        if !killed.is_empty() {
            info!(machine_id, killed = killed.len(), "orphan sessions killed (§7.5)");
        }
        killed
    }

    async fn drop_pending_ack(&self, machine_id: &str, command_id: &str) {
        let mut machines = self.inner.machines.write().await;
        if let Some(entry) = machines.get_mut(machine_id) {
            entry.pending_acks.remove(command_id);
        }
    }
}

#[cfg(test)]
#[path = "machine_registry_test.rs"]
mod machine_registry_test;
