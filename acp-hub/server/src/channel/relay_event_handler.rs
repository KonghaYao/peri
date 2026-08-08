//! machine 入站事件消费与断链清理（架构 §4.5/§6.1/§8.2/§8.5，设计稿
//! `f5-channel-control.md` §8）。
//!
//! 入站链路：epoch 校验（防御，§4.5.1）→ binding 校验（§6.1 规则 5）→
//! ACPChannel 规范化 → `DocManager::submit_event`（F4 单写者 + 微批次 +
//! 落盘）。`RpcResponse`（L3）经 pending_rpc 表匹配通知 coordinator（§4.4）。
//!
//! **持久化澄清**（设计稿 §8 注）：machine 入站事件**不进 outbox**（outbox
//! 是命令账本，§4.4）；入站事件的持久化 = 经 DocManager → UpdateSink 落
//! update 日志 + `(epoch, last_seq)` 水位——此即补推起点事实源
//! （`from_seq = last_seq + 1`，§8.5）。
//!
//! 断链清理（§8.2 matrix machine 行 + §7.1 离线即刻生效）：该 machine 全部
//! 活 session → 活动 turn `MarkTurnInterrupted`、`registry.set_session_gap`
//! 置标记（缺口数量由补推时聚合器精确计算）、session 状态 Gap。
//! **遗留**：pending 权限批量 expired（§7.1）需 F4 提供枚举/批量 CAS 命令
//! （本模块无 Doc 读取接口），断链时保持 pending（gap 期间只读，补推/新事件
//! 驱动），已记录输出。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{oneshot, RwLock};
use tracing::{debug, info, warn};

use acp_hub_proto::machine::{MachineBufferSync, MachineEvent, MachineProcessExit};

use crate::protocol::{AcpChannel, NormalizeOutcome};
use crate::state::doc_manager::{DocManager, SubmitError, SubmitResult};
use crate::state::normalized::NormalizedEvent;
use crate::state::registry::{DegradeCause, RegistryState};
use crate::state::doc_manager::DocCommand;

use crate::control::MachineRegistry;
use crate::control::{SessionRegistry, SessionState};

/// 消费结果（gateway 记录日志/计数用；脱敏，不携带正文）。
#[derive(Debug, Clone, PartialEq)]
pub enum ConsumeResult {
    /// 已投递聚合器（`applied=false` 表示聚合器拒绝——幂等/守卫/防御，按
    /// reason 计数，§6.3）。
    Delivered {
        /// hub 侧 session_id。
        session_id: String,
        /// 事件种类（脱敏）。
        kind: &'static str,
        /// machine 侧 seq。
        seq: u64,
        /// 聚合器是否接受。
        applied: bool,
    },
    /// JSON-RPC response 匹配 pending_rpc（L3 确认，§4.4）。
    RpcConfirmed {
        /// 关联 command_id。
        command_id: String,
        /// 完整 response（coordinator 解析 result/error）。
        response: serde_json::Value,
    },
    /// 单帧丢弃 + 原因（§4.5.1 防御；计数）。
    Dropped {
        /// 稳定原因（脱敏）。
        reason: &'static str,
    },
    /// 整批拒绝（buffer_sync epoch 不符，§4.5.1）。
    BatchRejected {
        /// 稳定原因。
        reason: &'static str,
    },
    /// 事件已投递但落盘失败（§17.2 degraded 输入）。
    PersistFailed {
        /// hub 侧 session_id。
        session_id: String,
    },
}

/// relay 错误（断链清理面）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    /// Registry 写回失败。
    #[error("registry write failed: {0}")]
    Registry(String),
    /// DocManager 提交拒绝（session 不存在/已关闭）。
    #[error("submit rejected: {0}")]
    Submit(String),
}

/// pending_rpc 条目（L3 确认，§4.4）。
#[derive(Debug)]
pub struct PendingRpc {
    /// 关联 command_id（coordinator 登记）。
    pub command_id: String,
    /// 响应通知通道（coordinator 等待侧；`None` = 已超时清理）。
    notify: Option<oneshot::Sender<serde_json::Value>>,
}

/// machine 入站事件消费（§4.5）。
#[derive(Clone)]
pub struct RelayEventHandler {
    inner: Arc<RelayInner>,
}

