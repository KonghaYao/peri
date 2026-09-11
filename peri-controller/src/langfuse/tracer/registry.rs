//! SubAgent 身份注册表(替代无身份 LIFO 栈)。
//!
//! 归属完全按事件侧 agent_id 路由,禁止任何"栈顶/当前活跃"近似:
//!
//! - `by_agent_id`:child_agent_id → [`ActiveSubagent`],stage/generation/tool 内容
//!   事件的 parent 归属(表 1)
//! - `invocations`:(父AgentId, ToolCallId) → [`SubagentInvocation`],Agent 工具调用
//!   与 child 的关联(表 2)
//! - 生命周期由 `ObserveEvent::SubagentStart`(创建 AGENT obs)与 `SubagentStop`
//!   (关闭)驱动;`ToolEnded` 不再关闭 subagent;`on_turn_end` 仅兜底
//! - 事件乱序经"注册闸门"有界缓存 + parent-first 重放;未知/丢失一律进入
//!   [`SubagentStatus::Incomplete`] 诊断分支,禁止静默挂主 agent

use std::collections::{HashMap, VecDeque};

use super::tool_batch::ToolBatch;

mod closure;
mod gate;
mod registration;
mod types;

pub(crate) use types::*;

/// 注册闸门缓存上限(有界,防未知 agent 无限灌入)
pub(crate) const GATE_CACHE_LIMIT: usize = 64;

/// 表 1:内容归属条目(AGENT obs 生命周期 + 该 child 自己的 ToolBatch)
pub(crate) struct ActiveSubagent {
    /// AGENT obs id(join 时生成;PendingInvocation 阶段为空串)
    pub observation_id: String,
    /// 观测关闭与异常诊断分开记录；仅统一关闭入口置 true。
    observation_closed: bool,
    /// 冻结:join 时从 invocation 取的父 stage span id(防漂移/防环)
    pub parent_observation_id: String,
    /// Start join 时刻(rfc3339)
    pub start_time: String,
    /// 该 AGENT obs 下最后一个子内容事件的处理时刻(rfc3339);
    /// 关闭时 end_time 取 max(stop_time, last_content_time),
    /// 防 Stop 事件乱序/同毫秒早到把 AGENT obs 时长虚标为 0
    pub last_content_time: String,
    pub agent_name: String,
    pub is_background: bool,
    /// 该 subagent 自己的 ToolBatch(内容工具归属)
    pub tool_batch: ToolBatch,
    pub status: SubagentStatus,
    /// Stop 载荷(result/is_error/stop_time);Stop 先到时暂存
    pub stop: Option<SubagentStopInfo>,
    /// 已绑定的 (parent_agent_id, tool_call_id)
    pub invocation_key: Option<(String, String)>,
    /// 父 Agent 工具 input(join 时从 invocation 克隆;AGENT obs 的 input)
    pub input: Option<serde_json::Value>,
}

pub(crate) struct SubagentRegistry {
    /// 表 1:内容归属(child_agent_id → ActiveSubagent)
    by_agent_id: HashMap<String, ActiveSubagent>,
    /// 表 2:调用关联((parent_agent_id, tool_call_id) → SubagentInvocation)
    invocations: HashMap<(String, String), SubagentInvocation>,
    /// 未绑定 invocation 的 FIFO 索引(join 用;回收时移除)
    unbounded_invocations: VecDeque<(String, String)>,
    /// Start 先于父 ToolStart 到达(等 join)
    pending_starts: VecDeque<StartPending>,
    /// child 内容事件缓存(等 Start;有界)
    gate_cache: VecDeque<(String, GateEvent)>,
    /// 注入的主 agent 身份(per-turn;None = 未注入,fallback 兼容旧测试)
    main_agent_id: Option<String>,
    /// 未注入 main_agent_id 时的 fallback 判定 warn 只打一次(Cell:is_main_agent 只借 &self)
    warned_no_main_agent: std::cell::Cell<bool>,
    /// incomplete 累计计数(供诊断/测试)
    incomplete_count: u64,
}

impl SubagentRegistry {
    pub(crate) fn new() -> Self {
        Self {
            by_agent_id: HashMap::new(),
            invocations: HashMap::new(),
            unbounded_invocations: VecDeque::new(),
            pending_starts: VecDeque::new(),
            gate_cache: VecDeque::new(),
            main_agent_id: None,
            warned_no_main_agent: std::cell::Cell::new(false),
            incomplete_count: 0,
        }
    }

    // ── 主 agent 身份 ────────────────────────────────────────────────────────

