use super::MessagePipeline;
pub use crate::ui::message_view::aggregate_batch_groups;
use crate::{
    app::tool_display,
    ui::{
        message_view::{aggregate_tool_groups, tool_color, ContentBlockView, MessageViewModel},
        theme,
    },
};

// ─── 管线事件 ────────────────────────────────────────────────────────────────

/// 管线处理事件后的输出动作
#[derive(Debug)]
pub enum PipelineAction {
    /// 无 UI 变化
    None,
    /// 新增消息（外部通知 + 用户消息）
    AddMessage(MessageViewModel),
    /// 尾部重建（prefix_len 标记不变前缀长度，tail_vms 存储重建尾部）
    RebuildAll {
        prefix_len: usize,
        tail_vms: Vec<MessageViewModel>,
    },
}

/// 合并冻结的 SubAgentGroup VM 到 reconcile 重建后的新 VMs 中，防止 Done 后 SubAgent 显示退化。
///
/// `frozen_vms` 是 SubAgentEnd 时构建的完整 SubAgentGroup VM（含 recent_messages、final_result 等），
/// 按 `agent_id` 匹配替换新 VMs 中的 SubAgentGroup 占位符。
///
/// 匹配策略：优先用 `instance_id`（如果两边都有值）精确匹配；
/// 回退到 `agent_id` 匹配（reconcile VM 的 instance_id 为 None 时的兼容路径）。
/// 对于同一 `agent_id` 的多个 VM，使用位置匹配保证一一对应。
///
/// 返回未匹配的冻结 VM 索引集合（供调用方决定是否追加到 tail_vms）。
pub(crate) fn merge_frozen_subagents(
    frozen_vms: &[MessageViewModel],
    new_vms: &mut [MessageViewModel],
) -> Vec<usize> {
    if frozen_vms.is_empty() {
        return Vec::new();
    }

    // 收集 reconcile 中 SubAgentGroup 的索引
    let new_subagent_indices: Vec<usize> = new_vms
        .iter()
        .enumerate()
        .filter(|(_, vm)| vm.is_subagent_group())
        .map(|(i, _)| i)
        .collect();

    let mut matched_frozen = vec![false; frozen_vms.len()];

    // 第一轮：用 instance_id 精确匹配（frozen 有 instance_id，reconcile 可能有也可能没有）
    for (fi, frozen_vm) in frozen_vms.iter().enumerate() {
        if matched_frozen[fi] {
            continue;
        }
        if let MessageViewModel::SubAgentGroup {
            instance_id: Some(frozen_iid),
            ..
        } = frozen_vm
        {
            // 尝试在 new_vms 中找到 instance_id 匹配的 SubAgentGroup
            for &ni in &new_subagent_indices {
                if let MessageViewModel::SubAgentGroup {
                    instance_id: Some(new_iid),
                    ..
                } = &new_vms[ni]
                {
                    if frozen_iid == new_iid {
                        new_vms[ni] = frozen_vm.clone();
                        matched_frozen[fi] = true;
                        break;
                    }
                }
            }
        }
    }

    // 第二轮：用 agent_id + 位置匹配（reconcile VM 的 instance_id 为 None）
    for (fi, frozen_vm) in frozen_vms.iter().enumerate() {
        if matched_frozen[fi] {
            continue;
        }
        if let MessageViewModel::SubAgentGroup {
            agent_id: frozen_aid,
            ..
        } = frozen_vm
        {
            for &ni in &new_subagent_indices {
                if let MessageViewModel::SubAgentGroup {
                    agent_id: new_aid, ..
                } = &new_vms[ni]
                {
                    if frozen_aid == new_aid {
                        new_vms[ni] = frozen_vm.clone();
                        matched_frozen[fi] = true;
                        break;
                    }
                }
            }
        }
    }

    // 返回未匹配的冻结 VM 索引
    matched_frozen
        .iter()
        .enumerate()
        .filter(|(_, &m)| !m)
        .map(|(i, _)| i)
        .collect()
}

impl MessagePipeline {
    /// 构建 RebuildAll action（用于外部 agent_ops 显式触发重建）。
    /// 由调用者提供 prefix_len（round_start_vm_idx），pipeline 内部不维护 VM 索引。
    pub fn build_rebuild_all(&self, prefix_len: usize) -> PipelineAction {
        let tail_vms = self.build_tail_vms();
        PipelineAction::RebuildAll {
            prefix_len,
            tail_vms,
        }
    }