struct RelayInner {
    doc: Arc<DocManager>,
    sessions: SessionRegistry,
    machine: Arc<MachineRegistry>,
    registry: RegistryState,
    channel: AcpChannel,
    /// pending_rpc 表（rpc_id → command_id；L3 确认，§4.4）——与 coordinator
    /// 共享的 in-memory 表（设计稿【决策】放本模块，coordinator 登记、本模块
    /// 匹配）。
    pending_rpc: RwLock<HashMap<String, PendingRpc>>,
    /// 丢弃计数（§17.1 指标；按原因）。
    dropped: AtomicU64,
}

impl RelayEventHandler {
    /// 装配（hub 调用；`AcpChannel` 以默认权限超时 5min 构建，§16）。
    pub fn new(
        doc: Arc<DocManager>,
        sessions: SessionRegistry,
        machine: Arc<MachineRegistry>,
        registry: RegistryState,
    ) -> Self {
        RelayEventHandler {
            inner: Arc::new(RelayInner {
                doc,
                sessions,
                machine,
                registry,
                channel: AcpChannel::default(),
                pending_rpc: RwLock::new(HashMap::new()),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    /// `machine/event` 消费（§4.5）。
    ///
    /// 链路：epoch 校验（hello 上报的 stream_epochs；无记录 → 放行，聚合器
    /// 防御兜底）→ binding 校验（§6.1）→ normalize → submit_event。
    pub async fn on_machine_event(&self, machine_id: &str, ev: &MachineEvent) -> ConsumeResult {
        // 1. epoch 校验（§4.5.1 防御；正常路径 hello 已对账）。
        if let Some(expected) = self
            .inner
            .machine
            .session_epoch(machine_id, &ev.session_id)
            .await
        {
            if expected != ev.epoch {
                self.count_dropped("epoch_mismatch");
                return ConsumeResult::Dropped {
                    reason: "epoch_mismatch",
                };
            }
        }
        // 2. binding 校验（§6.1 规则 5 / §6.5：binding 前帧一律丢弃）。
        let Some(hub_session_id) = self.inner.sessions.resolve(&ev.session_id).await else {
            // binding 缺失：**JSON-RPC response 例外**（§4.4 L3 确认不依赖
            // binding）——create 序列 initialize/session/new 的响应在
            // binding 建立前到达（§6.2），经 pending_rpc（rpc_id → command_id）
            // 匹配，无 session 语义。其余帧按 §6.1 丢弃。
            let now = chrono::Utc::now().to_rfc3339();
            match self
                .inner
                .channel
                .normalize("", ev.epoch, ev.seq, &now, &ev.frame)
            {
                NormalizeOutcome::RpcResponse { id, is_error } => {
                    return self.confirm_rpc(&id, ev.frame.clone(), is_error).await;
                }
                _ => {
                    self.count_dropped("binding_missing");
                    return ConsumeResult::Dropped {
                        reason: "binding_missing",
                    };
                }
            }
        };
        // 3. 规范化（§6.1）。
        let now = chrono::Utc::now().to_rfc3339();
        match self
            .inner
            .channel
            .normalize(&hub_session_id, ev.epoch, ev.seq, &now, &ev.frame)
        {
            NormalizeOutcome::Event(nev) => {
                self.submit(&hub_session_id, nev).await
            }
            NormalizeOutcome::RpcResponse { id, is_error } => {
                self.confirm_rpc(&id, ev.frame.clone(), is_error).await
            }
            NormalizeOutcome::Dropped(reason) => {
                self.count_dropped(reason.as_str());
                ConsumeResult::Dropped {
                    reason: reason.as_str(),
                }
            }
        }
    }

    /// `machine/buffer_sync` 消费（§8.5 补推纪律）。
    ///
    /// epoch 校验（与 hello 上报的 stream_epochs 不一致 → 拒绝整批，§4.5.1）
    /// → 逐帧按 from_seq 连续性投递（乱序/重复丢弃计数——聚合器幂等兜底）→
    /// 排空完成判定（设计稿决策 4：server 不做额外结束帧；gap 的精确计数与
    /// 追平清除由 F4 聚合器 `judge_stream`/gap_dirty → registry 写回）。
    pub async fn on_buffer_sync(&self, machine_id: &str, sync: &MachineBufferSync) -> ConsumeResult {
        // 1. epoch 校验（与 server 记录不一致即拒绝该批，§4.5.1）。
        if let Some(expected) = self
            .inner
            .machine
            .session_epoch(machine_id, &sync.session_id)
            .await
        {
            if expected != sync.epoch {
                self.count_dropped("buffer_sync_epoch_mismatch");
                return ConsumeResult::BatchRejected {
                    reason: "buffer_sync_epoch_mismatch",
                };
            }
        }
        // 2. binding 校验（§6.1）。
        let Some(hub_session_id) = self.inner.sessions.resolve(&sync.session_id).await else {
            self.count_dropped("binding_missing");
            return ConsumeResult::BatchRejected {
                reason: "binding_missing",
            };
        };
        // 3. from_seq 连续性（乱序/重复 → 丢弃计数，§8.5 纪律）。
        let mut expected_seq = sync.from_seq;
        let mut delivered = 0usize;
        let mut rejected = 0usize;
        let now = chrono::Utc::now().to_rfc3339();
        for bf in &sync.frames {
            if bf.seq != expected_seq {
                rejected += 1;
                self.count_dropped("buffer_sync_out_of_order");
                continue;
            }
            expected_seq = bf.seq + 1;
            match self
                .inner
                .channel
                .normalize(&hub_session_id, sync.epoch, bf.seq, &now, &bf.frame)
            {
                NormalizeOutcome::Event(nev) => {
                    match self.submit(&hub_session_id, nev).await {
                        ConsumeResult::Delivered { .. } => delivered += 1,
                        ConsumeResult::Dropped { reason } => {
                            rejected += 1;
                            self.count_dropped(reason);
                        }
                        _ => {}
                    }
                }
                NormalizeOutcome::RpcResponse { id, is_error } => {
                    self.confirm_rpc(&id, bf.frame.clone(), is_error).await;
                }
                NormalizeOutcome::Dropped(reason) => {
                    rejected += 1;
                    self.count_dropped(reason.as_str());
                }
            }
        }
        debug!(
            session_id = hub_session_id, epoch = sync.epoch, from_seq = sync.from_seq,
            delivered, rejected,
            "buffer_sync consumed"
        );
        if delivered == 0 && rejected > 0 {
            ConsumeResult::BatchRejected {
                reason: "all_frames_rejected",
            }
        } else {
            ConsumeResult::Delivered {
                session_id: hub_session_id,
                kind: "buffer_sync",
                seq: sync.from_seq,
                applied: rejected == 0,
            }
        }
    }

    /// 断链清理（§8.2 matrix machine 行 + §7.1 离线即刻生效）：
    /// 该 machine 全部活 session：活动 turn → `MarkTurnInterrupted`（DocCommand）、
    /// session 置 gap 标记（registry；缺口数量由补推时聚合器精确计算）、
    /// 状态迁移 Gap。
    pub async fn on_machine_disconnect(&self, machine_id: &str) -> Result<(), RelayError> {
        let sessions = self
            .inner
            .sessions
            .sessions_for_machine(machine_id)
            .await;
        let mut interrupted = 0usize;
        let mut gapped = 0usize;
        for (session_id, state) in &sessions {
            if state.is_terminal() {
                continue;
            }
            // 活动 turn → interrupted（§7.1；turn_id 由 coordinator 登记）。
            if let Some(turn_id) = self.inner.sessions.active_turn(session_id).await {
                let result = self
                    .inner
                    .doc
                    .submit_command(
                        session_id,
                        DocCommand::MarkTurnInterrupted {
                            turn_id: turn_id.clone(),
                        },
                    )
                    .await;
                if matches!(result, SubmitResult::Applied(_)) {
                    interrupted += 1;
                } else if matches!(result, SubmitResult::Rejected(SubmitError::SessionNotFound)) {
                    // session writer 未打开：仅记录（视图缺失由 gap 呈现）。
                    warn!(session_id, "turn interrupt skipped: session writer absent");
                }
            }
            // gap 标记（§8.2/§7.3：补推完成、seq 追平后清除）。
            let _ = self
                .inner
                .registry
                .set_session_gap(session_id, Some(0))
                .await;
            let _ = self
                .inner
                .registry
                .report_condition(DegradeCause::SessionGap)
                .await;
            if self
                .inner
                .sessions
                .transition(session_id, SessionState::Gap)
                .await
                .is_ok()
            {
                gapped += 1;
            }
        }
        info!(
            machine_id, sessions = sessions.len(), interrupted, gapped,
            "machine disconnect cleanup complete"
        );
        Ok(())
    }

    /// `machine/process_exit` 消费（§4.5）：终态写视图（F4 `SetSessionTerminal`）、
    /// session 状态迁移（ended/crashed，§7.3）；不再接受新事件（聚合器终态
    /// 守卫，§8.2）。
    pub async fn on_process_exit(&self, machine_id: &str, exit: &MachineProcessExit) -> ConsumeResult {
        let _ = machine_id;
        let Some(hub_session_id) = self.inner.sessions.resolve(&exit.session_id).await else {
            self.count_dropped("binding_missing");
            return ConsumeResult::Dropped {
                reason: "binding_missing",
            };
        };
        let status = if exit.code == 0 {
            acp_hub_proto::schema::SessionStatus::Ended
        } else {
            acp_hub_proto::schema::SessionStatus::Crashed
        };
        let state = if exit.code == 0 {
            SessionState::Ended
        } else {
            SessionState::Crashed
        };
        let _ = self
            .inner
            .doc
            .submit_command(
                &hub_session_id,
                DocCommand::SetSessionTerminal { status },
            )
            .await;
        let _ = self.inner.sessions.transition(&hub_session_id, state).await;
        self.inner.sessions.clear_active_turn(&hub_session_id).await;
        ConsumeResult::Delivered {
            session_id: hub_session_id,
            kind: "process_exit",
            seq: 0,
            applied: true,
        }
    }

    /// rpcId 登记（coordinator 调用；返回等待侧 oneshot）。
    pub async fn register_rpc(
        &self,
        rpc_id: &str,
        command_id: String,
    ) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.inner.pending_rpc.write().await.insert(
            rpc_id.to_string(),
            PendingRpc {
                command_id,
                notify: Some(tx),
            },
        );
        rx
    }

    /// rpcId 撤销（coordinator L3 超时后调用，§4.4 路径 B）：移除表项，防
    /// 永不回应的 rpc 累积泄漏。若 response 恰好在撤销前已匹配（表项已取走）
    /// → 无操作（幂等）。
    pub async fn cancel_rpc(&self, rpc_id: &str) {
        self.inner.pending_rpc.write().await.remove(rpc_id);
    }

    /// L3 匹配：`RpcResponse{id}` → pending_rpc 命中 → 通知 coordinator →
    /// 移除表项。
    async fn confirm_rpc(
        &self,
        rpc_id: &str,
        response: serde_json::Value,
        _is_error: bool,
    ) -> ConsumeResult {
        let entry = self.inner.pending_rpc.write().await.remove(rpc_id);
        match entry {
            Some(pending) => {
                if let Some(tx) = pending.notify {
                    let _ = tx.send(response.clone());
                }
                ConsumeResult::RpcConfirmed {
                    command_id: pending.command_id,
                    response,
                }
            }
            None => ConsumeResult::Dropped {
                reason: "rpc_id_unknown",
            },
        }
    }

    /// 丢弃计数（§17.1 指标；供日志与测试断言）。
    pub fn dropped_total(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// pending_rpc 表大小（诊断/测试）。
    pub async fn pending_rpc_len(&self) -> usize {
        self.inner.pending_rpc.read().await.len()
    }

    fn count_dropped(&self, _reason: &'static str) {
        self.inner.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// 规范化事件投递（delta 类入队即返；控制类挂 oneshot 等落盘）。
    async fn submit(&self, session_id: &str, nev: NormalizedEvent) -> ConsumeResult {
        let kind = nev.kind();
        let seq = nev.seq;
        match self.inner.doc.submit_event(nev).await {
            SubmitResult::Applied(r) => ConsumeResult::Delivered {
                session_id: session_id.to_string(),
                kind,
                seq,
                applied: r.applied,
            },
            SubmitResult::Rejected(_) => ConsumeResult::Dropped {
                reason: "submit_rejected",
            },
            SubmitResult::PersistFailed => ConsumeResult::PersistFailed {
                session_id: session_id.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "relay_event_handler_test.rs"]
mod relay_event_handler_test;
