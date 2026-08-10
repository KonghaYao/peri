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

use peri_agent::agent::events::Stage;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

use super::tool_batch::{ToolBatch, ToolsBatchFlush};

/// 注册闸门缓存上限(有界,防未知 agent 无限灌入)
pub(crate) const GATE_CACHE_LIMIT: usize = 64;

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

/// 表 1:内容归属条目(AGENT obs 生命周期 + 该 child 自己的 ToolBatch)
pub(crate) struct ActiveSubagent {
    /// AGENT obs id(join 时生成;PendingInvocation 阶段为空串)
    pub observation_id: String,
    /// 冻结:join 时从 invocation 取的父 stage span id(防漂移/防环)
    pub parent_observation_id: String,
    /// Start join 时刻(rfc3339)
    pub start_time: String,
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

    // ── 注册闸门(有界缓存 + 重放) ─────────────────────────────────────────────

    /// 缓存一条未知 agent 的内容事件。返回 true = 已缓存(等待 Start join 重放);
    /// false = 缓存失败(溢出逐出后拒绝)。
    pub(crate) fn try_gate(&mut self, ev: GateEvent) -> bool {
        let agent_id = ev.agent_id().to_string();
        if self.gate_cache.len() >= GATE_CACHE_LIMIT {
            // 溢出:逐出最旧事件(丢弃,不重放);若其 agent 的 Start 正在等待 join
            // (pending_starts),该 child 已无法完整重放 → 标 CacheOverflow。
            if let Some((evicted_agent, _)) = self.gate_cache.pop_front() {
                if self
                    .pending_starts
                    .iter()
                    .any(|sp| sp.child_agent_id == evicted_agent)
                {
                    self.pending_starts
                        .retain(|sp| sp.child_agent_id != evicted_agent);
                    self.mark_incomplete(&evicted_agent, IncompleteReason::CacheOverflow);
                }
                tracing::warn!(
                    target: "langfuse::subagent",
                    %evicted_agent,
                    gate_len = self.gate_cache.len(),
                    "gate_cache 溢出,逐出最旧事件(不重放)"
                );
            }
            // 缓存满时拒绝新事件:直接丢弃,按未知处理
            tracing::warn!(
                target: "langfuse::subagent",
                %agent_id,
                "gate_cache 已满,拒绝缓存新事件(丢弃,不挂主 agent)"
            );
            return false;
        }
        self.gate_cache.push_back((agent_id, ev));
        true
    }

    /// 取出该 child 的全部 gate 缓存事件(按原顺序),并从缓存移除
    pub(crate) fn take_gated_events(&mut self, child_agent_id: &str) -> Vec<GateEvent> {
        let mut taken = Vec::new();
        let mut i = 0;
        while i < self.gate_cache.len() {
            if self.gate_cache[i].0 == child_agent_id {
                let (_, ev) = self.gate_cache.remove(i).unwrap();
                taken.push(ev);
            } else {
                i += 1;
            }
        }
        taken
    }

    pub(crate) fn gated_len(&self) -> usize {
        self.gate_cache.len()
    }

    // ── invocation 注册与 join ───────────────────────────────────────────────

    /// Agent/Task 工具 ToolStart:登记 invocation(不创建任何 AGENT obs),
    /// 若已有等待中的 Start(父 agent 匹配)则立即 join。
    pub(crate) fn register_invocation(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        input: &serde_json::Value,
        parent_stage_span_id: &str,
    ) -> Option<SubagentStartOutcome> {
        let key = (agent_id.to_string(), tool_call_id.to_string());
        if !self.invocations.contains_key(&key) {
            self.invocations.insert(
                key.clone(),
                SubagentInvocation {
                    parent_agent_id: agent_id.to_string(),
                    tool_call_id: tool_call_id.to_string(),
                    parent_stage_span_id: parent_stage_span_id.to_string(),
                    input: input.clone(),
                    bound_child: None,
                    deferred_output: None,
                    tool_ended: false,
                    stop_received: false,
                },
            );
            self.unbounded_invocations.push_back(key.clone());
        }
        // 尝试 join 等待中的 Start(parent 匹配)
        let child = self
            .pending_starts
            .iter()
            .find(|sp| sp.parent_agent_id == agent_id)
            .map(|sp| sp.child_agent_id.clone());
        if let Some(child) = child {
            return self.try_join(&child);
        }
        None
    }

