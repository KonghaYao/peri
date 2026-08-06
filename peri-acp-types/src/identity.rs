//! §9 身份标识契约（`docs/top-level.md` §9 身份标识；伞形 PRD 决策 9/11）。
//!
//! 跨层消息统一携带 `(session_id, session_epoch, turn_id, attempt_id)`；
//! epoch / attempt_id 不可复用（防迟到消息命中新 session / 新 attempt）。
//!
//! 本模块是 §9 层间接口签名的先行落位（伞形 PRD 未决项 5）：
//! 只定义 canonical 类型与生成语义，各层接口引用这些签名随拆分子 issue 推进。
//! 语义已有（v2 事件携带 turn_id + session_id），此处补齐 epoch / attempt_id
//! 与四元组组合类型。

use serde::{Deserialize, Serialize};

use crate::thread::CancelPolicy;

/// Agent 唯一标识 — UUID v7（subagent 身份统一：child_thread_id → AgentId）。
///
/// v2 事件强制携带 `agent_id`（事件源 agent；SubAgent 场景即 source_agent_id）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(uuid::Uuid);

impl AgentId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    /// 从 UUID 构造 AgentId（供 subagent 身份统一：child_thread_id → AgentId）
    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<String> for AgentId {
    type Error = uuid::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        uuid::Uuid::parse_str(&value).map(Self::from_uuid)
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 会话纪元：session 每次创建/恢复递增。
///
/// 不可复用约束：epoch 只增不减（[`SessionEpoch::next`]），迟到消息携带的旧
/// epoch 无法命中新 session 实例。首次创建为 [`SessionEpoch::initial`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionEpoch(u64);

impl SessionEpoch {
    /// 首次创建的纪元（1；0 保留给"未知/未分配"场景）。
    pub const fn initial() -> Self {
        Self(1)
    }

    /// 递增到下个纪元（session 重建/恢复时调用）。epoch 不可复用，只增不减。
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// 底层值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Default for SessionEpoch {
    fn default() -> Self {
        Self::initial()
    }
}

/// Attempt ID：每次 attempt（一次可消费的 turn 执行）新生成。
///
/// 不可复用约束：uuid v7 每次生成唯一（时间有序），迟到/重复消息携带的旧
/// attempt_id 无法命中新 attempt。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttemptId(String);

impl AttemptId {
    /// 生成新 attempt_id（uuid v7）。
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// 底层字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AttemptId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Turn 身份：session 维度标识（session_id + epoch），跨层消息携带。
///
/// 用于区分"同一 session 的不同生命周期实例"（session 销毁重建后 epoch 递增，
/// 旧 epoch 的消息不再归属当前实例）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnIdentity {
    pub session_id: String,
    pub session_epoch: SessionEpoch,
}

impl TurnIdentity {
    pub fn new(session_id: impl Into<String>, session_epoch: SessionEpoch) -> Self {
        Self {
            session_id: session_id.into(),
            session_epoch,
        }
    }
}

/// Attempt 身份：完整四元组，跨层消息统一携带。
///
/// cancel 幂等判定针对 (session_id, turn_id, attempt_id)（PRD 决策 11），
/// 本四元组为消息层完整身份；cancel 请求可携带本结构定位目标 attempt。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttemptIdentity {
    pub session_id: String,
    pub session_epoch: SessionEpoch,
    pub turn_id: String,
    pub attempt_id: AttemptId,
}

impl AttemptIdentity {
    pub fn new(
        session_id: impl Into<String>,
        session_epoch: SessionEpoch,
        turn_id: impl Into<String>,
        attempt_id: AttemptId,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            session_epoch,
            turn_id: turn_id.into(),
            attempt_id,
        }
    }
}

/// 会话内事件序号（session_seq）：同一 session 内单调递增。
///
/// 事件契约（`docs/top-level.md` §9 事件契约）要求同 session 事件带单调序号，
/// 用于 TUI 侧去重判定 `(session_id, turn_id, sequence)` 与事件排序。
/// 首次事件为 [`SessionSeq::initial`]（1；0 保留给"未知/未分配"场景）。
///
/// **不实现 `Default`**：与「禁止 `Default::default()` 伪装缺失身份」的
/// canonical 契约一致（`2026-07-25-event-identity-diverges-across-dual-delivery-paths.md`），
/// 缺失序号必须显式表达（`Option<SessionSeq>`），不允许隐式归零。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionSeq(u64);

