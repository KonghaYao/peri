//! Runtime — 多 session 编排器（`docs/top-level.md` §3）。
//!
//! 薄编排：唯一持有 `session_id -> SessionHandle` 映射；不持有 session 状态、
//! 无持久态、无业务配置（状态在 Agent 层各 session 内，其余全部注入）。
//!
//! 事件聚合补打（§9 事件契约）：Agent 层事件携带 turn_id + agent_id；
//! session_id 由本层按 session 维度补打，session_seq 单调递增。生产接线
//! （Agent EventBus → [`Runtime::stamp`]）随 executor 拆分（L5）落地，
//! 本模块为聚合补打逻辑的落位点。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::RwLock;

use peri_acp_types::identity::{
    CancelRequest, EventDeliveryClass, EventEnvelope, SessionEpoch, SessionSeq,
};

use crate::error::RuntimeError;

/// 未补打事件：Agent 层业务事件的身份最小投影。
///
/// §9 事件契约：事件携带 turn_id + agent_id（Agent 层事件不携带 session_id）；
/// session_id 与 session_seq 由 [`Runtime::stamp`] 按 session 维度补打。
/// 本类型为聚合补打与销毁 drain 的最小载体，不承载事件 payload；
/// 生产接线（Agent EventBus → 本类型）随 executor 拆分（L5）落地。
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

/// 运行句柄：Runtime 编排 Agent 层 session 运行单元的最小接口。
///
/// 语义（伞形 PRD 决策 20/21、`docs/top-level.md` §9）：
/// - [`SessionHandle::run`]：启动/恢复执行（run 语义，Controller 经 Runtime 发起）
/// - [`SessionHandle::cancel`]：取消当前执行（cancel 语义；携带 §9 三元组
///   (session_id, turn_id, attempt_id) 与 clear_queue/policy——幂等判定与
///   turn 终态唯一归 Agent 侧实现，上层仅传递）
/// - 销毁六阶段（§9 session 销毁顺序）：停收新输入 → 取消 owned tasks →
///   join（带 deadline）→ 超时 abort → 持久化事务收束 → drain 事件；
///   编排顺序由 [`Runtime::destroy`] 保证，本 trait 暴露各阶段最小操作
///
/// **与 `peri-agent::agent::stages::SessionHandle` 区分**：后者是 RCRA 阶段间
/// 共享上下文（turn/transcript/queue 引用），非运行句柄；本类型是 Runtime
/// 编排 Agent 层运行单元的边界接口。实现方为 Agent 层 session 工厂 / 装配点
/// （L5），本 crate 不解释 Agent 层语义；trait 方法以 anyhow 表达层内错误，
/// 由 Runtime 边界包 context 为 [`RuntimeError`]。
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

/// 映射条目：句柄 + 事件补打簿记（epoch/seq）。
///
/// seq/epoch 是事件聚合补打的簿记，非 session 业务状态——session 业务状态
/// 仍在 Agent 层（§3 无状态原则：不持有 session 状态，其余全部注入）。
struct SessionEntry {
    handle: Arc<dyn SessionHandle>,
    epoch: SessionEpoch,
    seq: SessionSeq,
}

/// Runtime — 多 session 编排器（薄）。
///
/// 唯一持有 `session_id -> SessionHandle` 映射（§3）。职责：
/// - 创建/销毁 session 的编排入口（[`Runtime::register`]/[`Runtime::destroy`]，
///   经 Agent 层工厂创建后注入）
/// - 事件聚合补打（[`Runtime::stamp`]）：session_id 按 session 维度补打、
///   session_seq 单调递增（§9 事件契约）
/// - cancel 定位与转发（[`Runtime::cancel`]）：只定位与转发，不解释取消语义（§6）
pub struct Runtime {
    sessions: RwLock<HashMap<String, SessionEntry>>,
}

impl Runtime {
    /// 创建空 Runtime。
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 注册 session 句柄（经 Agent 层工厂创建后注入）。
    ///
    /// 同 session_id 已注册时报 [`RuntimeError::SessionAlreadyRegistered`]
    /// （防双注册撞车）；重建须先 [`Runtime::destroy`] 移除旧实例。
    /// epoch 自 [`SessionEpoch::initial`] 起；重建递增（`SessionEpoch::next`）
    /// 属持久化恢复路径（L5），本映射不持有跨实例记忆（无持久态）。
    pub fn register<H>(
        &self,
        session_id: impl Into<String>,
        handle: Arc<H>,
    ) -> Result<(), RuntimeError>
    where
        H: SessionHandle + 'static,
    {
        let session_id = session_id.into();
        let mut guard = self.sessions.write();
        if guard.contains_key(&session_id) {
            return Err(RuntimeError::SessionAlreadyRegistered(session_id));
        }
        guard.insert(
            session_id,
            SessionEntry {
                handle,
                epoch: SessionEpoch::initial(),
                seq: SessionSeq::initial(),
            },
        );
        Ok(())
    }