    /// SubagentStart:登记(占位 PendingInvocation + pending_starts)并尝试 join。
    pub(crate) fn on_subagent_start(
        &mut self,
        parent_agent_id: &str,
        child_agent_id: &str,
        agent_name: &str,
        is_background: bool,
    ) -> SubagentStartOutcome {
        if self.by_agent_id.contains_key(child_agent_id) {
            tracing::warn!(
                target: "langfuse::subagent",
                %child_agent_id,
                "SubagentStart 重复(已有活跃/终态记录),标记 DuplicateStart"
            );
            self.mark_incomplete(child_agent_id, IncompleteReason::DuplicateStart);
            return SubagentStartOutcome::Duplicate;
        }
        // 占位登记:防 Stop/重复 Start 竞态;join 成功后补 obs 字段
        self.by_agent_id.insert(
            child_agent_id.to_string(),
            ActiveSubagent {
                observation_id: String::new(),
                parent_observation_id: String::new(),
                start_time: chrono::Utc::now().to_rfc3339(),
                agent_name: agent_name.to_string(),
                is_background,
                tool_batch: ToolBatch::new(),
                status: SubagentStatus::PendingInvocation,
                stop: None,
                invocation_key: None,
                input: None,
            },
        );
        self.pending_starts.push_back(StartPending {
            child_agent_id: child_agent_id.to_string(),
            parent_agent_id: parent_agent_id.to_string(),
            agent_name: agent_name.to_string(),
            is_background,
        });
        tracing::info!(
            target: "langfuse::subagent",
            event = "subagent_start",
            %parent_agent_id,
            %child_agent_id,
            %agent_name,
            is_background,
            "SubagentStart 登记(pending join)"
        );
        if let Some(outcome) = self.try_join(child_agent_id) {
            return outcome;
        }
        SubagentStartOutcome::Pending
    }

