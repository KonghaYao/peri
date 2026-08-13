//! instance 注册表（架构 §7.1/§4.5/§7.5，设计稿 `f5-channel-control.md` §11）。
//!
//! instance 生命周期（REGISTERED → ONLINE ⇄ OFFLINE）+ 指令下发（spawn/kill/
//! forward_rpc）+ ack 跟踪（oneshot 回填 + 超时）+ hello 幂等替换（fencing）。
//! 判定性时间戳由 server 权威时钟（§4.7：`last_heartbeat` 用 [`Instant`]）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

use acp_hub_proto::frame::Frame;
use acp_hub_proto::instance::{
    InstanceForwardAck, InstanceHeartbeat, InstanceHello, InstanceKill, InstanceKillAck,
    InstanceProcessExit, InstanceSpawn, InstanceSpawnAck,
};

use crate::channel::OutboundMsg;
use crate::control::{ChatRegistry, ChatState};

/// instance 生命周期状态（§7.1 图）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    /// hello 成功（含双向认证，§9.2 步骤 2）。
    Registered,
    /// 心跳活跃。
    Online,
    /// 心跳超时（默认 30s）/ 连接断开。
    Offline,
}

impl InstanceState {
    /// 是否可接收指令（spawn/kill/forward_rpc）。
    pub fn can_serve(self) -> bool {
        matches!(self, InstanceState::Registered | InstanceState::Online)
    }

    /// 状态标签（脱敏日志）。
    pub fn as_str(self) -> &'static str {
        match self {
            InstanceState::Registered => "registered",
            InstanceState::Online => "online",
            InstanceState::Offline => "offline",
        }
    }
}

/// instance 在线连接句柄（fencing 后失效）。
#[derive(Debug, Clone)]
pub struct InstanceConn {
    /// 连接发送通道（gateway 的 ws 发送队列）。
    pub tx: mpsc::Sender<OutboundMsg>,
}

/// ack 回填（§4.5：spawn_ack/kill_ack/forward_ack 按 command_id 路由）。
#[derive(Debug, Clone, PartialEq)]
pub enum InstanceAck {
    /// `instance/spawn_ack`。
    Spawn(InstanceSpawnAck),
    /// `instance/kill_ack`。
    Kill(InstanceKillAck),
    /// `instance/forward_ack`（下行 JSON-RPC 转发确认，L1+L2，§4.4）。
    Forward(InstanceForwardAck),
    /// `instance/process_exit`（无 command_id，按 chat 路由）。
    ProcessExit(InstanceProcessExit),
}

/// hello 处理产物（补推协调输入，§4.5/§7.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloOutcome {
    /// 是否 fencing 了旧连接（§4.5 幂等替换）。
    pub fenced_previous: bool,
    /// `buffer_lost` 上报（daemon 崩溃缓冲丢失，§7.5）。
    pub buffer_lost: bool,
    /// instance 声称存活的 chat 清单（§8.3 对账输入）。
    pub alive_sessions: Vec<String>,
    /// per-chat 流纪元映射（§4.5.1）。
    pub chat_epochs: HashMap<String, u64>,
}

/// 指令下发结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnOutcome {
    /// spawn_ack 已回填。
    Acked(InstanceSpawnAck),
}

/// 指令下发结果。
#[derive(Debug, Clone, PartialEq)]
pub enum KillOutcome {
    /// kill_ack 已回填。
    Acked(InstanceKillAck),
}

/// instance 注册表错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstanceError {
    /// instance 未登记（hello 未到达）。
    #[error("instance not registered: {0}")]
    UnknownInstance(String),
    /// instance 离线（OFFLINE，§7.1）。
    #[error("instance offline")]
    Offline,
    /// 下发超时（spawn 10s / kill 10s / forward 10s，§6.2/§16）→ AGENT_UNAVAILABLE(retryable)。
    #[error("command timeout")]
    Timeout,
    /// instance 侧转发确认失败（`instance/forward_ack` ok=false：ACP 进程已
    /// 退出/管道关闭等）→ AGENT_UNAVAILABLE(retryable)。
    #[error("forward rejected: {0}")]
    ForwardRejected(String),
    /// 连接已 fencing/关闭。
    #[error("instance connection gone")]
    ConnectionGone,
}

