use super::super::tool_card::{SubAgentAccumulator, ToolCardAccumulator};
use super::{CurrentTurn, PendingSubagentGroup, TurnSegment};

/// 工具名是否为子 Agent 启动调用（Agent 工具卡片与 Subagent 分组配对的唯一判据）。
///
/// 配对判定分三层：身份优先（`parent_tool_call_id` == 卡片 `tool_id`，两个方向
/// 共用）、无身份时前向扫描（`start_subagent` 认领最早未 claim 卡片）、以及
/// 分组先到时的待配对表（`start_tool` 认领，仅身份匹配或无身份分组）。
pub(super) fn is_agent_launcher_tool(tool_name: &str) -> bool {
    tool_name == "Agent"
}

impl CurrentTurn {
    /// Begin a new sub-agent group from `"subagent-started"`.
    ///
    /// Flushes any pending text before the sub-agent boundary.
    ///
    /// 配对优先级：
    /// 1. `parent_tool_call_id` 有值 → 按身份找该 Agent 工具卡片（并发批次下事件
    ///    到达顺序不可判定，身份是唯一可靠依据）；卡片尚未到达时记入待配对表，
    ///    由 `start_tool` 在卡片出现时按同一身份认领。
    /// 2. 无父身份（旧生产端 / `/bg` 等无工具调用上下文）→ 前向扫描第一个未
    ///    claim 的 Agent 卡片（到达顺序兜底，语义同修复前）。
    ///
    /// 两条路都找不到落点时，段先记在尾部并进入待配对表，等卡片到达再迁移。
    pub fn start_subagent(
        &mut self,
        agent_id: String,
        agent_name: String,
        parent_tool_call_id: Option<String>,
    ) {
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

        // (seg_pos, tool_idx)：找到配对卡片时在其后插入 SubAgent 段，防止多 Agent
        // 同 turn 时 SubAgent 段全部 append 到末尾导致 "agent agent tools tools"。
        let insert_at = match parent_tool_call_id.as_deref() {
            Some(parent_id) => self.agent_card_segment_for(parent_id),
            None => self.first_unclaimed_agent_card_segment(),
        };

        if let Some((seg_pos, tool_idx)) = insert_at {
            self.tool_cards[tool_idx].claimed_by_subagent = true;
            self.segments
                .insert(seg_pos, TurnSegment::SubAgent { subagent_idx: idx });
            // 段列表中部插入会破坏 segment↔cache 的索引对齐——清空缓存整体重建。
            // 该操作低频（每 subagent 一次），O(total) 成本可接受。
            self.cached_view_models = im::Vector::new();
        } else {
            // 卡片尚未到达（并行多 Agent 批次里非首个工具调用的 ToolStarted 只
            // 在 dispatch 阶段发出，与子 Agent 直发的 SubagentStarted 竞争）：
            // 不能就此 append 到末尾——那样分组会挂在别的（仍 loading 的）Agent
            // 调用之下。先记账，等卡片到达时由 `start_tool` 认领
            // （见 adopt_pending_subagent_group）。
            self.pending_subagent_groups.push(PendingSubagentGroup {
                subagent_idx: idx,
                parent_tool_call_id,
            });
            self.segments
                .push(TurnSegment::SubAgent { subagent_idx: idx });
        }

        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// 父工具调用 id 对应的 Agent 卡片段位置 `(seg_pos, tool_idx)`。
    fn agent_card_segment_for(&self, parent_tool_call_id: &str) -> Option<(usize, usize)> {
        self.segments.iter().enumerate().find_map(|(i, seg)| {
            let TurnSegment::Tool { tool_idx } = seg else {
                return None;
            };
            let card = self.tool_cards.get(*tool_idx)?;
            (card.tool_id == parent_tool_call_id).then_some((i + 1, *tool_idx))
        })
    }

    /// 第一个未 claim 的 Agent 卡片段位置（无父身份时的到达顺序兜底）。
    fn first_unclaimed_agent_card_segment(&self) -> Option<(usize, usize)> {
        self.segments.iter().enumerate().find_map(|(i, seg)| {
            let TurnSegment::Tool { tool_idx } = seg else {
                return None;
            };
            let card = self.tool_cards.get(*tool_idx)?;
            (is_agent_launcher_tool(&card.tool_name) && !card.claimed_by_subagent)
                .then_some((i + 1, *tool_idx))
        })
    }

    /// Agent ToolCard 晚于子分组到达时的接管：把待配对分组段移动到该卡片之后，
    /// 恢复 "Agent 调用紧接自己的子工具行" 的时序。
    ///
    /// 优先按身份配对（分组记录的 `parent_tool_call_id` == 卡片 `tool_id`）；
    /// 仅当卡片没有身份匹配、且存在无父身份的待配对分组（旧生产端 / 无工具上下文
    /// 路径）时，才按到达顺序 FIFO 兜底——有身份的分组绝不按顺序猜。
    pub(super) fn adopt_pending_subagent_group(&mut self, tool_idx: usize, tool_seg_pos: usize) {
        let tool_id = self.tool_cards[tool_idx].tool_id.clone();
        let matched = self
            .pending_subagent_groups
            .iter()
            .position(|pending| pending.parent_tool_call_id.as_deref() == Some(tool_id.as_str()))
            .or_else(|| {
                self.pending_subagent_groups
                    .iter()
                    .position(|pending| pending.parent_tool_call_id.is_none())
            });
        let Some(pos) = matched else {
            return;
        };
        let subagent_idx = self.pending_subagent_groups.remove(pos).subagent_idx;
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