    /// 尝试 join 指定 child 的 pending Start。成功 → 冻结父 span、创建 obs 字段、
    /// 取出 gate 事件;若 Stop 与父 ToolEnded 均已到 → 立即关闭。
    fn try_join(&mut self, child_agent_id: &str) -> Option<SubagentStartOutcome> {
        // 已被标记 incomplete(如缓存溢出)的 child 不再 join
        if matches!(
            self.by_agent_id.get(child_agent_id).map(|sa| &sa.status),
            Some(SubagentStatus::Incomplete(_))
        ) {
            self.pending_starts
                .retain(|sp| sp.child_agent_id != child_agent_id);
            return Some(SubagentStartOutcome::Duplicate);
        }
        let sp_pos = self
            .pending_starts
            .iter()
            .position(|sp| sp.child_agent_id == child_agent_id)?;
        // 先找可绑定 invocation(不先移除 pending——join 失败时 Start 仍需等待)。
        // FIFO 配对语义:工具调用顺序 = subagent 启动顺序(同步路径),跨 forwarder
        // 竞态只影响事件到达顺序,不影响 FIFO 相对顺序;因此只匹配同 parent 的
        // 最旧**未绑定** invocation(已绑定的跳过,防两个 Start 绑同一 invocation),
        // 无匹配返回 None(保持 pending,等后续 invocation 到达再 join)。
        let key = {
            let sp = &self.pending_starts[sp_pos];
            self.unbounded_invocations
                .iter()
                .find(|(p, c)| {
                    *p == sp.parent_agent_id
                        && self
                            .invocations
                            .get(&(p.clone(), c.clone()))
                            .is_some_and(|i| i.bound_child.is_none())
                })
                .cloned()
        };
        let Some(key) = key else {
            return None; // 无未绑定匹配:保持 pending,不跨 parent/不取已绑定项
        };
        // 找到后才正式移除 pending 与 unbounded 索引
        let sp = self.pending_starts.remove(sp_pos).unwrap();
        let mut inv = self.invocations.get_mut(&key)?.clone();
        inv.bound_child = Some(sp.child_agent_id.clone());
        inv.stop_received = self
            .by_agent_id
            .get(&sp.child_agent_id)
            .map(|sa| sa.stop.is_some())
            .unwrap_or(false);
        let input = Some(inv.input.clone());
        self.invocations.insert(key.clone(), inv);

        let obs = AgentObsStart {
            observation_id: format!("obs_{}", uuid::Uuid::now_v7()),
            parent_observation_id: self
                .invocations
                .get(&key)
                .map(|i| i.parent_stage_span_id.clone())
                .unwrap_or_default(),
            start_time: chrono::Utc::now().to_rfc3339(),
            agent_name: sp.agent_name.clone(),
            input: input.clone(),
        };
        let sa = self.by_agent_id.get_mut(&sp.child_agent_id).unwrap();
        sa.observation_id = obs.observation_id.clone();
        sa.parent_observation_id = obs.parent_observation_id.clone();
        sa.start_time = obs.start_time.clone();
        sa.agent_name = obs.agent_name.clone();
        sa.invocation_key = Some(key.clone());
        sa.input = input;
        let had_stop = sa.stop.is_some();
        sa.status = if had_stop {
            SubagentStatus::StopReceived
        } else {
            SubagentStatus::Active
        };

        // 取出该 child 的 gate 缓存事件(重放由 tracer 执行)
        let replayed = self.take_gated_events(&sp.child_agent_id);

        // Stop 已到且父 ToolEnded 已到 → 立即关闭
        let immediately_close = if had_stop {
            let inv = self.invocations.get(&key);
            if inv.map(|i| i.tool_ended).unwrap_or(false) {
                self.close_subagent(&sp.child_agent_id)
            } else {
                None
            }
        } else {
            None
        };

        tracing::info!(
            target: "langfuse::subagent",
            event = "subagent_joined",
            child_agent_id = %sp.child_agent_id,
            parent_agent_id = %sp.parent_agent_id,
            obs_id = %obs.observation_id,
            replayed = replayed.len(),
            "SubagentStart join 成功"
        );
        Some(SubagentStartOutcome::Joined {
            obs,
            replayed,
            immediately_close,
        })
    }

    /// 父 ToolEnded:结束 invocation(不关闭 AGENT obs)。两信号齐备(Stop 已到)
    /// 时回收:flush child tool_batch → Closed → 返回关闭信息。
    pub(crate) fn on_invocation_tool_end(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        output: &str,
        _is_error: bool,
    ) -> Option<ClosedSubagent> {
        let key = (agent_id.to_string(), tool_call_id.to_string());
        let inv = self.invocations.get_mut(&key)?;
        inv.tool_ended = true;
        inv.deferred_output = Some(output.to_string());
        if !inv.stop_received {
            return None; // 等 Stop 到达后再回收
        }
        let child = inv.bound_child.clone();
        let deferred = inv.deferred_output.clone();
        self.invocations.remove(&key);
        self.unbounded_invocations.retain(|k| k != &key);
        let child = child?;
        let sa = self.by_agent_id.get_mut(&child)?;
        if !matches!(
            sa.status,
            SubagentStatus::StopReceived | SubagentStatus::Active
        ) {
            return None;
        }
        let flush = std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush();
        let stop = sa.stop.take().unwrap_or_else(|| SubagentStopInfo {
            result: String::new(),
            is_error: true,
            stop_time: chrono::Utc::now().to_rfc3339(),
        });
        let output = if stop.result.is_empty() {
            deferred.unwrap_or_default()
        } else {
            stop.result.clone()
        };
        let closed = ClosedSubagent {
            observation_id: sa.observation_id.clone(),
            parent_observation_id: sa.parent_observation_id.clone(),
            start_time: sa.start_time.clone(),
            agent_name: sa.agent_name.clone(),
            input: sa.input.clone(),
            output,
            stop_time: stop.stop_time,
            is_error: stop.is_error,
            flush,
            incomplete_reason: None,
        };
        sa.status = SubagentStatus::Closed;
        Some(closed)
    }

