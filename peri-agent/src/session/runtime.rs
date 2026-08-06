//! Agent 运行时注册表条目与 cancel 最终执行权（3.0 归位，L5）。
//!
//! # 归位说明
//!
//! 自 `peri-acp/src/session/agent_runtime.rs` 迁入（L5：executor 拆分）。
//! `AgentRuntime` / `CancelPolicy` / `AgentStatus` 契约事实源为
//! `peri-acp-types`（`session` / `thread` 模块，经本模块 re-export）。
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

/// 契约类型（peri-acp-types 事实源，经 `crate::thread` re-export）。
pub use crate::thread::{AgentStatus, CancelPolicy};
pub use peri_acp_types::session::{
    cancel_all_agents, cancel_all_in, cancel_cascade_agents, cancel_cascade_in, AgentRuntime,
};

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
}
