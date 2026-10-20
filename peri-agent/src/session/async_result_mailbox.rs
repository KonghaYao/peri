//! 会话级后台结果处理责任（AsyncResultMailbox）。
//!
//! 执行状态（shell 是否退出、任务是否终止）由 TaskManager / registry 拥有；本模块
//! 只记录**处理责任**：一条正式终态通知是否已经被某次主模型响应有效覆盖。两者
//! 不共享状态，也不允许用摘要、界面隐藏或队列 ACK 代替主模型确认。
//!
//! 状态机：
//!
//! ```text
//! (none) --accept--> Queued --canonical commit--> Ready
//! Ready --request snapshot--> InFlight --valid commit + barrier--> Covered
//! Ready/InFlight --stage failure--> Blocked --explicit continue--> Ready
//! any non-covered --Stop--> Suspended --explicit continue--> Ready/Queued
//! ```
//!
//! 约束：
//! - 同一 `delivery_id` + 同一内容幂等；同 ID 不同内容报冲突。
//! - `Queued` 只表示责任已建立、通知可能仍在队列；不进入 Compact 保护集合。
//! - `Ready`/`InFlight`/`Blocked`/`Suspended` 进入保护集合，Full Compact 不得归档。
//! - `Covered` 只保留 ID + 指纹，释放通知正文与请求快照。
//! - Stop 不得被自动唤醒绕过；`Blocked` 不能自我激活。
//!
//! 本模块不持有 MessageQueue，也不扫描历史提醒：投递方先调用 [`accept`] 建立
//! 责任，再把消息 push 到队列；Receive 在规范提醒提交成功后调用
//! [`mark_canonical_ready`]。锁不跨 await，消费者不会反向嵌套队列锁。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::session::MessageSource;
use peri_acp_types::system_reminder::TrustedSystemReminder;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::messages::MessageId;

/// 运行时会话身份 + 发起执行线程身份。
///
/// 根会话跨 prompt 复用同一 mailbox；子 agent 使用自己的 mailbox，不共享确认。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResultScope {
    pub session_id: String,
    pub initiator: String,
}

impl ResultScope {
    pub fn new(session_id: impl Into<String>, initiator: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            initiator: initiator.into(),
        }
    }
}

/// 单条结果的显式状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncResultState {
    /// 责任已建立，通知仍在队列或尚未完成规范提交。
    Queued,
    /// 规范提醒已提交；可进入 Compact 保护集合，可触发续跑。
    Ready,
    /// 已被某次主模型请求包含；等待有效响应提交。
    InFlight,
    /// 阶段失败或提交失败；保留责任，但不自动激活。
    Blocked,
    /// 用户 Stop 或会话暂停；不得自动唤醒，也不伪装成 Covered。
    Suspended,
    /// 包含该结果的有效主模型响应已提交；正文引用已释放。
    Covered,
}

impl AsyncResultState {
    /// 是否进入 Full Compact / 模型投影保护集合。
    pub fn is_protected(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::InFlight | Self::Blocked | Self::Suspended
        )
    }
}

/// 一次处理失败的有界诊断；不修改 canonical reminder 内容或指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncResultDiagnostic {
    pub stage: String,
    pub summary: String,
    /// Blocked 永远不允许自我激活；保留字段用于日志与后续策略评估。
    pub auto_recovery_allowed: bool,
}

const DIAGNOSTIC_MAX_CHARS: usize = 240;

fn bounded(text: &str) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= DIAGNOSTIC_MAX_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// 供模型投影使用的受保护结果（按接收顺序，每个 ID 一次）。
#[derive(Debug, Clone)]
pub struct ProtectedResult {
    pub delivery_id: MessageId,
    pub public_task_id: String,
    pub remote_task_id: Option<String>,
    pub state: AsyncResultState,
    pub reminder: Arc<TrustedSystemReminder>,
    pub diagnostic: Option<AsyncResultDiagnostic>,
}