    /// SubagentStop:按状态迁移(暂存/StopReceived/回收),返回关闭信息(如有)
    pub(crate) fn on_subagent_stop(
        &mut self,
        _parent_agent_id: &str,
        child_agent_id: &str,
        result: &str,
        is_error: bool,
    ) -> Option<ClosedSubagent> {
        let stop = SubagentStopInfo {
            result: result.to_string(),
            is_error,
            stop_time: chrono::Utc::now().to_rfc3339(),
        };
        let Some(sa) = self.by_agent_id.get_mut(child_agent_id) else {
            tracing::warn!(
                target: "langfuse::subagent",
                %child_agent_id,
                "SubagentStop 无对应 Start(丢失/乱序),标记 MissingStart"
            );
            self.incomplete_count += 1;
            return None;
        };
        match sa.status {
            SubagentStatus::PendingInvocation => {
                // Start 已入 pending、Stop 先到:暂存,等 join 后补 obs 生命周期
                sa.stop = Some(stop);
                None
            }
            SubagentStatus::Active => {
                let key = sa.invocation_key.clone();
                sa.stop = Some(stop);
                // 同步更新 invocation 的 stop_received(join 时的快照可能早于 Stop)
                if let Some(key) = &key {
                    if let Some(inv) = self.invocations.get_mut(key) {
                        inv.stop_received = true;
                    }
                }
                if let Some(key) = key {
                    if self
                        .invocations
                        .get(&key)
                        .map(|i| i.tool_ended)
                        .unwrap_or(false)
                    {
                        return self.close_subagent(child_agent_id);
                    }
                }
                sa.status = SubagentStatus::StopReceived;
                None
            }
            SubagentStatus::StopReceived => {
                tracing::warn!(
                    target: "langfuse::subagent",
                    %child_agent_id,
                    "SubagentStop 重复(已 StopReceived),标记 DuplicateStop"
                );
                self.mark_incomplete(child_agent_id, IncompleteReason::DuplicateStop);
                None
            }
            SubagentStatus::Closed | SubagentStatus::Incomplete(_) => {
                tracing::warn!(
                    target: "langfuse::subagent",
                    %child_agent_id,
                    "SubagentStop 到达但已关闭/终态,标记 DuplicateStop"
                );
                self.mark_incomplete(child_agent_id, IncompleteReason::DuplicateStop);
                None
            }
        }
    }

    /// 关闭 AGENT obs + 回收 invocation(两信号齐备或兜底时调用)
    fn close_subagent(&mut self, child_agent_id: &str) -> Option<ClosedSubagent> {
        let sa = self.by_agent_id.get_mut(child_agent_id)?;
        if !matches!(
            sa.status,
            SubagentStatus::Active | SubagentStatus::StopReceived
        ) {
            return None;
        }
        let flush = std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush();
        let (key, deferred) = match &sa.invocation_key {
            Some(k) => {
                let deferred = self
                    .invocations
                    .get(k)
                    .and_then(|i| i.deferred_output.clone());
                (Some(k.clone()), deferred)
            }
            None => (None, None),
        };
        let stop = sa.stop.take().unwrap_or_else(|| SubagentStopInfo {
            result: String::new(),
            is_error: true,
            stop_time: chrono::Utc::now().to_rfc3339(),
        });
        let output = if stop.result.is_empty() {
            deferred.unwrap_or_default()
        } else {
            stop.result.clone()
        };
        let closed = ClosedSubagent {
            observation_id: sa.observation_id.clone(),
            parent_observation_id: sa.parent_observation_id.clone(),
            start_time: sa.start_time.clone(),
            agent_name: sa.agent_name.clone(),
            input: sa.input.clone(),
            output,
            stop_time: stop.stop_time,
            is_error: stop.is_error,
            flush,
            incomplete_reason: None,
        };
        sa.status = SubagentStatus::Closed;
        if let Some(key) = key {
            self.invocations.remove(&key);
            self.unbounded_invocations.retain(|k| k != &key);
        }
        Some(closed)
    }