impl SessionSeq {
    /// 首个事件的序号（1；0 保留给"未知/未分配"场景）。
    pub const fn initial() -> Self {
        Self(1)
    }

    /// 递增到下个序号。单调不变量：`next` 严格大于当前值，绝不回退。
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// 底层值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 事件交付类别（canonical envelope 的 delivery class）。
///
/// 对应 v2 EventBus 三层通道的交付语义（`peri-agent/src/agent/events_v2.rs`）：
/// - [`EventDeliveryClass::Critical`]：render/state 层，有界 mpsc 通道，满时丢弃
/// - [`EventDeliveryClass::Broadcast`]：observe 层，broadcast 通道，慢消费者 lagging
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDeliveryClass {
    Critical,
    Broadcast,
}

/// canonical 事件 envelope：跨 transport 统一承载事件身份。
///
/// 契约源自 `2026-07-25-event-identity-diverges-across-dual-delivery-paths.md`：
/// 身份字段（turn_id / agent_id / session_seq）由事件源或聚合层填充，**不**
/// 由各 mapper 临时补齐；`message_id` 可选但语义明确（缺失用 `None`，不伪装）。
///
/// 本类型是 §9 层间接口签名的先行落位（伞形 PRD 未决项 5）：语义已有
/// （v2 事件携带 turn_id + agent_id；session_id 由 Runtime 聚合时按 session 补打），
/// 生产接线随 executor 拆分（L5）推进。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 由 Runtime 聚合时按 session 补打（Agent 层事件不携带）。
    pub session_id: String,
    /// session 生命周期实例（epoch 不可复用，防迟到消息命中新 session）。
    pub session_epoch: SessionEpoch,
    /// 事件源 agent 的 turn 纽带（v2 事件强制携带）。
    pub turn_id: String,
    /// 事件源 agent（v2 事件强制携带；SubAgent 场景即 source_agent_id）。
    pub agent_id: String,
    /// 同 session 单调序号（去重键 `(session_id, turn_id, sequence)` 的第三元）。
    pub session_seq: SessionSeq,
    /// 可选但语义明确的 message_id（v2 chunk 事件无 message 级身份时为 None）。
    pub message_id: Option<String>,
    /// 交付类别（critical 同步 / broadcast 观测）。
    pub delivery_class: EventDeliveryClass,
}

impl EventEnvelope {
    pub fn new(
        session_id: impl Into<String>,
        session_epoch: SessionEpoch,
        turn_id: impl Into<String>,
        agent_id: impl Into<String>,
        session_seq: SessionSeq,
        delivery_class: EventDeliveryClass,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            session_epoch,
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            session_seq,
            message_id: None,
            delivery_class,
        }
    }
}

/// §9 cancel 契约：cancel 请求的 canonical 类型（伞形 PRD 未决项 5 先行落位）。
///
/// - 幂等判定针对三元组 (session_id, turn_id, attempt_id)：`identity` 携带
///   （epoch 不可复用，防迟到 cancel 命中新 session 实例）
/// - cancel ≠ 清除待办：`clear_queue` 默认 `false`，MQ 未消费消息保留
///   （随下次循环作为新 attempt 输入）
/// - `policy`（Cascade/Independent）：判定与终止执行归 Agent 层（§2），
///   上层（Controller/Runtime）仅定位与传递，不解释取消语义（§6）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    /// 目标 attempt 的完整身份（四元组；幂等判定取其三元组）。
    pub identity: AttemptIdentity,
    /// 是否清除 MQ 待办（默认 `false`：cancel 不丢消息）。
    pub clear_queue: bool,
    /// 取消传播策略（Cascade/Independent；判定归 Agent，上层仅传递）。
    pub policy: CancelPolicy,
}

impl CancelRequest {
    /// 构造 cancel 请求：默认不清除 MQ 待办（§9：cancel ≠ 清除待办）。
    pub fn new(identity: AttemptIdentity, policy: CancelPolicy) -> Self {
        Self {
            identity,
            policy,
            clear_queue: false,
        }
    }

    /// 带 clear_queue 标志构造（§9：cancel 请求可带 clear_queue 标志，默认 false）。
    pub fn with_clear_queue(mut self, clear_queue: bool) -> Self {
        self.clear_queue = clear_queue;
        self
    }
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
