//! Thread MQ（持久收件箱）的跨层契约：入队输入、存储事实与操作结果。
//!
//! 这些类型是 `SessionResources` 的 MQ 行为面与业务侧之间唯一的形状：业务方拿不到
//! SQL、表行或事务，存储 adapter 也不解释业务语义。字段与
//! `docs/design/thread-mq.md` §3 的列一一对应——`enqueued_at` 只出现在
//! [`MqEntry`]/[`MqWatermark`]（存储生成），[`MqInput`] 里没有这个位置。
//!
//! 术语：
//!
//! - **移交（hand off）**：MQ 行在同一事务里写入 `messages` 并被删除。移交后 MQ 不再
//!   持有该消息，表里也不留终态或墓碑。
//! - **水位（watermark）**：`(enqueued_at, id)` 二元组。它是唯一的消费顺序键，不引入
//!   `seq` 或放行序号。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::messages::MessageId;
use crate::session::{ExecutionBinding, MessageKind, MessagePolicy, MessageSource};
use crate::session_resources::{SessionResourceResult, SessionResources};
use crate::store::PersistedPayload;
use crate::thread::ThreadId;

/// 一次入队请求：调用方给内容，存储给入队时间与目标。
///
/// 不接受 `thread_id` 或 `enqueued_at`：目标由已绑定 thread 的调用入口决定，入队时间
/// 由存储端生成；重试复用同一个 [`MessageId`]，不得改写既有行的时间。
#[derive(Debug, Clone)]
pub struct MqInput {
    pub id: MessageId,
    pub kind: MessageKind,
    pub source: MessageSource,
    pub payload: PersistedPayload,
    pub policy: MessagePolicy,
    /// 用户输入的原始编辑器文本；其他来源为 `None`。
    pub original_draft: Option<String>,
}

/// `thread_mq` 中一行的事实，不含任何运行期状态。
#[derive(Debug, Clone)]
pub struct MqEntry {
    pub id: MessageId,
    /// 存储端生成的 UTC 时间，固定 `YYYY-MM-DDTHH:MM:SS.ffffffZ`。
    pub enqueued_at: String,
    pub kind: MessageKind,
    pub source: MessageSource,
    pub payload: PersistedPayload,
    pub policy: MessagePolicy,
    pub original_draft: Option<String>,
}

impl MqEntry {
    /// 本行的消费顺序键。
    pub fn watermark(&self) -> MqWatermark {
        MqWatermark {
            enqueued_at: self.enqueued_at.clone(),
            id: self.id,
        }
    }
}

/// 消费顺序键：先 `enqueued_at`，时间相同再按消息 ID 稳定排序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MqWatermark {
    pub enqueued_at: String,
    pub id: MessageId,
}

impl MqWatermark {
    pub fn new(enqueued_at: impl Into<String>, id: MessageId) -> Self {
        Self {
            enqueued_at: enqueued_at.into(),
            id,
        }
    }
}

impl PartialOrd for MqWatermark {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MqWatermark {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.enqueued_at.cmp(&other.enqueued_at).then_with(|| {
            // 消息 ID 是 UUID v7：同一时间戳内的字典序即创建顺序。
            self.id.as_uuid().cmp(&other.id.as_uuid())
        })
    }
}

/// 一次限定查询：只表达投影用途，不暴露任意 SQL。
#[derive(Debug, Clone, Default)]
pub struct MqFilter {
    /// 只要用户待发输入（待发送区投影）。
    pub user_input_only: bool,
    /// 只保留这些来源；为空表示不按来源过滤。
    pub sources: Vec<MessageSource>,
    /// 严格晚于该水位的行。
    pub after: Option<MqWatermark>,
    /// 上限；按 `(enqueued_at, id)` 升序取前 N 条。
    pub limit: Option<usize>,
}

/// 入队结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqEnqueueDisposition {
    /// 本次新接纳并已提交。
    Inserted,
    /// 同一 ID 的同一内容已在 MQ：复用原接纳结果（含原 `enqueued_at`）。
    Existing,
    /// 同一 ID 已在本 thread 历史：不再创建 MQ 行或处理轮次。
    AlreadyHandedOff,
}

/// 入队结果。只有 [`Inserted`](MqEnqueueDisposition::Inserted)/[`Existing`] 的
/// `entry.enqueued_at` 是持久事实；`AlreadyHandedOff` 的 entry 是请求内容的投影
/// （历史表不保存入队时间，MQ 行也已不存在）。
#[derive(Debug, Clone)]
pub struct MqEnqueueOutcome {
    pub disposition: MqEnqueueDisposition,
    pub entry: MqEntry,
}

/// 取消结论：返回被删除的整行（含原稿），不留撤回副本。
///
/// 变体尺寸不齐是刻意的：`Cancelled` 必须携带整行（原稿与策略都在里面），取消是低频操作，
/// 为一个字段更小的枚举把行装箱只会把「整行」这件事藏到第二层。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum MqCancelOutcome {
    Cancelled(MqEntry),
    /// 并发 Receive 已提交：消息在历史里，不能回删历史冒充取消。
    AlreadyHandedOff,
    /// 当前 MQ 与历史都没有这个 ID。
    NotFound,
}