    // ── on_turn_end 兜底 ─────────────────────────────────────────────────────

    /// 清理未收 Stop 的活跃条目、pending Start、gate 缓存与残留 invocation。
    /// 返回兜底关闭的 AGENT obs(metadata 携带 incomplete_reason)。
    pub(crate) fn cleanup_turn_end(&mut self) -> Vec<ClosedSubagent> {
        let mut closed = Vec::new();
        // 1. pending Start 未 join → ParentLost(父 ToolStart 丢失)
        let pending: Vec<StartPending> = self.pending_starts.drain(..).collect();
        for sp in pending {
            self.mark_incomplete(&sp.child_agent_id, IncompleteReason::ParentLost);
        }
        // 2. gate 缓存残留(Start 从未到达)→ UnknownAgent(按 agent 去重计数)
        let remaining: std::collections::HashSet<String> =
            self.gate_cache.drain(..).map(|(agent, _)| agent).collect();
        for agent in remaining {
            self.mark_incomplete(&agent, IncompleteReason::UnknownAgent);
        }
        // 3. 活跃(Active/StopReceived)条目未收 Stop → 兜底关闭(MissingStop)
        let active: Vec<String> = self
            .by_agent_id
            .iter()
            .filter(|(_, sa)| {
                matches!(
                    sa.status,
                    SubagentStatus::Active | SubagentStatus::StopReceived
                )
            })
            .map(|(k, _)| k.clone())
            .collect();
        for agent in active {
            let flush = self
                .by_agent_id
                .get_mut(&agent)
                .map(|sa| std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush())
                .unwrap_or_else(|| ToolBatch::new().flush());
            let (key, deferred) = {
                let sa = self.by_agent_id.get(&agent).unwrap();
                let deferred = match &sa.invocation_key {
                    Some(k) => self
                        .invocations
                        .get(k)
                        .and_then(|i| i.deferred_output.clone()),
                    None => None,
                };
                (sa.invocation_key.clone(), deferred)
            };
            let sa = self.by_agent_id.get_mut(&agent).unwrap();
            let stop = sa.stop.take().unwrap_or_else(|| SubagentStopInfo {
                result: String::new(),
                is_error: true,
                stop_time: chrono::Utc::now().to_rfc3339(),
            });
            let output = if stop.result.is_empty() {
                deferred.unwrap_or_default()
            } else {
                stop.result.clone()
            };
            closed.push(ClosedSubagent {
                observation_id: sa.observation_id.clone(),
                parent_observation_id: sa.parent_observation_id.clone(),
                start_time: sa.start_time.clone(),
                agent_name: sa.agent_name.clone(),
                input: sa.input.clone(),
                output,
                stop_time: stop.stop_time,
                is_error: stop.is_error,
                flush,
                incomplete_reason: Some(IncompleteReason::MissingStop),
            });
            sa.status = SubagentStatus::Closed;
            if let Some(key) = key {
                self.invocations.remove(&key);
                self.unbounded_invocations.retain(|k| k != &key);
            }
        }
        // 4. 残留 invocation(Start 丢失)→ 清除
        if !self.invocations.is_empty() {
            tracing::warn!(
                target: "langfuse::subagent",
                left = self.invocations.len(),
                "on_turn_end:残留未绑定 invocation 清除(Start 丢失)"
            );
            self.invocations.clear();
            self.unbounded_invocations.clear();
        }
        closed
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
            self.by_agent_id.insert(
                child_agent_id.to_string(),
                ActiveSubagent {
                    observation_id: String::new(),
                    parent_observation_id: String::new(),
                    start_time: chrono::Utc::now().to_rfc3339(),
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