/// 一次模型视图渲染使用的保护集合快照。
///
/// `version` 覆盖状态迁移与诊断变化，供模型视图缓存识别“canonical 修订未变，
/// 但保护集合已变”的情况。
#[derive(Debug, Clone, Default)]
pub struct ProtectedResults {
    pub version: u64,
    pub items: Vec<ProtectedResult>,
}

impl ProtectedResults {
    pub fn ids(&self) -> Vec<MessageId> {
        self.items.iter().map(|item| item.delivery_id).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 投递方的接纳结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// 新责任已建立，调用方必须随后把通知 push 到队列。
    Accepted,
    /// 同一 ID + 同一内容已接纳（可能已 Ready/Covered）；不要重复入队。
    Duplicate,
}

/// 一次主模型请求的不可变覆盖范围。
#[derive(Debug, Clone)]
pub struct RequestReceipt {
    attempt_id: String,
    request_id: String,
    included_ids: Vec<MessageId>,
}

impl RequestReceipt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn included_ids(&self) -> &[MessageId] {
        &self.included_ids
    }
}

struct ResultRecord {
    fingerprint: String,
    reminder: Arc<TrustedSystemReminder>,
    public_task_id: String,
    remote_task_id: Option<String>,
    admission_seq: u64,
    state: AsyncResultState,
    /// 规范提醒是否已经成功写入（或确认已存在）。挂起恢复时据此区分
    /// “仍由队列负责”和“可进入下一次请求”。
    canonical_committed: bool,
    attempt_id: Option<String>,
    request_id: Option<String>,
    diagnostic: Option<AsyncResultDiagnostic>,
}

struct MailboxState {
    records: HashMap<MessageId, ResultRecord>,
    /// 接收顺序；保护投影必须按此顺序输出。
    order: Vec<MessageId>,
    /// Covered 只保留紧凑回执。
    covered: HashMap<MessageId, String>,
    next_admission: u64,
    version: u64,
    /// 用户 Stop 后的会话级暂停；挂起期间新结果即使规范提交也只进入
    /// Suspended，不能成为自动续跑候选。
    suspended: bool,
    /// 单元测试用的确定性状态转移记录；生产日志仍走 tracing。
    #[cfg(test)]
    trace: Vec<String>,
}

impl MailboxState {
    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    #[cfg(test)]
    fn state_of(&self, id: &MessageId) -> Option<AsyncResultState> {
        self.records.get(id).map(|record| record.state)
    }
}

/// 会话级后台结果处理责任 owner。
pub struct AsyncResultMailbox {
    scope: ResultScope,
    state: Mutex<MailboxState>,
    /// 保留式版本信号：订阅方先检查 [`Self::has_ready`]，再等待变化；
    /// watch 确保错过通知后仍能观察当前版本。
    state_version: watch::Sender<u64>,
}

impl Default for AsyncResultMailbox {
    fn default() -> Self {
        Self::new("", "")
    }
}

impl AsyncResultMailbox {
    pub fn new(session_id: impl Into<String>, initiator: impl Into<String>) -> Self {
        Self {
            scope: ResultScope::new(session_id, initiator),
            state: Mutex::new(MailboxState {
                records: HashMap::new(),
                order: Vec::new(),
                covered: HashMap::new(),
                // 与 MessageQueue 的 admission sequence 对齐：首个结果得到 1，
                // 空 mailbox 的水位为 0，`has_ready_after(0)` 才能覆盖首个结果。
                next_admission: 1,
                version: 0,
                suspended: false,
                #[cfg(test)]
                trace: Vec::new(),
            }),
            state_version: watch::channel(0).0,
        }
    }

    pub fn scope(&self) -> &ResultScope {
        &self.scope
    }