    /// 已注册 session_id 列表（无顺序保证）。
    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.read().keys().cloned().collect()
    }

    /// 该 session 是否已注册。
    pub fn contains(&self, session_id: &str) -> bool {
        self.sessions.read().contains_key(session_id)
    }

    /// 取句柄引用（查映射）。
    pub fn handle(&self, session_id: &str) -> Option<Arc<dyn SessionHandle>> {
        self.sessions
            .read()
            .get(session_id)
            .map(|entry| Arc::clone(&entry.handle))
    }

    /// 事件聚合补打（§9 事件契约）：为 Agent 层未补打事件补 session_id 与
    /// 单调 session_seq，返回 canonical envelope。
    ///
    /// - session_id：按 session 维度补打（Agent 层事件不携带）
    /// - session_seq：同 session 单调递增（[`SessionSeq::initial`] 起，绝不回退）
    /// - session_epoch：透传当前实例纪元
    ///
    /// 未注册 session 报 [`RuntimeError::UnknownSession`]——销毁后的迟到事件
    /// 无法补打，与 epoch 不可复用契约共同防迟到消息命中新 session。
    pub fn stamp(
        &self,
        session_id: &str,
        event: &UnstampedEvent,
    ) -> Result<EventEnvelope, RuntimeError> {
        let mut guard = self.sessions.write();
        let entry = guard
            .get_mut(session_id)
            .ok_or_else(|| RuntimeError::UnknownSession(session_id.to_string()))?;
        let seq = entry.seq;
        entry.seq = seq.next();
        Ok(EventEnvelope {
            session_id: session_id.to_string(),
            session_epoch: entry.epoch,
            turn_id: event.turn_id.clone(),
            agent_id: event.agent_id.clone(),
            session_seq: seq,
            message_id: event.message_id.clone(),
            delivery_class: event.delivery_class,
        })
    }

    /// 取消：查映射 → 转发句柄（§9 cancel 契约：Agent 持有最终执行权与幂等
    /// 判定，上层仅传递；本方法只定位与转发，不解释取消语义——§6）。
    ///
    /// 定位依据为请求携带的三元组 (session_id, turn_id, attempt_id)
    /// （`CancelRequest.identity`）：session_id 用于查映射，turn_id/attempt_id
    /// 原样透传给句柄，由 Agent 侧判定是否命中当前 attempt。
    pub fn cancel(&self, request: &CancelRequest) -> Result<(), RuntimeError> {
        let handle = self
            .handle(&request.identity.session_id)
            .ok_or_else(|| RuntimeError::UnknownSession(request.identity.session_id.clone()))?;
        handle.cancel(request);
        Ok(())
    }

    /// 启动执行：查映射 → 转发句柄 run（§6：run Session）。
    pub async fn run(&self, session_id: &str) -> Result<(), RuntimeError> {
        let handle = self
            .handle(session_id)
            .ok_or_else(|| RuntimeError::UnknownSession(session_id.to_string()))?;
        handle
            .run()
            .await
            .map_err(|err| RuntimeError::RunFailed(session_id.to_string(), err))?;
        Ok(())
    }

    /// 销毁 session（§9 session 销毁顺序契约，编排顺序固定）：
    ///
    /// 1. 停收新输入 → 2. 取消 owned tasks → 3. join（带 deadline）→
    /// 4. 超时 abort → 5. 持久化事务收束 → 6. drain 事件（补打）→ 7. 移除映射
    ///
    /// 返回 drain 出的补打事件（envelope），由调用方（Controller）投递。
    /// 持久化失败时返回错误且**不移除映射**（已执行阶段幂等设计，重试安全）。
    pub async fn destroy(
        &self,
        session_id: &str,
        join_deadline: Duration,
    ) -> Result<Vec<EventEnvelope>, RuntimeError> {
        let handle = self
            .handle(session_id)
            .ok_or_else(|| RuntimeError::UnknownSession(session_id.to_string()))?;
        // 1-2：停收新输入 + 取消 owned tasks
        handle.stop_accepting();
        handle.cancel_owned();
        // 3：join（带 deadline）；4：超时 abort
        if !handle.join(join_deadline).await {
            handle.abort();
        }
        // 5：持久化事务收束（失败上抛，映射保留）
        handle
            .persist()
            .await
            .map_err(|err| RuntimeError::PersistFailed(session_id.to_string(), err))?;
        // 6：drain 事件（补打）；7：移除映射
        let drained = handle
            .drain()
            .into_iter()
            .map(|event| self.stamp(session_id, &event))
            .collect::<Result<Vec<_>, _>>()?;
        self.sessions.write().remove(session_id);
        Ok(drained)
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