    pub(crate) fn set_main_agent_id(&mut self, id: String) {
        self.main_agent_id = Some(id);
    }

    /// 主 agent 判定:已注入 → 相等;未注入 → 非 registry 成员视为主 agent
    /// (兼容旧测试/未注入路径,必须 warn 一次)
    pub(crate) fn is_main_agent(&self, agent_id: &str) -> bool {
        if let Some(main) = &self.main_agent_id {
            return agent_id == main;
        }
        let is_main = !self.by_agent_id.contains_key(agent_id);
        if is_main && !self.warned_no_main_agent.get() {
            tracing::warn!(
                target: "langfuse::subagent",
                %agent_id,
                "main_agent_id 未注入:非 registry 成员视为主 agent(仅未注入路径)"
            );
            self.warned_no_main_agent.set(true);
        }
        is_main
    }

    // ── 内容归属决策 ──────────────────────────────────────────────────────────

    /// 内容事件归属:by_agent_id(Active/StopReceived)→ Subagent;
    /// PendingInvocation/Incomplete → Unknown(走闸门/跳过);主 agent → Main
    pub(crate) fn ownership(&self, agent_id: &str) -> Ownership {
        if let Some(sa) = self.by_agent_id.get(agent_id) {
            return match sa.status {
                SubagentStatus::PendingInvocation => Ownership::Unknown,
                SubagentStatus::Incomplete(_) => Ownership::Unknown,
                _ => Ownership::Subagent,
            };
        }
        if self.is_main_agent(agent_id) {
            return Ownership::Main;
        }
        Ownership::Unknown
    }

    pub(crate) fn observation_id_of(&self, agent_id: &str) -> Option<String> {
        self.by_agent_id
            .get(agent_id)
            .filter(|sa| !sa.observation_id.is_empty())
            .map(|sa| sa.observation_id.clone())
    }

    /// 该 agent 自己的 ToolBatch(仅 Subagent 归属时调用)
    pub(crate) fn tool_batch_mut(&mut self, agent_id: &str) -> &mut ToolBatch {
        &mut self
            .by_agent_id
            .get_mut(agent_id)
            .expect("registered subagent")
            .tool_batch
    }

    pub(crate) fn has_invocation(&self, agent_id: &str, tool_call_id: &str) -> bool {
        self.invocations
            .contains_key(&(agent_id.to_string(), tool_call_id.to_string()))
    }

    // ── 诊断/测试 ────────────────────────────────────────────────────────────

    pub(crate) fn status_of(&self, agent_id: &str) -> Option<&SubagentStatus> {
        self.by_agent_id.get(agent_id).map(|sa| &sa.status)
    }

    #[cfg(test)]
    pub(crate) fn invocation_key_of(&self, agent_id: &str) -> Option<(String, String)> {
        self.by_agent_id
            .get(agent_id)
            .and_then(|sa| sa.invocation_key.clone())
    }

    pub(crate) fn incomplete_count(&self) -> u64 {
        self.incomplete_count
    }

    pub(crate) fn by_agent_id_len(&self) -> usize {
        self.by_agent_id.len()
    }

    fn mark_incomplete(&mut self, child_agent_id: &str, reason: IncompleteReason) {
        if let Some(sa) = self.by_agent_id.get_mut(child_agent_id) {
            if let SubagentStatus::Incomplete(_) = sa.status {
                return; // 已是终态
            }
            sa.status = SubagentStatus::Incomplete(reason.clone());
        } else {
            // 未知 agent(Start 从未到达):插入占位记录(orphan 标记,不产生 obs,
            // 后续内容事件归属 Unknown 继续走闸门/丢弃)
            let now = chrono::Utc::now().to_rfc3339();
            self.by_agent_id.insert(
                child_agent_id.to_string(),
                ActiveSubagent {
                    observation_id: String::new(),
                    observation_closed: false,
                    parent_observation_id: String::new(),
                    start_time: now.clone(),
                    last_content_time: now,
                    agent_name: "unknown".to_string(),
                    is_background: false,
                    tool_batch: ToolBatch::new(),
                    status: SubagentStatus::Incomplete(reason.clone()),
                    stop: None,
                    invocation_key: None,
                    input: None,
                },
            );
        }
        self.incomplete_count += 1;
        tracing::warn!(
            target: "langfuse::subagent",
            %child_agent_id,
            reason = ?reason,
            "subagent 标记 incomplete"
        );
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;

#[cfg(test)]
#[path = "registry_lifecycle_test.rs"]
mod lifecycle_tests;
