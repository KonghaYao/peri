use super::super::tool_card::{SubAgentAccumulator, ToolCardAccumulator};
use super::{CurrentTurn, TurnSegment};

impl CurrentTurn {
    /// Begin a new sub-agent group from `"subagent-started"`.
    ///
    /// Flushes any pending text before the sub-agent boundary.
    pub fn start_subagent(&mut self, agent_id: String, agent_name: String) {
        // Duplicate Start for the same live occurrence is idempotent. A resume,
        // however, reuses child_thread_id after the previous occurrence stopped;
        // it must create a fresh group and claim the new Agent ToolCard.
        if self
            .subagents
            .iter()
            .any(|s| s.agent_id == agent_id && s.is_running)
        {
            return;
        }
        self.flush_text_segment();
        let idx = self.subagents.len();

        // 前向扫描找第一个未 claim 的 Agent ToolCard，在其后插入 SubAgent 段。
        // 防止多 Agent 同 turn 时 SubAgent 段全部 append 到末尾导致
        // "agent agent tools tools" 而非 "agent tools agent tools"。
        let mut insert_at: Option<(usize, usize)> = None; // (seg_pos, tool_idx)
        for (i, seg) in self.segments.iter().enumerate() {
            if let TurnSegment::Tool { tool_idx } = seg
                && let Some(tc) = self.tool_cards.get(*tool_idx)
                && tc.tool_name == "Agent"
                && !tc.claimed_by_subagent
            {
                insert_at = Some((i + 1, *tool_idx));
                break;
            }
        }

        if let Some((seg_pos, tool_idx)) = insert_at {
            self.tool_cards[tool_idx].claimed_by_subagent = true;
            self.segments
                .insert(seg_pos, TurnSegment::SubAgent { subagent_idx: idx });
            // 段列表中部插入会破坏 segment↔cache 的索引对齐——清空缓存整体重建。
            // 该操作低频（每 subagent 一次），O(total) 成本可接受。
            self.cached_view_models = im::Vector::new();
        } else {
            self.segments
                .push(TurnSegment::SubAgent { subagent_idx: idx });
        }

        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// [诊断] 返回当前所有 SubAgentAccumulator 的 agent_id 列表。
    pub fn subagent_ids(&self) -> Vec<&str> {
        self.subagents.iter().map(|s| s.agent_id.as_str()).collect()
    }

    /// Mark a sub-agent group as done from `"subagent-stopped"`.
    ///
    /// `is_error` 是 parent 终态的唯一事实源（agent 层语义：Completed→false、
    /// Interrupted/Error→true）；`result` 仅在 genuine error 且非空白（trim
    /// 后非空）时保存为可见原因（`error_reason`），completed parent 即使有
    /// 失败 child tool 也不携带 parent error。保存的是原始未 trim 的 result
    /// （空白仅用于判缺，不修改展示文本）。
    pub fn stop_subagent(&mut self, agent_id: &str, is_error: bool, result: &str) {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.is_running = false;
            s.is_error = is_error;
            s.error_reason = (is_error && !result.trim().is_empty()).then(|| result.to_string());
            // [§6.7] 冻结子 turn 的 trailing 流式段——子 turn 不经过快照折叠
            // pass，不冻结则 trailing bubble 保持 Running 形态（started_at
            // 存活、elapsed 持续增长），详情面板对已完成 subagent 渲染永久的
            // `◐ Thinking… Ns`。
            s.child_turn.freeze_trailing();
            // 子 turn 必须同时 deactivate：ToolStarted 无 ToolEnded 直接停止时，
            // active 残留 true 会让 build_tool_card 以 `turn_active` 把无
            // output_summary 的工具卡保持 Running（is_running = active && 无输出）。
            s.child_turn.deactivate();
            s.cached_view_model.replace(None);
            self.invalidate_cache();
        }
    }

    /// Route text chunks into a sub-agent child message.
    pub fn append_subagent_text(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.append_text(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route reasoning chunks into a sub-agent child message.
    pub fn append_subagent_reasoning(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.append_reasoning(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route tool start into a sub-agent child message.
    pub fn start_subagent_tool(&mut self, agent_id: &str, tool: ToolCardAccumulator) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.start_tool(tool);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            // [诊断] 路由失败时记录所有已注册的 agent_id
            let registered: Vec<&str> =
                self.subagents.iter().map(|s| s.agent_id.as_str()).collect();
            tracing::debug!(
                agent_id = %agent_id,
                registered = ?registered,
                "start_subagent_tool: agent_id not found in registered SubAgentAccumulators"
            );
            false
        }
    }

    /// Route tool end into a sub-agent child message.
    pub fn end_subagent_tool(
        &mut self,
        agent_id: &str,
        tool_id: &str,
        output: String,
        is_error: bool,
    ) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            let ended = s.end_tool(tool_id, output, is_error);
            if ended {
                self.active = true;
                self.invalidate_cache();
            }
            ended
        } else {
            false
        }
    }
}
