//! instance 入站事件消费与断链清理（架构 §4.5/§6.1/§8.2/§8.5，设计稿
//! `f5-channel-control.md` §8）。
//!
//! 入站链路：epoch 校验（防御，§4.5.1）→ binding 校验（§6.1 规则 5）→
//! ACPChannel 规范化 → `DocManager::submit_event`（F4 单写者 + 微批次 +
//! 落盘）。`RpcResponse`（L3）经 pending_rpc 表匹配通知 coordinator（§4.4）。
//!
//! **持久化澄清**（设计稿 §8 注）：instance 入站事件**不进 outbox**（outbox
//! 是命令账本，§4.4）；入站事件的持久化 = 经 DocManager → UpdateSink 落
//! update 日志 + `(epoch, last_seq)` 水位——此即补推起点事实源
//! （`from_seq = last_seq + 1`，§8.5）。
//!
//! 断链清理（§8.2 matrix instance 行 + §7.1 离线即刻生效）：该 instance 全部
//! 活 chat → 活动 turn `MarkTurnInterrupted`、`registry.set_chat_gap`
//! 置标记（缺口数量由补推时聚合器精确计算）、chat 状态 Gap。
//! **遗留**：pending 权限批量 expired（§7.1）需 F4 提供枚举/批量 CAS 命令
//! （本模块无 Doc 读取接口），断链时保持 pending（gap 期间只读，补推/新事件
//! 驱动），已记录输出。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{oneshot, RwLock};
use tracing::{debug, info, warn};

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::instance::{InstanceBufferSync, InstanceEvent, InstanceProcessExit};
use acp_hub_proto::schema::PermissionOptions;

use crate::protocol::{
    extract_session_id, AcpChannel, NormalizeOutcome, PermissionRequestFields, PERMISSION_TIMEOUT,
};
use crate::state::doc_manager::DocCommand;
use crate::state::doc_manager::{DocManager, SubmitError, SubmitResult};
use crate::state::normalized::NormalizedEvent;
use crate::state::registry::{DegradeCause, RegistryState};

use crate::control::InstanceRegistry;
use crate::control::{ChatRegistry, ChatState};

/// 消费结果（gateway 记录日志/计数用；脱敏，不携带正文）。
#[derive(Debug, Clone, PartialEq)]
pub enum ConsumeResult {
    /// 已投递聚合器（`applied=false` 表示聚合器拒绝——幂等/守卫/防御，按
    /// reason 计数，§6.3）。
    Delivered {
        /// hub 侧 chat_id。
        chat_id: String,
        /// 事件种类（脱敏）。
        kind: &'static str,
        /// instance 侧 seq。
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
        /// hub 侧 chat_id。
        chat_id: String,
    },
}

/// relay 错误（断链清理面）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    /// Registry 写回失败。
    #[error("registry write failed: {0}")]
    Registry(String),
    /// DocManager 提交拒绝（chat 不存在/已关闭）。
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

/// pending_permission 表条目（#1：官方 `session/request_permission` 响应回
/// 投数据；key = server 生成 permission_id）。
#[derive(Debug, Clone)]
pub struct PendingPermissionReq {
    /// agent 的 request id（响应帧 id 原样回显）。
    pub request_id: serde_json::Value,
    /// 官方 options 原样（响应 optionId 回显选档）。
    pub options: Vec<serde_json::Value>,
    /// 归属 chat（断链/进程退出清理用）。
    pub chat_id: String,
    /// 首次裁决的 commandId。明确未送达时只允许这一条命令
    /// 恢复，防止新 commandId 把同一安全副作用重放。
    resolving_command_id: Option<String>,
    /// 与 `resolving_command_id` 绑定的原始决策。
    resolving_decision: Option<PermissionDecision>,
}

/// instance 入站事件消费（§4.5）。
#[derive(Clone)]
pub struct RelayEventHandler {
    inner: Arc<RelayInner>,
}

