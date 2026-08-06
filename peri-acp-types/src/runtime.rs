//! Runtime 层接口契约（§9 层间接口签名先行落位；伞形 PRD 未决项 5）。
//!
//! 本模块承载 Runtime 编排 Agent 层 session 运行单元的边界接口
//! （[`SessionHandle`]）与未补打事件的最小载体（[`UnstampedEvent`]）：
//! - [`SessionHandle`]：`session_id -> 运行句柄` 映射的 trait 面。实现方为
//!   Agent 层 session 工厂 / 装配点（L5）；Controller 经 Runtime 查映射发起
//!   run/cancel；ACP 层实现方（如每轮执行薄壳）只实现本 trait 并注册进
//!   Runtime，不直接调用 Agent 层执行本体
//! - [`UnstampedEvent`]：Agent 层业务事件的身份最小投影（携带 turn_id +
//!   agent_id + message_id + delivery_class）；session_id / session_seq 由
//!   [`Runtime::stamp`] 按 session 维度补打（`docs/top-level.md` §9 事件契约）
//!
//! 依赖方向：契约层无依赖（仅 serde/async-trait/tokio），各层引用本签名。

use std::time::Duration;

use async_trait::async_trait;

use crate::identity::{CancelRequest, EventDeliveryClass};
use crate::messages::MessageContent;

/// 未补打事件：Agent 层业务事件的身份最小投影。
///
/// §9 事件契约：事件携带 turn_id + agent_id（Agent 层事件不携带 session_id）；
/// session_id 与 session_seq 由 Runtime 按 session 维度补打。
/// 本类型为聚合补打与销毁 drain 的最小载体，不承载事件 payload
/// （payload 走 `peri_acp_types::event::EventMessage`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstampedEvent {
    /// 事件源 agent 的 turn 纽带（v2 事件强制携带）。
    pub turn_id: String,
    /// 事件源 agent（SubAgent 场景即 source_agent_id）。
    pub agent_id: String,
    /// 可选但语义明确的 message_id（v2 chunk 事件无 message 级身份时为 None）。
    pub message_id: Option<String>,
    /// 交付类别（critical 同步 / broadcast 观测）。
    pub delivery_class: EventDeliveryClass,
}

impl UnstampedEvent {
    /// 构造未补打事件。
    pub fn new(
        turn_id: impl Into<String>,
        agent_id: impl Into<String>,
        message_id: Option<String>,
        delivery_class: EventDeliveryClass,
    ) -> Self {
        Self {
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            message_id,
            delivery_class,
        }
    }
}

/// 运行句柄：Runtime 编排 Agent 层 session 运行单元的最小接口。
///
/// 语义（伞形 PRD 决策 20/21、`docs/top-level.md` §9）：
/// - [`SessionHandle::run`]：启动/恢复执行（run 语义，Controller 经 Runtime 发起）
/// - [`SessionHandle::cancel`]：取消当前执行（cancel 语义；携带 §9 三元组
///   (session_id, turn_id, attempt_id) 与 clear_queue/policy——幂等判定与
///   turn 终态唯一归 Agent 侧实现，上层仅传递）
/// - [`SessionHandle::submit_input`]：注入运行时输入（消息/工具注入面收口；
///   初始输入在会话启动参数，运行期输入经此注入）
/// - 销毁六阶段（§9 session 销毁顺序）：停收新输入 → 取消 owned tasks →
///   join（带 deadline）→ 超时 abort → 持久化事务收束 → drain 事件；
///   编排顺序由 Runtime 保证，本 trait 暴露各阶段最小操作
///
/// **与 `peri-agent::agent::stages::SessionHandle` 区分**：后者是 RCRA 阶段间
/// 共享上下文（turn/transcript/queue 引用），非运行句柄；本类型是 Runtime
/// 编排 Agent 层运行单元的边界接口。实现方为 Agent 层 session 工厂 / 装配点
/// （L5）或 ACP 层执行薄壳（过渡）；本契约不解释 Agent 层语义；trait 方法
/// 以 anyhow 表达层内错误，由 Runtime 边界包 context 为 [`crate::error`]
/// 对应枚举。
#[async_trait]
pub trait SessionHandle: Send + Sync {
    /// 启动/恢复执行（run 语义）。
    async fn run(&self) -> Result<(), anyhow::Error>;
    /// 取消当前执行（cancel 语义，携带 §9 cancel 请求）。
    ///
    /// 幂等：重复 cancel 针对同一 (session_id, turn_id, attempt_id) 结果一致、
    /// turn 终态唯一（Completed 或 Interrupted）——判定簿记在 Agent 侧
    /// （Agent 持有最终执行权，上层仅传递）；本方法为边界透传口。
    fn cancel(&self, request: &CancelRequest);
    /// 注入运行时输入（消息/工具注入面）：session 生命周期内的追加输入收口。
    ///
    /// 初始输入在会话启动参数（Controller 层 `LiteParams`），本方法收口
    /// 运行期输入注入（后续 user 输入经 Controller → Runtime 透传）。
    /// 错误经 anyhow 表达，由 Runtime 边界包 context。
    fn submit_input(&self, input: MessageContent) -> Result<(), anyhow::Error>;
    /// 销毁阶段 1：停收新输入。
    fn stop_accepting(&self);
    /// 销毁阶段 2：取消 owned tasks。
    fn cancel_owned(&self);
    /// 销毁阶段 3：带 deadline 的 join。
    ///
    /// 返回 `true` = deadline 内结束；`false` = 超时（调用方应执行
    /// 销毁阶段 4 [`SessionHandle::abort`]）。
    async fn join(&self, deadline: Duration) -> bool;
    /// 销毁阶段 4：超时 abort（fire-and-forget 强杀）。
    fn abort(&self);
    /// 销毁阶段 5：持久化事务收束（Thread/transcript = 持久真相，§9）。
    async fn persist(&self) -> Result<(), anyhow::Error>;
    /// 销毁阶段 6：排干剩余事件（未补打形态，由 Runtime 补打后投递）。
    fn drain(&self) -> Vec<UnstampedEvent>;
}