    /// 在通知进入队列前建立 Queued 责任。
    ///
    /// 幂等：同 ID 同指纹返回 [`AcceptOutcome::Duplicate`]；同 ID 不同内容返回
    /// 冲突错误，且不改变已有记录。
    pub fn accept(
        &self,
        delivery_id: MessageId,
        reminder: &TrustedSystemReminder,
        source: MessageSource,
    ) -> Result<AcceptOutcome, String> {
        let fingerprint = reminder_fingerprint(reminder, &source);
        let mut state = self.state.lock();
        if let Some(prior) = state.covered.get(&delivery_id) {
            return if prior == &fingerprint {
                Ok(AcceptOutcome::Duplicate)
            } else {
                Err("conflicting terminal result identity".into())
            };
        }
        if let Some(prior) = state.records.get(&delivery_id) {
            return if prior.fingerprint == fingerprint {
                Ok(AcceptOutcome::Duplicate)
            } else {
                Err("conflicting terminal result identity".into())
            };
        }
        let admission_seq = state.next_admission;
        state.next_admission = state.next_admission.wrapping_add(1);
        let (public_task_id, remote_task_id) = task_identity(reminder);
        state.records.insert(
            delivery_id,
            ResultRecord {
                fingerprint,
                reminder: Arc::new(reminder.clone()),
                public_task_id: public_task_id.clone(),
                remote_task_id,
                admission_seq,
                state: AsyncResultState::Queued,
                canonical_committed: false,
                attempt_id: None,
                request_id: None,
                diagnostic: None,
            },
        );
        state.order.push(delivery_id);
        state.bump();
        #[cfg(test)]
        state.trace.push(format!(
            "result_accepted {} {public_task_id}",
            delivery_id.as_uuid()
        ));
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::info!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            delivery_id = %delivery_id.as_uuid(),
            public_task_id = %public_task_id,
            admission_seq,
            "result_accepted"
        );
        Ok(AcceptOutcome::Accepted)
    }

    /// Receive 成功写入或确认规范提醒后调用。
    ///
    /// 已 Cover 的重复消息不重新激活；Suspended 保持暂停，等待显式继续。
    /// 记录缺失时按当前通知补建 Ready（跨 prompt 重建、无宿主路径），仍保留责任。
    ///
    /// `_admission_seq` 是队列侧序号，仅用于调用方便；mailbox 的自动激活水位使用
    /// 自己的接纳计数（宿主记录 `admission_watermark()`）。两套计数混用会让旧结果
    /// 凭更大的队列序号越过失败水位。
    pub fn mark_canonical_ready(
        &self,
        delivery_id: MessageId,
        reminder: &TrustedSystemReminder,
        source: MessageSource,
        _admission_seq: Option<u64>,
    ) -> Result<(), String> {
        let fingerprint = reminder_fingerprint(reminder, &source);
        let mut state = self.state.lock();
        if let Some(prior) = state.covered.get(&delivery_id) {
            return if prior == &fingerprint {
                Ok(())
            } else {
                Err("conflicting canonical terminal delivery identity".into())
            };
        }
        let scope_suspended = state.suspended;
        if let Some(record) = state.records.get_mut(&delivery_id) {
            if record.fingerprint != fingerprint {
                return Err("conflicting canonical terminal delivery identity".into());
            }
            match record.state {
                AsyncResultState::Queued => {
                    // Stop 后的新结果也不能因为规范提交就变成自动续跑候选。
                    record.state = if scope_suspended {
                        AsyncResultState::Suspended
                    } else {
                        AsyncResultState::Ready
                    };
                    record.canonical_committed = true;
                    let public_task_id = record.public_task_id.clone();
                    state.bump();
                    #[cfg(test)]
                    state.trace.push(format!(
                        "result_canonical_ready {} {public_task_id}",
                        delivery_id.as_uuid()
                    ));
                    let version = state.version;
                    drop(state);
                    self.state_version.send_replace(version);
                    tracing::info!(
                        session_id = %self.scope.session_id,
                        initiator = %self.scope.initiator,
                        delivery_id = %delivery_id.as_uuid(),
                        public_task_id = %public_task_id,
                        suspended = scope_suspended,
                        "result_canonical_ready"
                    );
                }
                // 重复接收、Blocked/Suspended/InFlight 的规范提醒重放都不改变
                // 处理责任；只有下一次显式继续或新请求才能推进。
                _ => {
                    // 规范提交本身是既成事实：即使是 Stop 之后才被 Receive
                    // 消费的提醒，也要记住它已进入 transcript，显式继续时才能
                    // 从 Queued 正确恢复为 Ready。
                    record.canonical_committed = true;
                }
            }
            return Ok(());
        }
        // mailbox 使用自己的单调接纳序号：宿主以 `admission_watermark()` 记录失败
        // 边界，与队列的 admission sequence 是两套独立计数，混用会让旧结果
        // 凭借更大的队列序号越过失败水位、自我激活。
        let admission_seq = {
            let seq = state.next_admission;
            state.next_admission = state.next_admission.wrapping_add(1);
            seq
        };
        let (public_task_id, remote_task_id) = task_identity(reminder);
        let rebuilt_state = if state.suspended {
            AsyncResultState::Suspended
        } else {
            AsyncResultState::Ready
        };
        state.records.insert(
            delivery_id,
            ResultRecord {
                fingerprint,
                reminder: Arc::new(reminder.clone()),
                public_task_id,
                remote_task_id,
                admission_seq,
                state: rebuilt_state,
                canonical_committed: true,
                attempt_id: None,
                request_id: None,
                diagnostic: None,
            },
        );
        state.order.push(delivery_id);
        state.bump();
        #[cfg(test)]
        state.trace.push(format!(
            "result_canonical_ready {} rebuilt",
            delivery_id.as_uuid()
        ));
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::info!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            delivery_id = %delivery_id.as_uuid(),
            "result_canonical_ready (rebuilt)"
        );
        Ok(())
    }

    /// 当前保护集合（按接收顺序）。
    pub fn protected_snapshot(&self) -> ProtectedResults {
        let state = self.state.lock();
        let items = state
            .order
            .iter()
            .filter_map(|id| {
                let record = state.records.get(id)?;
                record.state.is_protected().then(|| ProtectedResult {
                    delivery_id: *id,
                    public_task_id: record.public_task_id.clone(),
                    remote_task_id: record.remote_task_id.clone(),
                    state: record.state,
                    reminder: Arc::clone(&record.reminder),
                    diagnostic: record.diagnostic.clone(),
                })
            })
            .collect();
        ProtectedResults {
            version: state.version,
            items,
        }
    }

    /// 是否存在可运行的 Ready 结果（Blocked/Suspended 不算）。
    pub fn has_ready(&self) -> bool {
        self.state
            .lock()
            .records
            .values()
            .any(|record| record.state == AsyncResultState::Ready)
    }

    /// 最后一次接纳分配的代数（与队列 admission watermark 同语义）。
    ///
    /// 宿主在每次执行尝试开始时记录该值，失败后只用**更新**的 Ready 结果作为
    /// 自动恢复触发来源；旧失败记录不能自我激活。
    pub fn admission_watermark(&self) -> u64 {
        let state = self.state.lock();
        state.next_admission.saturating_sub(1)
    }

    /// 是否存在在 `watermark` 之后接纳、且当前可运行的 Ready 结果。
    pub fn has_ready_after(&self, watermark: u64) -> bool {
        self.state.lock().records.values().any(|record| {
            record.state == AsyncResultState::Ready && record.admission_seq > watermark
        })
    }

    /// 保留式版本信号；订阅后先检查 [`Self::has_ready`]。
    pub fn subscribe_state(&self) -> watch::Receiver<u64> {
        self.state_version.subscribe()
    }

    /// 已记录结果数量（诊断用）。
    pub fn pending_count(&self) -> usize {
        self.state.lock().records.len()
    }

    /// 是否没有任何 Covered 回执（诊断用）。
    #[cfg(test)]
    pub(crate) fn covered_is_empty(&self) -> bool {
        self.state.lock().covered.is_empty()
    }

    /// 按最终主模型请求的**实际投影**记录 receipt。
    ///
    /// `included_ids` 必须来自渲染后的消息快照；本方法不接受“全部 pending”。
    /// 返回 `None` 表示本次请求没有包含任何受保护结果。
    pub fn capture_request(
        &self,
        attempt_id: impl Into<String>,
        request_id: impl Into<String>,
        included_ids: &[MessageId],
    ) -> Option<RequestReceipt> {
        let attempt_id = attempt_id.into();
        let request_id = request_id.into();
        let mut state = self.state.lock();
        let mut captured = Vec::new();
        for id in included_ids {
            let Some(record) = state.records.get_mut(id) else {
                continue;
            };
            if !record.state.is_protected() {
                continue;
            }
            record.state = AsyncResultState::InFlight;
            record.attempt_id = Some(attempt_id.clone());
            record.request_id = Some(request_id.clone());
            captured.push(*id);
        }
        if captured.is_empty() {
            return None;
        }
        state.bump();
        #[cfg(test)]
        state.trace.push(format!(
            "primary_result_receipt {request_id} {}",
            captured
                .iter()
                .map(|id| id.as_uuid().to_string())
                .collect::<Vec<_>>()
                .join(",")
        ));
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::info!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            %request_id,
            attempt_id = %attempt_id,
            delivery_ids = ?captured,
            "primary_result_receipt"
        );
        Some(RequestReceipt {
            attempt_id,
            request_id,
            included_ids: captured,
        })
    }

    /// 仅在有效响应及对应 transcript 提交 barrier 成功后调用。
    pub fn cover_after_commit(
        &self,
        receipt: &RequestReceipt,
        committed_response_id: Option<MessageId>,
    ) -> Result<usize, String> {
        let mut state = self.state.lock();
        // 先验证整张 receipt，避免部分确认后才发现 attempt 已失效。
        for id in &receipt.included_ids {
            let Some(record) = state.records.get(id) else {
                // 已 Cover 或重复确认：幂等。
                continue;
            };
            if record.attempt_id.as_deref() != Some(receipt.attempt_id.as_str()) {
                return Err(format!(
                    "result {id:?} is no longer bound to attempt {}",
                    receipt.attempt_id
                ));
            }
            if record.state != AsyncResultState::InFlight {
                return Err(format!(
                    "result {id:?} left InFlight before response commit"
                ));
            }
        }
        let mut covered = 0usize;
        for id in &receipt.included_ids {
            let Some(record) = state.records.remove(id) else {
                continue;
            };
            state.order.retain(|existing| existing != id);
            state.covered.insert(*id, record.fingerprint);
            covered += 1;
        }
        if covered > 0 {
            state.bump();
            #[cfg(test)]
            state.trace.push(format!(
                "result_covered {} {}",
                receipt.request_id,
                receipt
                    .included_ids
                    .iter()
                    .map(|id| id.as_uuid().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            let version = state.version;
            drop(state);
            self.state_version.send_replace(version);
            tracing::info!(
                session_id = %self.scope.session_id,
                initiator = %self.scope.initiator,
                request_id = %receipt.request_id,
                attempt_id = %receipt.attempt_id,
                committed_response_id = ?committed_response_id,
                covered,
                "result_covered"
            );
        }
        Ok(covered)
    }

    /// 标记本次 attempt 包含的结果处理失败。
    pub fn block_attempt(&self, receipt: &RequestReceipt, stage: &str, summary: &str) {
        self.block_matching(stage, summary, Some(receipt.attempt_id.as_str()));
    }

    /// 阶段失败：阻塞所有尚未覆盖的 Ready/InFlight 结果。
    ///
    /// 已 Blocked/Suspended 的记录保持原诊断；Queued 仍由队列负责。
    pub fn block_uncovered(&self, stage: &str, summary: &str) {
        self.block_matching(stage, summary, None);
    }

    fn block_matching(&self, stage: &str, summary: &str, attempt_id: Option<&str>) {
        let mut state = self.state.lock();
        let mut blocked = Vec::new();
        for (id, record) in state.records.iter_mut() {
            if let Some(attempt) = attempt_id {
                if record.attempt_id.as_deref() != Some(attempt) {
                    continue;
                }
            }
            if !matches!(
                record.state,
                AsyncResultState::Ready | AsyncResultState::InFlight
            ) {
                continue;
            }
            record.state = AsyncResultState::Blocked;
            record.diagnostic = Some(AsyncResultDiagnostic {
                stage: stage.to_string(),
                summary: bounded(summary),
                auto_recovery_allowed: false,
            });
            record.attempt_id = None;
            record.request_id = None;
            blocked.push(*id);
        }
        if blocked.is_empty() {
            return;
        }
        state.bump();
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::warn!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            stage,
            delivery_ids = ?blocked,
            "result_blocked"
        );
    }

    /// 用户 Stop：所有未覆盖结果暂停，不自动唤醒，也不伪造 Covered。
    ///
    /// 暂停是 scope 级事实：Stop 之后才规范提交的新结果同样进入 Suspended，
    /// 不能仅凭新完成通知绕过用户停止。
    pub fn suspend_scope(&self, reason: &str) {
        let mut state = self.state.lock();
        let changed = !state.suspended;
        state.suspended = true;
        let mut suspended = Vec::new();
        for (id, record) in state.records.iter_mut() {
            if record.state == AsyncResultState::Covered {
                continue;
            }
            if record.state != AsyncResultState::Suspended {
                record.state = AsyncResultState::Suspended;
                record.attempt_id = None;
                record.request_id = None;
                suspended.push(*id);
            }
        }
        if !changed && suspended.is_empty() {
            return;
        }
        state.bump();
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::info!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            reason,
            delivery_ids = ?suspended,
            "result_suspended"
        );
    }

    /// 用户明确继续：Suspended 恢复为可处理状态；Queued 仍等待规范提交。
    ///
    /// Blocked 恢复为 Ready 后，下一次有效请求会带上失败诊断；不会自动重放旧工具。
    pub fn resume_scope(&self) {
        let mut state = self.state.lock();
        let changed = state.suspended;
        state.suspended = false;
        let mut resumed = Vec::new();
        for (id, record) in state.records.iter_mut() {
            let next = match record.state {
                AsyncResultState::Suspended => {
                    if record.canonical_committed {
                        AsyncResultState::Ready
                    } else {
                        // 从未规范提交的挂起结果仍由队列负责，不能直接标 Ready。
                        AsyncResultState::Queued
                    }
                }
                AsyncResultState::Blocked => AsyncResultState::Ready,
                _ => continue,
            };
            record.state = next;
            record.attempt_id = None;
            record.request_id = None;
            resumed.push(*id);
        }
        if !changed && resumed.is_empty() {
            return;
        }
        state.bump();
        let version = state.version;
        drop(state);
        self.state_version.send_replace(version);
        tracing::info!(
            session_id = %self.scope.session_id,
            initiator = %self.scope.initiator,
            delivery_ids = ?resumed,
            "result_resumed"
        );
    }

    /// 诊断/测试观察点：单条结果当前状态。
    #[cfg(test)]
    pub(crate) fn state_of(&self, id: &MessageId) -> Option<AsyncResultState> {
        self.state.lock().state_of(id)
    }

    /// 诊断/测试观察点：确定性状态转移记录（不依赖 tracing callsite 兴趣缓存）。
    #[cfg(test)]
    pub(crate) fn trace_events(&self) -> Vec<String> {
        self.state.lock().trace.clone()
    }

    /// 诊断/测试观察点：该 ID 是否保留了正文引用。
    #[cfg(test)]
    pub(crate) fn retains_body(&self, id: &MessageId) -> bool {
        self.state.lock().records.contains_key(id)
    }
}

fn reminder_fingerprint(reminder: &TrustedSystemReminder, source: &MessageSource) -> String {
    let encoded = serde_json::to_vec(&(reminder.as_reminder(), source))
        .unwrap_or_else(|_| reminder.as_reminder().body.as_bytes().to_vec());
    let digest = Sha256::digest(encoded);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn task_identity(reminder: &TrustedSystemReminder) -> (String, Option<String>) {
    let metadata = &reminder.as_reminder().metadata;
    let public = metadata
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let remote = metadata
        .get("remote_task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    (public, remote)
}

#[cfg(test)]
#[path = "async_result_mailbox_test.rs"]
mod tests;