/// Receive 对单行的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqReceiveDisposition {
    /// 写入历史后删除 MQ 行。
    Commit,
    /// 只删除已失效的 MQ 行，不写历史（旧 run 绑定的控制消息）。
    Suppress,
}

/// Receive 选中项：调用方给出它读到的行，存储按 `enqueued_at` 复核新鲜度。
#[derive(Debug, Clone)]
pub struct MqReceiveItem {
    pub entry: MqEntry,
    pub disposition: MqReceiveDisposition,
}

/// Receive 许可的用途：模型执行 or 同 thread 生命周期结算（不获得模型运行资格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqReceivePurpose {
    ModelLoop,
    NonModelSettlement,
}

/// 一次原子移交的选择集合。
#[derive(Debug, Clone)]
pub struct MqReceiveSelection {
    pub items: Vec<MqReceiveItem>,
    pub purpose: MqReceivePurpose,
    pub execution: ExecutionBinding,
}

/// 单行的移交结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqReceiveStatus {
    /// 本次写入历史并删除 MQ 行。
    Committed,
    /// 同 thread 历史已有同 ID 同内容：只删除 MQ 行。
    AlreadyPresent,
    /// 只删除了失效的 MQ 行，未写历史。
    Suppressed,
    /// MQ 行不存在或 `enqueued_at` 不再是选择时的值：什么都没写。
    Missing,
    /// 同 ID 的历史内容不同或属于其他 thread：删除 MQ 行、不写历史。
    Conflict,
}

/// 单行的移交结果；`entry` 是存储读到的行（`Missing` 为 `None`）。
#[derive(Debug, Clone)]
pub struct MqReceiveItemOutcome {
    pub id: MessageId,
    pub status: MqReceiveStatus,
    pub entry: Option<MqEntry>,
}

/// 一次 Receive 的逐项结果，顺序与选择一致。
#[derive(Debug, Clone)]
pub struct MqReceiveOutcome {
    pub items: Vec<MqReceiveItemOutcome>,
}

/// 显式发送的运行期结论。
///
/// `accepted` 只表示已写入本次发送选择——消息仍在 MQ，尚未移交；`missing` 要么已移交到
/// 历史，要么当前不存在。发送不写历史、不生成本地回执（`docs/design/thread-mq.md` §5.3）。
#[derive(Debug, Clone, Default)]
pub struct MqSendOutcome {
    pub accepted: Vec<MqEntry>,
    pub missing: Vec<MessageId>,
}

// ─── ThreadMqStore ───────────────────────────────────────────────────────────

/// 测试用的提交门：(进入通知, 放行信号)。
#[cfg(feature = "test-support")]
pub type ReceiveGate = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

/// 持久 MQ 的事实操作面：业务门面的**唯一**事实来源。
///
/// 实现必须满足 `SessionResources` 的 MQ 语义（`docs/design/thread-mq.md` §5）：
/// 入队判定顺序「历史 → MQ → 条件插入」，Receive 是原子移交，取消物理删除行。
/// 门面只在内存里保留唤醒信号与运行期发送选择，不复制任何行。
#[async_trait]
pub trait ThreadMqStore: Send + Sync {
    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome>;

    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>>;

    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome>;

    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome>;

    async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()>;

    /// 入队是否无需 I/O：为 true 时调用方可以在同步能力面里等到提交结果。
    fn supports_immediate_enqueue(&self) -> bool {
        false
    }

    /// 仅测试：让下一次原子提交在写入前暂停；默认实现忽略门控。
    #[cfg(feature = "test-support")]
    fn install_receive_gate(&self, _gate: ReceiveGate) {}
}

/// [`ThreadMqStore`] 的 `SessionResources` 转发实现：生产路径的唯一适配器。
#[derive(Clone)]
pub struct SessionResourcesMqStore {
    resources: Arc<dyn SessionResources>,
}

impl SessionResourcesMqStore {
    pub fn new(resources: Arc<dyn SessionResources>) -> Self {
        Self { resources }
    }
}

impl std::fmt::Debug for SessionResourcesMqStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionResourcesMqStore").finish()
    }
}

#[async_trait]
impl ThreadMqStore for SessionResourcesMqStore {
    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.resources.mq_enqueue(thread_id, input).await
    }

    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        self.resources.mq_query(thread_id, filter).await
    }

    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        self.resources.mq_cancel(thread_id, id).await
    }

    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        self.resources.mq_receive(thread_id, selection).await
    }

    async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        self.resources.mq_clear(thread_id).await
    }

    #[cfg(feature = "test-support")]
    fn install_receive_gate(&self, gate: ReceiveGate) {
        self.resources.install_receive_gate(gate);
    }
}
