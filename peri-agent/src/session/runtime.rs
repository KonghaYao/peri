//! Agent 运行时注册表条目与 cancel 最终执行权（3.0 归位，L5）。
//!
//! # 归位说明
//!
//! 自 `peri-acp/src/session/agent_runtime.rs` 迁入（L5：executor 拆分）。
//! `CancelPolicy` / `AgentStatus` 不再本地重复定义，统一使用
//! `peri-acp-types` 契约类型（经 [`crate::thread`] re-export，串行化
//! 与持久化事实源，非法值不静默 fallback）。
//!
//! # cancel 最终执行权（top-level.md §2 / §9）
//!
//! - 生命周期状态（active_agents 注册表）按 §0 归 Agent 层（当前迁移阶段
//!   由 ACP `AcpSession.active_agents` 持有，类型归位后判定逻辑先行落位，
//!   注册表字段随 L2/L5 运行态归位迁入 [`crate::session::Session`]）
//! - Cascade/Independent 判定与终止执行（[`cancel_cascade_agents`] /
//!   [`cancel_all_agents`]）归本层；上层（ACP/Controller）仅负责定位
//!   （查 session 映射）并传递 runtimes 集合
//! - Model 执行中止由 Agent 层 `run_react_loop` 的 cancel 检查发起（Receive
//!   唯一退出口，`stages/mod.rs`），本模块只处理子 agent 运行时 token

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use crate::thread::ThreadId;
/// 契约类型（peri-acp-types 事实源，经 `crate::thread` re-export）。
pub use crate::thread::{AgentStatus, CancelPolicy};

/// 运行时 agent 实例（子 agent 取消判定与终止执行的载体）。
pub struct AgentRuntime {
    pub thread_id: ThreadId,
    pub cancel_token: CancellationToken,
    pub cancel_policy: CancelPolicy,
    pub status: AgentStatus,
}

impl AgentRuntime {
    pub fn new(thread_id: ThreadId, cancel_policy: CancelPolicy) -> Self {
        Self {
            thread_id,
            cancel_token: CancellationToken::new(),
            cancel_policy,
            status: AgentStatus::Active,
        }
    }
}

/// cancel 判定（Cascade/Independent）与终止执行：取消所有 Cascade policy 的
/// 同步子 agent（跟随父 agent 取消）。Independent（bg）子 agent 不受影响，
/// 仅跟随 session 根取消。
///
/// 断言与迁移前 `AcpSession::cancel_cascade_children` 完全一致（L5 纯归位，
/// 行为语义不重写）。上层定位后调用本函数即完成最终执行。
pub fn cancel_cascade_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        if runtime.cancel_policy == CancelPolicy::Cascade {
            runtime.cancel_token.cancel();
        }
    }
}

/// 取消所有 agent（session 结束 / close_session 时）。
///
/// 断言与迁移前 `AcpSession::cancel_all_agents` 完全一致。
pub fn cancel_all_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        runtime.cancel_token.cancel();
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表执行 cascade 判定。
pub fn cancel_cascade_in<'a>(
    runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>,
) {
    for map in runtimes {
        cancel_cascade_agents(map.values());
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表取消全部。
pub fn cancel_all_in<'a>(runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>) {
    for map in runtimes {
        cancel_all_agents(map.values());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::CancelPolicy as Policy;

    fn make_runtime(policy: Policy) -> AgentRuntime {
        AgentRuntime::new("thread-1".to_string(), policy)
    }

    /// Cascade 子 agent 跟随父取消；Independent 不受 cascade 影响。
    #[test]
    fn cancel_cascade_only_cancels_cascade_agents() {
        let cascade = make_runtime(Policy::Cascade);
        let independent = make_runtime(Policy::Independent);
        cancel_cascade_agents([&cascade, &independent]);
        assert!(cascade.cancel_token.is_cancelled());
        assert!(!independent.cancel_token.is_cancelled());
    }

    /// cancel_all 不区分 policy，全部终止（session 结束语义）。
    #[test]
    fn cancel_all_cancels_every_agent() {
        let cascade = make_runtime(Policy::Cascade);
        let independent = make_runtime(Policy::Independent);
        cancel_all_agents([&cascade, &independent]);
        assert!(cascade.cancel_token.is_cancelled());
        assert!(independent.cancel_token.is_cancelled());
    }

    /// 空注册表安全（不存在不 panic，与迁移前 ACP 语义一致）。
    #[test]
    fn cancel_on_empty_registry_is_noop() {
        let empty: HashMap<ThreadId, AgentRuntime> = HashMap::new();
        cancel_cascade_in([&empty]);
        cancel_all_in([&empty]);
    }
}