struct RelayInner {
    doc: Arc<DocManager>,
    chats: ChatRegistry,
    instance: Arc<InstanceRegistry>,
    registry: RegistryState,
    channel: AcpChannel,
    /// pending_rpc 表（rpc_id → command_id；L3 确认，§4.4）——与 coordinator
    /// 共享的 in-memory 表（设计稿【决策】放本模块，coordinator 登记、本模块
    /// 匹配）。
    pending_rpc: RwLock<HashMap<String, PendingRpc>>,
    /// pending_permissions 表（permission_id → 官方 request 回投数据；#1，
    /// 与 pending_rpc 并列，coordinator resolve 时一次性 take）。
    pending_permissions: RwLock<HashMap<String, PendingPermissionReq>>,
    /// 丢弃计数（§17.1 指标；按原因）。
    dropped: AtomicU64,
}

impl RelayEventHandler {
    /// 装配（hub 调用；`AcpChannel` 以默认权限超时 5min 构建，§16）。
    pub fn new(
        doc: Arc<DocManager>,
        chats: ChatRegistry,
        instance: Arc<InstanceRegistry>,
        registry: RegistryState,
    ) -> Self {
        RelayEventHandler {
            inner: Arc::new(RelayInner {
                doc,
                chats,
                instance,
                registry,
                channel: AcpChannel::default(),
                pending_rpc: RwLock::new(HashMap::new()),
                pending_permissions: RwLock::new(HashMap::new()),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    /// `instance/event` 消费（§4.5）。
    ///
    /// 链路：epoch 校验（hello 上报的 stream_epochs；无记录 → 放行，聚合器
    /// 防御兜底）→ binding 校验（§6.1）→ normalize → submit_event。
    pub async fn on_instance_event(&self, instance_id: &str, ev: &InstanceEvent) -> ConsumeResult {
        // 1. epoch 校验（§4.5.1 防御；正常路径 hello 已对账）。
        //    信封 chat_id = instance 进程归属（hub chat id，spawn 时
        //    确立，§4.5.1）；instance hello 上报 stream_epochs 同为该键。
        if let Some(expected) = self
            .inner
            .instance
            .chat_epoch(instance_id, &ev.chat_id)
            .await
        {
            if expected != ev.epoch {
                self.count_dropped("epoch_mismatch");
                return ConsumeResult::Dropped {
                    reason: "epoch_mismatch",
                };
            }
        }
        // 2. binding 校验（§6.1 规则 5 / §6.5 / §495）：**ACP 帧内携带的
        //    sessionId**（acp_session_id，test-child/真实 ACP 自建 id）必须
        //    命中可信 binding（acp_session_id → hub chat_id）且映射回本
        //    信封 chat。信封本身是 instance 进程归属（dumb pipe 不翻译 id，
        //    §3.3），不再作 binding 查询键。
        //    JSON-RPC 形态例外（#5，与 child.rs C1 同判据——有 jsonrpc 键
        //    的 response/request/notification 无帧内 sessionId 语义）：
        //    create 序列 initialize/session/new 的响应（§4.4 L3 经 pending_rpc
        //    匹配）、官方 session/request_permission request（#1，params.
        //    sessionId 必填但防御性统一）、agent/status 通知（instance 级
        //    事件）按信封兜底投递；其余帧按 §6.1 丢弃。
        let hub_chat_id = ev.chat_id.clone();
        let binding_ok = match extract_session_id(&ev.frame) {
            Some(acp_id) => matches!(
                self.inner.chats.resolve(&acp_id).await,
                Some(mapped) if mapped == hub_chat_id
            ),
            None => false,
        };
        if !binding_ok {
            // 无帧内 sessionId（或未命中 binding）：仅 JSON-RPC 形态（有
            // jsonrpc 键，与 child.rs C1 同判据）按信封兜底投递。
            if ev.frame.get("jsonrpc").is_none() {
                self.count_dropped("binding_missing");
                return ConsumeResult::Dropped {
                    reason: "binding_missing",
                };
            }
            // 方法面帧（request/notification：官方 request_permission、
            // agent/status 等）进入 chat 作用域投递（register_permission_
            // request / submit），以「信封 chat 存在」为唯一校验——信封是
            // spawn 时确立的进程归属（§4.5.1，客户端不可控）。JSON-RPC
            // response 例外（§4.4 L3 经 pending_rpc 匹配，无 chat 语义，
            // 历史行为保留：不要求信封 chat 登记）。
            if ev.frame.get("method").is_some()
                && self.inner.chats.entry(&hub_chat_id).await.is_none()
            {
                self.count_dropped("binding_missing");
                return ConsumeResult::Dropped {
                    reason: "binding_missing",
                };
            }
            // 投递路径与下方 Some 分支合并（不再 normalize("") 兜底）：
            // RpcResponse → confirm_rpc；PermissionRequest →
            // register_permission_request；Event → submit；Dropped → 计数。
        }
        // 3. 规范化（§6.1）。
        let now = chrono::Utc::now().to_rfc3339();
        match self
            .inner
            .channel
            .normalize(&hub_chat_id, ev.epoch, ev.seq, &now, &ev.frame)
        {
            NormalizeOutcome::Event(nev) => {
                let r = self.submit(&hub_chat_id, *nev).await;
                // 断链追平恢复（§7.3/§8.5）：实时帧投递成功 → 尝试清除
                // 断链置的 gap 标记并恢复 chat 可用。判定（可校准/不可
                // 校准）在 writer 内以聚合器事实源进行——uncalibratable
                // chat 的事件被聚合器拒绝（applied=false），不会误恢复。
                if matches!(r, ConsumeResult::Delivered { applied: true, .. }) {
                    self.recover_from_gap(&hub_chat_id).await;
                }
                r
            }
            // #1 官方 request_permission：登记 pending 表 + 投递投影。
            NormalizeOutcome::PermissionRequest(req) => {
                let r = self
                    .register_permission_request(&hub_chat_id, ev.epoch, ev.seq, &now, &req)
                    .await;
                // 断链追平恢复（评审 P2-2：与 Event 分支 227-229 对称）：
                // 实时帧投递成功 → 尝试清除断链置的 gap 标记并恢复 chat
                // 可用（判定在 writer 内，applied=false 不会误恢复）。
                if matches!(r, ConsumeResult::Delivered { applied: true, .. }) {
                    self.recover_from_gap(&hub_chat_id).await;
                }
                r
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

    /// `instance/buffer_sync` 消费（§8.5 补推纪律）。
    ///
    /// epoch 校验（与 hello 上报的 stream_epochs 不一致 → 拒绝整批，§4.5.1）
    /// → 逐帧按 from_seq 连续性投递（乱序/重复丢弃计数——聚合器幂等兜底）→
    /// 排空完成判定（设计稿决策 4：server 不做额外结束帧；gap 的精确计数与
    /// 追平清除由 F4 聚合器 `judge_stream`/gap_dirty → registry 写回）。
    pub async fn on_buffer_sync(
        &self,
        instance_id: &str,
        sync: &InstanceBufferSync,
    ) -> ConsumeResult {
        // 1. epoch 校验（与 server 记录不一致即拒绝该批，§4.5.1）。
        if let Some(expected) = self
            .inner
            .instance
            .chat_epoch(instance_id, &sync.chat_id)
            .await
        {
            if expected != sync.epoch {
                self.count_dropped("buffer_sync_epoch_mismatch");
                return ConsumeResult::BatchRejected {
                    reason: "buffer_sync_epoch_mismatch",
                };
            }
        }
        // 2. binding 校验（§6.1/§495）：信封 chat_id = instance 进程归属
        //    （hub chat id，§4.5.1）；帧内 sessionId 逐帧对照可信 binding
        //    （acp_session_id → hub chat_id）。JSON-RPC 形态例外（#5，与
        //    on_instance_event 同判据）按信封兜底（§4.4 L3 / #1 request /
        //    agent/status 通知）。
        let hub_chat_id = sync.chat_id.clone();
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
            let binding_ok = match extract_session_id(&bf.frame) {
                Some(acp_id) => matches!(
                    self.inner.chats.resolve(&acp_id).await,
                    Some(mapped) if mapped == hub_chat_id
                ),
                None => false,
            };
            if !binding_ok {
                // 同构（on_instance_event C2）：无帧内 sessionId 的 JSON-RPC
                // 形态帧（有 jsonrpc 键）按信封兜底（投递路径与下方 Some
                // 分支合并）；方法面帧另要求信封 chat 登记；原始形态仍拒
                // （§6.1）。
                if bf.frame.get("jsonrpc").is_none()
                    || (bf.frame.get("method").is_some()
                        && self.inner.chats.entry(&hub_chat_id).await.is_none())
                {
                    rejected += 1;
                    self.count_dropped("binding_missing");
                    continue;
                }
            }
            match self
                .inner
                .channel
                .normalize(&hub_chat_id, sync.epoch, bf.seq, &now, &bf.frame)
            {
                NormalizeOutcome::Event(nev) => match self.submit(&hub_chat_id, *nev).await {
                    ConsumeResult::Delivered { .. } => delivered += 1,
                    ConsumeResult::Dropped { reason } => {
                        rejected += 1;
                        self.count_dropped(reason);
                    }
                    _ => {}
                },
                // #1 官方 request_permission：登记 pending 表 + 投递投影
                // （补推路径同构，与实时帧一致）。
                NormalizeOutcome::PermissionRequest(req) => {
                    match self
                        .register_permission_request(&hub_chat_id, sync.epoch, bf.seq, &now, &req)
                        .await
                    {
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
        // #3 增量窗口续命：补推投递成功同样刷新活动 turn 计时（断链补推
        // 期间的长流式 turn 不因窗口到期误判 delivery_unknown）。
        if delivered > 0 {
            self.inner.chats.touch_active_turn(&hub_chat_id).await;
        }
        debug!(
            chat_id = hub_chat_id,
            epoch = sync.epoch,
            from_seq = sync.from_seq,
            delivered,
            rejected,
            "buffer_sync consumed"
        );
        // 补推追平（§7.3/§8.5）：缓冲完整（无乱序/重复丢弃）且至少一帧
        // 投递 → 清除断链置的 gap 标记并恢复 chat 可用。`rejected > 0`
        // 保留 gap——缓冲孔洞的真实缺口由聚合器在后续实时帧 seq 跳号时
        // 精确计数上报（设计稿「缺口数量由补推时聚合器精确计算」）。
        if rejected == 0 && delivered > 0 {
            self.recover_from_gap(&hub_chat_id).await;
        }
        if delivered == 0 && rejected > 0 {
            ConsumeResult::BatchRejected {
                reason: "all_frames_rejected",
            }
        } else {
            ConsumeResult::Delivered {
                chat_id: hub_chat_id,
                kind: "buffer_sync",
                seq: sync.from_seq,
                applied: rejected == 0,
            }
        }
    }

    /// 断链清理（§8.2 matrix instance 行 + §7.1 离线即刻生效）：
    /// 该 instance 全部活 chat：活动 turn → `MarkTurnInterrupted`（DocCommand）、
    /// chat 置 gap 标记（registry；缺口数量由补推时聚合器精确计算）、
    /// 状态迁移 Gap。
    pub async fn on_instance_disconnect(&self, instance_id: &str) -> Result<(), RelayError> {
        let chats = self.inner.chats.chats_for_instance(instance_id).await;
        let mut interrupted = 0usize;
        let mut gapped = 0usize;
        for (chat_id, state) in &chats {
            if state.is_terminal() {
                continue;
            }
            // 活动 turn → interrupted（§7.1；turn_id 由 coordinator 登记）。
            if let Some(turn_id) = self.inner.chats.active_turn(chat_id).await {
                let result = self
                    .inner
                    .doc
                    .submit_command(
                        chat_id,
                        DocCommand::MarkTurnInterrupted {
                            turn_id: turn_id.clone(),
                        },
                    )
                    .await;
                if matches!(result, SubmitResult::Applied(_)) {
                    interrupted += 1;
                } else if matches!(result, SubmitResult::Rejected(SubmitError::ChatNotFound)) {
                    // chat writer 未打开：仅记录（视图缺失由 gap 呈现）。
                    warn!(chat_id, "turn interrupt skipped: chat writer absent");
                }
                // 表项清理（§7.2）：turn 已置 interrupted 终态，登记表不得
                // 滞留——否则永久阻塞后续 load「有活动 turn」校验。
                self.inner.chats.clear_active_turn(chat_id).await;
            }
            // 断链时该 chat 全部 pending 权限批量 expired（对齐参考实现
            // expireTurnPermissions：断链即会话失效，未决议权限全部过期）。
            self.inner
                .doc
                .submit_command(chat_id, DocCommand::ExpirePendingPermissions)
                .await;
            // #1 官方 pending_permissions 表同源清理（§7.1 语义对齐：
            // 断链即会话失效，回投数据不再有效）。
            self.inner
                .pending_permissions
                .write()
                .await
                .retain(|_, v| v.chat_id != *chat_id);
            // gap 标记（§8.2/§7.3：补推完成、seq 追平后清除）。
            let _ = self.inner.registry.set_chat_gap(chat_id, Some(0)).await;
            let _ = self
                .inner
                .registry
                .report_condition(DegradeCause::ChatGap)
                .await;
            if self
                .inner
                .chats
                .transition(chat_id, ChatState::Gap)
                .await
                .is_ok()
            {
                gapped += 1;
            }
        }
        info!(
            instance_id,
            chats = chats.len(),
            interrupted,
            gapped,
            "instance disconnect cleanup complete"
        );
        Ok(())
    }

    /// `instance/process_exit` 消费（§4.5）：终态写视图（F4 `SetChatTerminal`）、
    /// chat 状态迁移（ended/crashed，§7.3）；不再接受新事件（聚合器终态
    /// 守卫，§8.2）。
    pub async fn on_process_exit(
        &self,
        instance_id: &str,
        exit: &InstanceProcessExit,
    ) -> ConsumeResult {
        let _ = instance_id;
        // 信封 chat_id = instance 进程归属（hub chat id，spawn 时确立，
        // §4.5.1），无 binding 翻译（进程生命周期事件不携带 ACP 帧）。
        let hub_chat_id = exit.chat_id.clone();
        if self.inner.chats.entry(&hub_chat_id).await.is_none() {
            self.count_dropped("binding_missing");
            return ConsumeResult::Dropped {
                reason: "binding_missing",
            };
        }
        let status = if exit.code == 0 {
            acp_hub_proto::schema::ChatStatus::Ended
        } else {
            acp_hub_proto::schema::ChatStatus::Crashed
        };
        let state = if exit.code == 0 {
            ChatState::Ended
        } else {
            ChatState::Crashed
        };
        let _ = self
            .inner
            .doc
            .submit_command(&hub_chat_id, DocCommand::SetChatTerminal { status })
            .await;
        let _ = self.inner.chats.transition(&hub_chat_id, state).await;
        self.inner.chats.clear_active_turn(&hub_chat_id).await;
        // #1 官方 pending_permissions 表清理（进程退出即会话失效）。
        self.inner
            .pending_permissions
            .write()
            .await
            .retain(|_, v| v.chat_id != hub_chat_id);
        ConsumeResult::Delivered {
            chat_id: hub_chat_id,
            kind: "process_exit",
            seq: 0,
            applied: true,
        }
    }

    /// 登记官方 request_permission（#1）：记 pending_permissions 表 +
    /// 投递 `PermissionRequested` 事件。
    ///
    /// turn_id 从 active_turns 表注入（官方帧无 turnId；聚合器
    /// aggregator.rs:1042 要求 turn_id == control doc active_turn_id 才推进
    /// awaitingPermission——必须注入）；无活动 turn → 空串（投影仍写，仅
    /// 状态不推进，功能不丢）。
    async fn register_permission_request(
        &self,
        chat_id: &str,
        epoch: u64,
        seq: u64,
        now: &str,
        req: &PermissionRequestFields,
    ) -> ConsumeResult {
        self.inner.pending_permissions.write().await.insert(
            req.permission_id.clone(),
            PendingPermissionReq {
                request_id: req.request_id.clone(),
                options: req.options.clone(),
                chat_id: chat_id.to_string(),
                resolving_command_id: None,
                resolving_decision: None,
            },
        );
        let turn_id = self
            .inner
            .chats
            .active_turn(chat_id)
            .await
            .unwrap_or_default();
        let nev = NormalizedEvent {
            chat_id: chat_id.to_string(),
            seq,
            epoch,
            ts: now.to_string(),
            body: crate::state::normalized::EventBody::PermissionRequested {
                permission_id: req.permission_id.clone(),
                turn_id,
                tool_call_id: req.tool_call_id.clone(),
                tool: Some(req.tool.clone()),
                title: req.title.clone(),
                description: req.description.clone(),
                options: permission_option_kinds(&req.options),
                expires_at: expires_at(now),
            },
        };
        self.submit(chat_id, nev).await
    }

    /// 读取官方 request 回投材料。发送成功前不得移除：明确未送达时，同一
    /// decision + commandId 需要复用它恢复；相反 decision 不得消费它。
    pub async fn pending_permission(&self, permission_id: &str) -> Option<PendingPermissionReq> {
        self.inner
            .pending_permissions
            .read()
            .await
            .get(permission_id)
            .cloned()
    }

    /// 申领官方 permission response 的唯一投递权。
    ///
    /// 首次决策会将 `(commandId, decision)` 绑定到 request；后续只有
    /// 完全相同的命令才能在明确未送达后恢复。其他 commandId 或相反
    /// decision 均不得获取响应材料。
    pub async fn claim_pending_permission(
        &self,
        permission_id: &str,
        command_id: &str,
        decision: PermissionDecision,
    ) -> Option<PendingPermissionReq> {
        let mut pending = self.inner.pending_permissions.write().await;
        let request = pending.get_mut(permission_id)?;
        match (&request.resolving_command_id, request.resolving_decision) {
            (None, None) => {
                request.resolving_command_id = Some(command_id.to_string());
                request.resolving_decision = Some(decision);
            }
            (Some(existing_id), Some(existing_decision))
                if existing_id == command_id && existing_decision == decision => {}
            _ => return None,
        }
        Some(request.clone())
    }

    /// 投递确认后移除表项；幂等无害。未确认前必须保留，
    /// 以便原 `(commandId, decision)` 在明确未送达时恢复。
    pub async fn remove_pending_permission(&self, permission_id: &str) {
        self.inner
            .pending_permissions
            .write()
            .await
            .remove(permission_id);
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
    async fn submit(&self, chat_id: &str, nev: NormalizedEvent) -> ConsumeResult {
        let kind = nev.kind();
        let seq = nev.seq;
        match self.inner.doc.submit_event(nev).await {
            SubmitResult::Applied(r) => {
                // #3 增量窗口续命（issue #3）：事件投递成功（聚合器接受）
                // → 刷新该 chat 的活动 turn 计时——exec_prompt L3 以「窗口
                // （l3_timeout）内无增量投递」判定 delivery_unknown，长流式
                // turn 的事件回流不得被 30s 硬超时误杀。
                if r.applied {
                    self.inner.chats.touch_active_turn(chat_id).await;
                }
                ConsumeResult::Delivered {
                    chat_id: chat_id.to_string(),
                    kind,
                    seq,
                    applied: r.applied,
                }
            }
            SubmitResult::Rejected(_) => ConsumeResult::Dropped {
                reason: "submit_rejected",
            },
            SubmitResult::PersistFailed => ConsumeResult::PersistFailed {
                chat_id: chat_id.to_string(),
            },
        }
    }

    /// 断链追平恢复（§7.3/§8.5）：补推/实时帧恢复投递成功后调用——清除
    /// 断链时置的 gap 标记（`set_chat_gap(Some(0))` 占位 → 追平清除）并
    /// 迁移 ChatState Gap → Accepting（§7.3「Gap 清除 → 恢复可用、可开
    /// 新 turn」）。
    ///
    /// 幂等：chat 非 Gap 状态直接返回（恢复后首帧即完成，后续帧跳过）；
    /// 判定在 writer 内（[`DocCommand::ResumeAfterGap`]）：**不可校准**
    /// （epoch 变化，§4.5.1）拒绝恢复——不可校准缺口只能经 `session/load`
    /// 显式重建消除，不得误标为已追平（视图假装完整）。
    async fn recover_from_gap(&self, chat_id: &str) {
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            return;
        };
        if entry.state != ChatState::Gap {
            return;
        }
        match self
            .inner
            .doc
            .submit_command(chat_id, DocCommand::ResumeAfterGap)
            .await
        {
            SubmitResult::Applied(_)
                if self
                    .inner
                    .chats
                    .transition(chat_id, ChatState::Accepting)
                    .await
                    .is_ok() =>
            {
                info!(chat_id, "chat recovered from gap (stream caught up)");
            }
            _ => {
                // 拒绝（uncalibratable / chat 已关闭）或迁移失败：保持 gap。
            }
        }
    }
}

/// 官方 options kind → 内部投影枚举（#1，3 值投影层；§5.4）。
/// `allow_once→AllowOnce`、`allow_always→AllowSession`、
/// `reject_once|reject_always→Deny`；兼容 camelCase 别名
/// （`allowOnce`/`allowSession`，对齐 acp_channel.rs `permission_options`
/// 的兼容先例；reject 类官方无 camel 形态，防御性同兼容）。
/// 未识别 kind → 跳过（与 permission_options 同语义，§5.4 投影层宽容）。
fn permission_option_kinds(options: &[serde_json::Value]) -> Vec<PermissionOptions> {
    options
        .iter()
        .filter_map(|v| {
            Some(match v.get("kind")?.as_str()? {
                "allow_once" | "allowOnce" => PermissionOptions::AllowOnce,
                "allow_always" | "allowSession" => PermissionOptions::AllowSession,
                "reject_once" | "rejectOnce" | "reject_always" | "rejectAlways" => {
                    PermissionOptions::Deny
                }
                _ => return None,
            })
        })
        .collect()
}

/// 权限请求过期时刻（#1：#1 复用 acp_channel.rs `PERMISSION_TIMEOUT`（5min）
/// 常量逻辑，与 map_raw permission_request 同源注入；server 权威时钟
/// §4.7）。now 非 RFC3339 → 原样回退。
fn expires_at(now: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(now) {
        Ok(t) => (t + chrono::Duration::from_std(PERMISSION_TIMEOUT)
            .unwrap_or(chrono::Duration::seconds(300)))
        .to_rfc3339(),
        Err(_) => now.to_string(),
    }
}

#[cfg(test)]
#[path = "relay_event_handler_test.rs"]
mod relay_event_handler_test;