/// instance 条目（进程内状态）。
struct InstanceEntry {
    state: InstanceState,
    token_id: String,
    hostname: String,
    conn: Option<mpsc::Sender<OutboundMsg>>,
    last_heartbeat: Instant,
    /// command_id → ack oneshot（spawn/kill ack 跟踪，§4.5）。
    pending_acks: HashMap<String, oneshot::Sender<InstanceAck>>,
    /// hello 上报的 per-chat 流纪元（§4.5.1；relay 入站校验输入）。
    chat_epochs: HashMap<String, u64>,
    /// 最近 hello 的 alive_sessions（对账）。
    alive_sessions: Vec<String>,
    /// 最近 hello 的 buffer_lost。
    buffer_lost: bool,
}

/// instance 生命周期注册表（§7.1）。
#[derive(Clone)]
pub struct InstanceRegistry {
    inner: Arc<InstanceInner>,
}

struct InstanceInner {
    instances: RwLock<HashMap<String, InstanceEntry>>,
    /// 离线判定超时（§16 默认 30s）。
    offline_timeout: Duration,
    /// spawn/kill 下发超时（§6.2/§16 默认 10s）。
    cmd_timeout: Duration,
    chats: ChatRegistry,
}

impl InstanceRegistry {
    /// 以离线超时与指令超时构建（§16：30s / 10s）。
    pub fn new(offline_timeout: Duration, cmd_timeout: Duration, chats: ChatRegistry) -> Self {
        InstanceRegistry {
            inner: Arc::new(InstanceInner {
                instances: RwLock::new(HashMap::new()),
                offline_timeout,
                cmd_timeout,
                chats,
            }),
        }
    }