    /// 从 pipeline 规范状态（transcript + partial）构建尾部 VMs。
    ///
    /// 单一数据源架构：transcript 是规范历史，partial 是当前迭代增量。
    /// - 若 `has_committed_this_round == true`：本轮已收到过 TurnCommitted，
    ///   transcript 含本轮全部已提交消息；从 `transcript_len_at_round_start` 切片
    ///   reconcile。partial（若有）追加为 streaming bubble + pending tools。
    /// - 若 `has_committed_this_round == false`（首轮提交前）：跳过 transcript 切片，
    ///   只输出 partial（streaming + pending tools）。
    pub(crate) fn build_tail_vms(&self) -> Vec<MessageViewModel> {
        let mut tail_vms = Vec::new();

        if self.has_committed_this_round {
            let start = self
                .transcript_len_at_round_start
                .min(self.transcript.len());
            // 直接从 round 起点渲染全部已提交消息。
            //
            // 不需要 rposition(Human) 找截断点：
            // - 正常会话：start 指向用户提交前的位置，transcript[start..] 就是本轮全部已提交消息
            // - compact 后：restore_completed 已将 start 设到 compact 消息之后，
            //   transcript[start..] 只含新会话消息
            // - goal steering 注入的 Human 消息也在 transcript[start..] 内，不会被跳过
            //
            // 之前的 rposition(Human) 会跳到注入的 <goal-message>，导致 AI 回复丢失。
            tail_vms = Self::messages_to_view_models(&self.transcript[start..], &self.cwd);
        }

        // 当前迭代 partial：流式 AssistantBubble + 工具调用
        // 关键架构改进：partial 只含当前迭代的状态，不会跨迭代累积——
        // 每次迭代结束时 commit_iteration 整体清空 partial。
        // 因此 partial 的内容始终位于 transcript 中已提交消息之后，
        // 自然保持时序正确（修复 v2 文本渲染在工具调用之前的 bug）。
        if let Some(partial) = self.partial.as_ref() {
            // 流式 AssistantBubble（当前迭代正在输出的文本/推理）
            if partial.has_streaming_content() {
                tail_vms.push(self.build_streaming_bubble());
            }

            // 当前迭代的工具调用：按 tool_calls 时间线顺序迭代
            use std::collections::HashSet;
            let mut completed_ids: HashSet<String> =
                HashSet::with_capacity(partial.completed_tools.len());
            for tc in &partial.tool_calls {
                if let Some(pending) = partial.pending_tools.get(&tc.id) {
                    if pending.name != "Agent" {
                        tail_vms.push(self.build_tool_start_vm(
                            &tc.id,
                            &pending.name,
                            &pending.input,
                        ));
                    }
                    continue;
                }
                // 工具已结束但 TurnCommitted 尚未到达：从 completed_tools 查找结果
                if let Some(ct) = partial
                    .completed_tools
                    .iter()
                    .find(|ct| ct.tool_call_id == tc.id)
                {
                    let display = tool_display::format_tool_name(&ct.name);
                    let args = tool_display::format_tool_args(&ct.name, &ct.input, Some(&self.cwd));
                    let diff_lines = if !ct.is_error {
                        crate::ui::message_view::build_diff_lines(&ct.name, &ct.input)
                    } else {
                        None
                    };
                    let auto_expand = tool_display::should_auto_expand_tool(&ct.name, ct.is_error);
                    let mut vm = MessageViewModel::ToolBlock {
                        tool_name: ct.name.clone(),
                        tool_call_id: ct.tool_call_id.clone(),
                        display_name: display,
                        args_display: args,
                        content: ct.output.clone(),
                        is_error: ct.is_error,
                        collapsed: !auto_expand,
                        color: if ct.is_error {
                            theme::ERROR
                        } else {
                            tool_color(&ct.name)
                        },
                        diff_lines,
                        content_hash: 0,
                    };
                    vm.recompute_hash();
                    tail_vms.push(vm);
                    completed_ids.insert(ct.tool_call_id.clone());
                }
            }

            // 防御性追加：completed_tools 中不在 tool_calls 的残余条目
            // （理论上 commit_iteration 会清空 partial，此处保留兜底）
            for ct in &partial.completed_tools {
                if completed_ids.contains(&ct.tool_call_id) {
                    continue;
                }
                let display = tool_display::format_tool_name(&ct.name);
                let args = tool_display::format_tool_args(&ct.name, &ct.input, Some(&self.cwd));
                let diff_lines = if !ct.is_error {
                    crate::ui::message_view::build_diff_lines(&ct.name, &ct.input)
                } else {
                    None
                };
                let auto_expand = tool_display::should_auto_expand_tool(&ct.name, ct.is_error);
                let mut vm = MessageViewModel::ToolBlock {
                    tool_name: ct.name.clone(),
                    tool_call_id: ct.tool_call_id.clone(),
                    display_name: display,
                    args_display: args,
                    content: ct.output.clone(),
                    is_error: ct.is_error,
                    collapsed: !auto_expand,
                    color: if ct.is_error {
                        theme::ERROR
                    } else {
                        tool_color(&ct.name)
                    },
                    diff_lines,
                    content_hash: 0,
                };
                vm.recompute_hash();
                tail_vms.push(vm);
            }
        }

        // SubAgentGroup VMs
        if self.has_committed_this_round {
            let unmatched = merge_frozen_subagents(&self.frozen_subagent_vms, &mut tail_vms);
            // 将未匹配的冻结 VM（reconcile 中没有对应 SubAgentGroup 的后台 agent）
            // 直接追加到 tail_vms，防止后台 agent 从视图中消失。
            for idx in unmatched {
                if let Some(frozen) = self.frozen_subagent_vms.get(idx) {
                    tail_vms.push(frozen.clone());
                }
            }
            for sub in &self.subagent_stack {
                if sub.finalized_vm.is_none() {
                    let mut vm = MessageViewModel::SubAgentGroup {
                        agent_id: sub.agent_id.clone(),
                        task_preview: sub.task_preview.clone(),
                        total_steps: sub.total_steps,
                        recent_messages: sub.recent_messages.clone(),
                        is_running: sub.is_running,
                        collapsed: false,
                        final_result: None,
                        is_error: false,
                        is_background: sub.is_background,
                        bg_hash: sub.bg_hash.clone(),
                        batch_agents: Vec::new(),
                        instance_id: Some(sub.instance_id.clone()),
                        content_hash: 0,
                    };
                    vm.recompute_hash();
                    tail_vms.push(vm);
                }
            }
        } else {
            for sub in &self.subagent_stack {
                let vm = if let Some(ref finalized) = sub.finalized_vm {
                    finalized.clone()
                } else {
                    let mut vm = MessageViewModel::SubAgentGroup {
                        agent_id: sub.agent_id.clone(),
                        task_preview: sub.task_preview.clone(),
                        total_steps: sub.total_steps,
                        recent_messages: sub.recent_messages.clone(),
                        is_running: sub.is_running,
                        collapsed: false,
                        final_result: None,
                        is_error: false,
                        is_background: sub.is_background,
                        bg_hash: sub.bg_hash.clone(),
                        batch_agents: Vec::new(),
                        instance_id: Some(sub.instance_id.clone()),
                        content_hash: 0,
                    };
                    vm.recompute_hash();
                    vm
                };
                tail_vms.push(vm);
            }
        }

        aggregate_tool_groups(&mut tail_vms);

        let has_partial_activity = self
            .partial
            .as_ref()
            .is_some_and(|p| p.has_streaming_content() || !p.tool_calls.is_empty());
        if !has_partial_activity {
            aggregate_batch_groups(&mut tail_vms);
        }

        add_thinking_tail_snapshot(&mut tail_vms);

        tail_vms
    }
}

/// 提取文本的最后 `n` 行（按换行符切分，单行不截断）。
/// 返回换行分隔的字符串。
pub(crate) fn extract_tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// 扫描 tail_vms 的最后一个 AssistantBubble，
/// 若满足条件（无 Text block + 最后一个 block 是 Reasoning）则设置 tail_lines。
fn add_thinking_tail_snapshot(tail_vms: &mut [MessageViewModel]) {
    for vm in tail_vms.iter_mut().rev() {
        if let MessageViewModel::AssistantBubble { blocks, .. } = vm {
            let has_text = blocks
                .iter()
                .any(|b| matches!(b, ContentBlockView::Text { raw, .. } if !raw.trim().is_empty()));
            if has_text {
                return;
            }
            if let Some(ContentBlockView::Reasoning {
                text, tail_lines, ..
            }) = blocks.last_mut()
            {
                let tail = extract_tail_lines(text, 3);
                if !tail.is_empty() {
                    *tail_lines = Some(tail);
                }
            }
            return;
        }
    }
}
