//! 父工具 invocation 与子 agent Start 的 FIFO 关联及 parent 冻结。

use super::super::tool_batch::ToolBatch;
use super::{
    ActiveSubagent, AgentObsStart, IncompleteReason, StartPending, SubagentInvocation,
    SubagentRegistry, SubagentStartOutcome, SubagentStatus,
};

impl SubagentRegistry {
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
        let now = chrono::Utc::now().to_rfc3339();
        self.by_agent_id.insert(
            child_agent_id.to_string(),
            ActiveSubagent {
                observation_id: String::new(),
                observation_closed: false,
                parent_observation_id: String::new(),
                start_time: now.clone(),
                last_content_time: now,
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
    pub(super) fn try_join(&mut self, child_agent_id: &str) -> Option<SubagentStartOutcome> {
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
        sa.last_content_time = obs.start_time.clone();
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
}
