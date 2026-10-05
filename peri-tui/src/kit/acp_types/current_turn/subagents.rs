use super::super::tool_card::{SubAgentAccumulator, ToolCardAccumulator};
use super::{CurrentTurn, TurnSegment};

/// 工具名是否为子 Agent 启动调用（Agent 工具卡片与 Subagent 分组配对的唯一判据）。
///
/// 配对有两个方向：卡片先到（`start_subagent` 前向扫描认领）与分组先到
/// （`start_tool` 用新卡片接管孤儿分组）——两处必须用同一判据。
pub(super) fn is_agent_launcher_tool(tool_name: &str) -> bool {
    tool_name == "Agent"
}

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
                && is_agent_launcher_tool(&tc.tool_name)
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
            // 分组先于父 Agent 卡片到达（并行多 Agent 批次里非首个工具调用的
            // ToolStarted 只在 dispatch 阶段发出，与子 Agent 直发的
            // SubagentStarted 竞争）：不能就此 append 到末尾——那样分组会挂在
            // 上一个仍在 loading 的 Agent 调用之下。先记账，等迟到的 Agent
            // 卡片出现时由 `start_tool` 接管（见 adopt_orphan_subagent_group）。
            self.orphan_subagent_groups.push(idx);
            self.segments
                .push(TurnSegment::SubAgent { subagent_idx: idx });
        }

        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// Agent ToolCard 晚于子分组到达时的接管：把最早创建的孤儿分组段移动到
    /// 该卡片之后，恢复 "Agent 调用紧接自己的子工具行" 的时序。
    ///
    /// 按到达顺序 FIFO 配对（第 k 张卡片 ↔ 第 k 个分组），与 `start_subagent`
    /// 前向扫描（最早未 claim 卡片）保持同一配对口径——事件乱序到达时无法恢复
    /// 真实身份配对，只能保证段位置不再挂到别的 Agent 调用之下。
    pub(super) fn adopt_orphan_subagent_group(&mut self, tool_idx: usize, tool_seg_pos: usize) {
        let Some(subagent_idx) = self.orphan_subagent_groups.first().copied() else {
            return;
        };
        self.orphan_subagent_groups.remove(0);
        let Some(seg_pos) = self.segments.iter().position(
            |seg| matches!(seg, TurnSegment::SubAgent { subagent_idx: si } if *si == subagent_idx),
        ) else {
            // 段已不存在（turn 重置/提交）：丢弃记账，保持不变量。
            return;
        };
        self.segments.remove(seg_pos);
        // 被移动的段在卡片之前时，移除后卡片位置左移一格，目标槽位随之左移。
        let insert_at = if seg_pos < tool_seg_pos {
            tool_seg_pos
        } else {
            tool_seg_pos + 1
        };
        self.segments
            .insert(insert_at, TurnSegment::SubAgent { subagent_idx });
        self.tool_cards[tool_idx].claimed_by_subagent = true;
        // 段列表重排会破坏 segment↔cache 的索引对齐——清空缓存整体重建。
        self.cached_view_models = im::Vector::new();
    }

    /// [诊断] 返回当前所有 SubAgentAccumulator 的 agent_id 列表。
    pub fn subagent_ids(&self) -> Vec<&str> {
        self.subagents.iter().map(|s| s.agent_id.as_str()).collect()
    }

    /// 该 agent 最新 occurrence 的 TUI 本地 instance id。
    ///
    /// 供后台 subagent 的 live 明细记录运行身份：`agent_id` 在 resume 时被复用，
    /// 只有 instance 能把选中组与它所属的那次运行对应起来。
    pub fn subagent_instance_id(&self, agent_id: &str) -> Option<&str> {
        self.subagents
            .iter()
            .rev()
            .find(|s| s.agent_id == agent_id)
            .map(|s| s.instance_id.as_str())
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