    /// hello 处理（认证在 gateway 完成，§9.2；§4.5 幂等替换）：同 instance_id
    /// 新连接 → 旧连接 fencing（旧连接事件丢弃、关闭）；注册/替换连接与
    /// 对账输入。返回 [`HelloOutcome`]（补推协调与孤儿清理钩子输入，§7.5）。
    pub async fn on_hello(
        &self,
        instance_id: &str,
        token_id: &str,
        conn: InstanceConn,
        hello: &InstanceHello,
    ) -> HelloOutcome {
        let mut instances = self.inner.instances.write().await;
        let fenced = if let Some(old) = instances.get(instance_id) {
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
        // M1 hello 无存活清单字段（§4.5 表 instance/hello 无 alive_sessions；
        // 存活清单经 instance/heartbeat 上报，§8.3 对账在其后由心跳驱动）。
        let alive: Vec<String> = Vec::new();
        let buffer_lost = hello.buffer_lost.unwrap_or(false);
        instances.insert(
            instance_id.to_string(),
            InstanceEntry {
                state: InstanceState::Online,
                token_id: token_id.to_string(),
                hostname: hello.hostname.clone(),
                conn: Some(conn.tx),
                last_heartbeat: Instant::now(),
                pending_acks: HashMap::new(),
                chat_epochs: epochs.clone(),
                alive_sessions: alive.clone(),
                buffer_lost,
            },
        );
        let (token_id, hostname, buffer_lost) = {
            let entry = instances.get(instance_id).expect("just inserted");
            (
                entry.token_id.clone(),
                entry.hostname.clone(),
                entry.buffer_lost,
            )
        };
        drop(instances);
        info!(
            instance_id,
            token_id,
            hostname,
            fenced,
            buffer_lost,
            alive_sessions = alive.len(),
            "instance hello registered (idempotent replace)"
        );
        HelloOutcome {
            fenced_previous: fenced,
            buffer_lost,
            alive_sessions: alive,
            chat_epochs: epochs,
        }
    }

    /// 心跳更新（§7.1：5s；alive_sessions 供对账，§8.3）。
    ///
    /// alive_sessions 变化且非空时触发对账（§8.3 步骤 5：意外存活 → kill
    /// 裁决 §7.5、pending_close 补发 §7.6）——M1 hello 无存活清单字段
    /// （§4.5 表），对账的 alive 输入唯一来源是心跳；spawn 后台任务避免
    /// kill（每 chat 等 ack 最多 10s）阻塞 gateway 帧循环。
    pub async fn on_heartbeat(
        &self,
        instance_id: &str,
        hb: &InstanceHeartbeat,
    ) -> Result<(), InstanceError> {
        let changed = {
            let mut instances = self.inner.instances.write().await;
            let Some(entry) = instances.get_mut(instance_id) else {
                return Err(InstanceError::UnknownInstance(instance_id.to_string()));
            };
            entry.last_heartbeat = Instant::now();
            entry.state = InstanceState::Online;
            let changed = entry.alive_sessions != hb.alive_sessions;
            entry.alive_sessions = hb.alive_sessions.clone();
            changed
        };
        debug!(instance_id, load = hb.load, "instance heartbeat");
        if changed && !hb.alive_sessions.is_empty() {
            let me = self.clone();
            let mid = instance_id.to_string();
            let alive = hb.alive_sessions.clone();
            tokio::spawn(async move {
                me.reconcile_and_kill(&mid, &alive).await;
            });
        }
        Ok(())
    }

    /// 离线判定 tick（与心跳同 tick）：`offline_timeout` 无心跳 → OFFLINE；
    /// 返回本次离线集合（由 hub 联动 `RelayEventHandler::on_instance_disconnect`，
    /// §7.1 离线即刻生效）。
    ///
    /// **连接句柄保留**：心跳超时只代表判定离线，TCP 连接可能仍存活；若清
    /// 空 `conn`，机器心跳恢复 → ONLINE 后仍不可服务（conn 无恢复路径），
    /// 而机器不会重连（连接健康）——服务瘫痪。真正断开由
    /// [`Self::on_disconnect`]（连接结束路径）清句柄。
    pub async fn sweep_offline(&self, now: Instant) -> Vec<String> {
        let mut instances = self.inner.instances.write().await;
        let mut offline: Vec<String> = Vec::new();
        for (id, entry) in instances.iter_mut() {
            if entry.state == InstanceState::Offline {
                continue;
            }
            if now.duration_since(entry.last_heartbeat) >= self.inner.offline_timeout {
                entry.state = InstanceState::Offline;
                offline.push(id.clone());
                warn!(
                    instance_id = id,
                    timeout_ms = self.inner.offline_timeout.as_millis() as u64,
                    "instance offline (heartbeat timeout)"
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
    pub async fn on_disconnect(&self, instance_id: &str, conn: &InstanceConn) -> bool {
        let mut instances = self.inner.instances.write().await;
        let Some(entry) = instances.get_mut(instance_id) else {
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
        let was_online = entry.state != InstanceState::Offline;
        entry.state = InstanceState::Offline;
        entry.conn = None;
        was_online
    }

    /// 指令下发（§4.5）：发送 + ack 表登记 + 超时（spawn/kill 10s，§16）。
    /// 超时 → [`InstanceError::Timeout`]（→ AGENT_UNAVAILABLE retryable）；
    /// OFFLINE → [`InstanceError::Offline`]。
    pub async fn send_spawn(
        &self,
        instance_id: &str,
        cmd: InstanceSpawn,
    ) -> Result<SpawnOutcome, InstanceError> {
        let ack = self
            .send_command(
                instance_id,
                cmd.command_id.clone(),
                Frame::InstanceSpawn(cmd),
            )
            .await?;
        match ack {
            InstanceAck::Spawn(s) => Ok(SpawnOutcome::Acked(s)),
            _ => Err(InstanceError::ConnectionGone),
        }
    }

    /// 指令下发（§4.5）：kill + ack 跟踪（kill_ack；幂等——已死成功返回）。
    pub async fn send_kill(
        &self,
        instance_id: &str,
        cmd: InstanceKill,
    ) -> Result<KillOutcome, InstanceError> {
        let ack = self
            .send_command(
                instance_id,
                cmd.command_id.clone(),
                Frame::InstanceKill(cmd),
            )
            .await?;
        match ack {
            InstanceAck::Kill(k) => Ok(KillOutcome::Acked(k)),
            _ => Err(InstanceError::ConnectionGone),
        }
    }

    /// 指令下发统一路径：登记 ack oneshot → 发送 → 等 ack（超时
    /// `cmd_timeout`）。
    async fn send_command(
        &self,
        instance_id: &str,
        command_id: String,
        frame: Frame,
    ) -> Result<InstanceAck, InstanceError> {
        let (reply, rx) = oneshot::channel();
        let tx = {
            let mut instances = self.inner.instances.write().await;
            let Some(entry) = instances.get_mut(instance_id) else {
                return Err(InstanceError::UnknownInstance(instance_id.to_string()));
            };
            if !entry.state.can_serve() {
                return Err(InstanceError::Offline);
            }
            let Some(conn) = entry.conn.clone() else {
                return Err(InstanceError::Offline);
            };
            if entry.pending_acks.contains_key(&command_id) {
                // 重发（chat_id 幂等键，§4.5）：登记覆盖旧 oneshot。
                warn!(
                    instance_id,
                    command_id, "duplicate in-flight command ack slot replaced"
                );
            }
            entry.pending_acks.insert(command_id.clone(), reply);
            conn
        };
        if tx.send(OutboundMsg::Frame(frame)).await.is_err() {
            self.drop_pending_ack(instance_id, &command_id).await;
            return Err(InstanceError::ConnectionGone);
        }
        match tokio::time::timeout(self.inner.cmd_timeout, rx).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) => {
                self.drop_pending_ack(instance_id, &command_id).await;
                Err(InstanceError::ConnectionGone)
            }
            Err(_) => {
                self.drop_pending_ack(instance_id, &command_id).await;
                Err(InstanceError::Timeout)
            }
        }
    }

    /// ack 路由（spawn_ack/kill_ack，§4.5）：按 command_id 回填 oneshot。
    /// 返回是否匹配到在途命令。
    pub async fn on_ack(&self, instance_id: &str, command_id: &str, ack: InstanceAck) -> bool {
        let mut instances = self.inner.instances.write().await;
        let Some(entry) = instances.get_mut(instance_id) else {
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
    /// L1+L2 合并确认由 `instance/forward_ack` 承载，§4.4 M1 合并）。
    ///
    /// 【冲突 1 裁决】下行载体由裸 JSON-RPC 文本改为 `instance/forward` 帧
    /// （M1 instance 帧集新增；instance 写 ACP stdin 成功回 forward_ack）。
    /// `command_id` 取消息 `id`（rpcId，server 生成，`hub-{n}` 全局单调），
    /// ack 路由与 spawn/kill 同表。
    pub async fn forward_rpc(
        &self,
        instance_id: &str,
        chat_id: &str,
        msg: &serde_json::Value,
    ) -> Result<(), InstanceError> {
        // #1 官方 request_permission 响应帧的 id = agent request id 原样回显
        // （常为数字自增）——`as_str` 失败不得误判 ConnectionGone：string/
        // number 均提取（字符串化仅用于 instance 内部 ack 路由 command_id，
        // 不改变写 ACP stdin 的帧内容）。
        let command_id = msg
            .get("id")
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .ok_or(InstanceError::ConnectionGone)?;
        let frame = Frame::InstanceForward(acp_hub_proto::instance::InstanceForward {
            command_id: command_id.to_string(),
            chat_id: chat_id.to_string(),
            frame: msg.clone(),
        });
        let ack = self
            .send_command(instance_id, command_id.to_string(), frame)
            .await?;
        match ack {
            InstanceAck::Forward(f) if f.ok => Ok(()),
            InstanceAck::Forward(f) => {
                Err(InstanceError::ForwardRejected(f.error.unwrap_or_default()))
            }
            _ => Err(InstanceError::ConnectionGone),
        }
    }

    /// notification 透传（§4.3 session/cancel：无 id、无响应帧）：无 ack 路由
    /// ——仅校验 instance 在线并发送（L1 转发确认）；instance 侧写 ACP stdin 后
    /// 回的 forward_ack 无 pending 匹配，幂等丢弃。写盘成功与否由调用方按
    /// notification 语义处理（发送成功即确认，§7.2）。
    pub async fn forward_notification(
        &self,
        instance_id: &str,
        chat_id: &str,
        msg: &serde_json::Value,
    ) -> Result<(), InstanceError> {
        let frame = Frame::InstanceForward(acp_hub_proto::instance::InstanceForward {
            command_id: String::new(),
            chat_id: chat_id.to_string(),
            frame: msg.clone(),
        });
        let tx = {
            let mut instances = self.inner.instances.write().await;
            let Some(entry) = instances.get_mut(instance_id) else {
                return Err(InstanceError::UnknownInstance(instance_id.to_string()));
            };
            if !entry.state.can_serve() {
                return Err(InstanceError::Offline);
            }
            let Some(conn) = entry.conn.clone() else {
                return Err(InstanceError::Offline);
            };
            conn
        };
        if tx.send(OutboundMsg::Frame(frame)).await.is_err() {
            return Err(InstanceError::ConnectionGone);
        }
        Ok(())
    }

    /// hello 上报的 per-chat 流纪元查询（relay 入站 epoch 校验，§4.5.1）。
    pub async fn chat_epoch(&self, instance_id: &str, chat_id: &str) -> Option<u64> {
        self.inner
            .instances
            .read()
            .await
            .get(instance_id)
            .and_then(|e| e.chat_epochs.get(chat_id).copied())
    }

    /// instance 状态查询（诊断/测试）。
    pub async fn state(&self, instance_id: &str) -> Option<InstanceState> {
        self.inner
            .instances
            .read()
            .await
            .get(instance_id)
            .map(|e| e.state)
    }

    /// 孤儿进程清理钩子（§7.5）：hello 后对「server 已标记终态/未登记但
    /// instance 声称存活」的 chat 补发 kill（实际进程清理在 instance 侧；
    /// server 只负责下发与 ack 跟踪）。
    ///
    /// 返回已下发 kill 的 chat 清单。
    pub async fn cleanup_orphans(&self, instance_id: &str, outcome: &HelloOutcome) -> Vec<String> {
        let report = match self
            .inner
            .chats
            .reconcile_alive(instance_id, &outcome.alive_sessions)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(instance_id, error = ?e, "orphan cleanup reconcile failed");
                return Vec::new();
            }
        };
        self.kill_chats(instance_id, &report.to_kill).await
    }

    /// 心跳驱动的对账（§8.3 步骤 5）：alive_sessions 与 Registry 比对 →
    /// 摘要日志 + to_kill 逐个补发 kill（§7.5 意外存活裁决 + §7.6
    /// pending_close 补发）。
    pub async fn reconcile_and_kill(&self, instance_id: &str, alive: &[String]) {
        let report = match self.inner.chats.reconcile_alive(instance_id, alive).await {
            Ok(r) => r,
            Err(e) => {
                warn!(instance_id, error = ?e, "heartbeat reconciliation failed");
                return;
            }
        };
        self.kill_chats(instance_id, &report.to_kill).await;
    }

    /// kill 裁决下发（§7.5/§7.6）：对 `to_kill` 逐个补发 `instance/kill`
    /// （幂等，已死成功返回），成功后 chat 置 Closed（「Registry 标记已清理」）。
    async fn kill_chats(&self, instance_id: &str, to_kill: &[String]) -> Vec<String> {
        let mut killed = Vec::new();
        for sid in to_kill {
            let command_id = uuid::Uuid::new_v4().to_string();
            let cmd = InstanceKill {
                command_id: command_id.clone(),
                chat_id: sid.clone(),
                grace: None,
            };
            match self.send_kill(instance_id, cmd).await {
                Ok(_) => {
                    killed.push(sid.clone());
                    // 意外存活/终态清理完成 → chat 置 Closed（§7.5「Registry
                    // 标记已清理」；pending_close 集合在 transition(Closed)
                    // 中清除，§7.6）。
                    if let Ok(()) = self.inner.chats.transition(sid, ChatState::Closed).await {
                        // noop
                    }
                }
                Err(e) => warn!(instance_id, chat_id = sid, error = ?e, "orphan kill failed"),
            }
        }
        if !killed.is_empty() {
            info!(
                instance_id,
                killed = killed.len(),
                "orphan chats killed (§7.5)"
            );
        }
        killed
    }

    async fn drop_pending_ack(&self, instance_id: &str, command_id: &str) {
        let mut instances = self.inner.instances.write().await;
        if let Some(entry) = instances.get_mut(instance_id) {
            entry.pending_acks.remove(command_id);
        }
    }
}

#[cfg(test)]
#[path = "instance_registry_test.rs"]
mod instance_registry_test;
