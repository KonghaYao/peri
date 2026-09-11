//! 注册、乱序缓存和观测关闭之间传递的内部数据；不持有独立注册表。

use peri_agent::agent::events::Stage;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

use super::super::tool_batch::ToolsBatchFlush;

/// incomplete 诊断原因(终态,不再变化)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IncompleteReason {
    /// 内容事件 agent_id 未注册且非 main_agent_id(Start 从未到达,残留缓存清理)
    UnknownAgent,
    /// 收到内容事件/ToolEnded 但 Start 从未到达
    MissingStart,
    /// 已 Active/StopReceived/Closed 又收到 Start
    DuplicateStart,
    /// 已 Closed/Incomplete 又收到 Stop
    DuplicateStop,
    /// 注册闸门缓存满被逐出
    CacheOverflow,
    /// Start join 失败(父 ToolStart 丢失,on_turn_end 兜底)
    ParentLost,
    /// ToolStart 先到、Start 缓存超时/溢出
    StartLost,
    /// Start 已 join(AGENT obs 已建)但 Stop 未到(on_turn_end 兜底)
    MissingStop,
}

/// 单个 subagent 的完整生命周期状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubagentStatus {
    /// Start 已到、父 ToolStart 未到(等 join;AGENT obs 尚未创建)
    PendingInvocation,
    /// join 完成,AGENT obs 已创建(ObservationCreate 已入队)
    Active,
    /// Stop 已到、父 ToolEnded 未到(等回收)
    StopReceived,
    /// AGENT obs 已关闭,invocation 已回收
    Closed,
    /// 异常终态(不再变化)
    Incomplete(IncompleteReason),
}

/// Stop 载荷
pub(crate) struct SubagentStopInfo {
    pub result: String,
    pub is_error: bool,
    pub stop_time: String,
}

/// 表 2:工具调用关联条目
#[derive(Clone)]
pub(crate) struct SubagentInvocation {
    /// 事件侧父 agent_id(ToolStart 携带)
    pub parent_agent_id: String,
    pub tool_call_id: String,
    /// join 时冻结的父 stage span(AGENT obs 的 parent)
    pub parent_stage_span_id: String,
    pub input: serde_json::Value,
    /// 已绑定 child_agent_id(Start join 前可为 None)
    pub bound_child: Option<String>,
    /// ToolEnded 的 output(Stop 先/后到都不丢不重)
    pub deferred_output: Option<String>,
    /// 父 ToolEnded 已处理
    pub tool_ended: bool,
    /// child Stop 已到
    pub stop_received: bool,
}

/// 注册闸门缓存的事件(tracer 入参级,重放时直接回放对应 on_* 调用)
pub(crate) enum GateEvent {
    StageStarted {
        agent_id: String,
        stage: Stage,
        turn_id: String,
    },
    LlmCallStart {
        agent_id: String,
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    },
    ToolStart {
        agent_id: String,
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolEnd {
        agent_id: String,
        tool_call_id: String,
        output: String,
        is_error: bool,
    },
}

impl GateEvent {
    pub(crate) fn agent_id(&self) -> &str {
        match self {
            GateEvent::StageStarted { agent_id, .. }
            | GateEvent::LlmCallStart { agent_id, .. }
            | GateEvent::ToolStart { agent_id, .. }
            | GateEvent::ToolEnd { agent_id, .. } => agent_id,
        }
    }
}

/// Start 先于父 ToolStart 到达时的等待条目
pub(crate) struct StartPending {
    pub child_agent_id: String,
    /// SubagentStart.agent_id(父)
    pub parent_agent_id: String,
    pub agent_name: String,
    pub is_background: bool,
}

/// AGENT obs 创建(open)所需信息,tracer 据此发 ObservationCreate(无 end_time)
pub(crate) struct AgentObsStart {
    pub observation_id: String,
    pub parent_observation_id: String,
    pub start_time: String,
    pub agent_name: String,
    /// 父 Agent 工具 input(AGENT obs 的 input)
    pub input: Option<serde_json::Value>,
}

/// AGENT obs 关闭所需全部信息,tracer 据此发 ObservationUpdate + flush child tool_batch
pub(crate) struct ClosedSubagent {
    /// 事件侧 child_agent_id(关闭时兜底清理该 agent 的活跃 stage 用)
    pub agent_id: String,
    pub observation_id: String,
    pub parent_observation_id: String,
    pub start_time: String,
    pub agent_name: String,
    /// 父 Agent 工具 input(AGENT obs 的 input)
    pub input: Option<serde_json::Value>,
    /// AGENT obs 的 output(优先 Stop result,空则取父工具 deferred_output)
    pub output: String,
    pub stop_time: String,
    pub is_error: bool,
    /// child tool_batch 的 flush 结果(可能为空批次)
    pub flush: ToolsBatchFlush,
    /// 非 None 表示兜底/异常关闭(metadata 携带 incomplete_reason)
    pub incomplete_reason: Option<IncompleteReason>,
}

/// on_subagent_start 的结果
#[allow(clippy::large_enum_variant)] // replayed/ClosedSubagent 体积大,一次性结果非热点
pub(crate) enum SubagentStartOutcome {
    /// join 成功:AGENT obs 已创建(open),gate 事件已取出待重放
    Joined {
        obs: AgentObsStart,
        replayed: Vec<GateEvent>,
        /// join 时 Stop 已到且父 ToolEnded 已到 → 立即关闭
        immediately_close: Option<ClosedSubagent>,
    },
    /// Start 已登记(pending_starts),等父 ToolStart join
    Pending,
    /// 重复 Start(已标记 DuplicateStart)
    Duplicate,
}

/// 内容归属决策
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ownership {
    /// 主 agent 域(agent_observation_id 或主活跃 stage)
    Main,
    /// 已注册 subagent(obs 已创建,可正常归属)
    Subagent,
    /// 未知/PendingInvocation/已 Incomplete:走注册闸门或跳过
    Unknown,
}
