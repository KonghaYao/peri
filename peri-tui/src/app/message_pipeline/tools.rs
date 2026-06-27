//! 工具调用状态追踪：tool_start/tool_end 处理。

use peri_agent::messages::ToolCallRequest;

use crate::ui::message_view::{instance_hash, parse_bg_hash, MessageViewModel};

use super::{state::PendingTool, BatchInfo, SubAgentState};

impl super::MessagePipeline {
    /// 工具调用开始（内部版本，只更新状态，不返回 PipelineAction）
    pub(crate) fn tool_start_internal(
        &mut self,
        tool_call_id: &str,
        name: &str,
        input: serde_json::Value,
        is_background: bool,
    ) {
        self.finalize_current_ai();
        let partial = self.partial_mut();
        partial
            .tool_calls
            .push(ToolCallRequest::new(tool_call_id, name, input.clone()));

        if name == "Agent" {
            let agent_id = input["subagent_type"]
                .as_str()
                .unwrap_or("Agent")
                .to_string();
            let task_preview: String = input["prompt"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            self.subagent_stack.push(SubAgentState {
                agent_id: agent_id.clone(),
                instance_id: tool_call_id.to_string(),
                task_preview: task_preview.clone(),
                total_steps: 0,
                recent_messages: Vec::new(),
                is_running: true,
                finalized_vm: None,
                is_background,
                bg_hash: Some(instance_hash(tool_call_id)),
            });
            // 批次检测：第一个 agent 创建批次，后续递增
            if let Some(ref mut batch) = self.active_batch {
                batch.started += 1;
            } else {
                self.active_batch = Some(BatchInfo {
                    started: 1,
                    completed: 0,
                });
            }
        } else {
            // 非 Agent 工具打断批次连续性
            self.active_batch = None;
        }

        let partial = self.partial_mut();
        partial.pending_tools.insert(
            tool_call_id.to_string(),
            PendingTool {
                tool_call_id: tool_call_id.to_string(),
                name: name.to_string(),
                input,
            },
        );
    }

    /// 工具调用结束（内部版本，只更新状态，不返回 PipelineAction）
    pub(crate) fn tool_end_internal(
        &mut self,
        tool_call_id: &str,
        name: &str,
        output: &str,
        is_error: bool,
    ) {
        let (pending_input_opt, is_agent) = {
            let partial = match self.partial.as_mut() {
                Some(p) => p,
                None => return, // 无 partial 时无 pending_tools 可清理
            };
            let pending = partial.pending_tools.remove(tool_call_id);
            let input = pending
                .as_ref()
                .map(|p| p.input.clone())
                .unwrap_or(serde_json::Value::Null);
            (input, name == "Agent")
        };

        if is_agent {
            // tool_call_id 现在就是 instance_id，直接精确匹配
            if let Some(sub) = self
                .subagent_stack
                .iter_mut()
                .find(|s| s.instance_id == tool_call_id && s.is_running)
            {
                if sub.is_background {
                    // 后台 agent 路径：不冻结，保持 is_running=true，解析 bg_hash
                    sub.bg_hash = parse_bg_hash(output);
                    // 保持 is_running=true，等待 BackgroundTaskCompleted 到达
                    // 显式确保 is_running=true（防止其他逻辑意外修改）
                    sub.is_running = true;
                } else {
                    // 前台 agent 路径：冻结 SubAgentGroup
                    sub.is_running = false;
                    let mut vm = MessageViewModel::SubAgentGroup {
                        agent_id: sub.agent_id.clone(),
                        task_preview: sub.task_preview.clone(),
                        total_steps: sub.total_steps,
                        recent_messages: std::mem::take(&mut sub.recent_messages),
                        is_running: false,
                        collapsed: false,
                        final_result: Some(output.to_string()),
                        is_error,
                        is_background: false,
                        bg_hash: sub.bg_hash.clone(),
                        batch_agents: Vec::new(),
                        instance_id: Some(sub.instance_id.clone()),
                        content_hash: 0,
                    };
                    vm.recompute_hash();
                    sub.finalized_vm = Some(vm.clone());
                    // 立即冻结：RebuildAll 可能在下一个 TurnCommitted 前触发
                    self.frozen_subagent_vms.push(vm);
                }
            }
            // 批次检测：递增完成计数
            if let Some(ref mut batch) = self.active_batch {
                batch.completed += 1;
            }
        } else {
            // 非 SubAgent 工具：保存到 partial.completed_tools，在 TurnCommitted 到达前显示
            use super::state::CompletedTool;
            let partial = self.partial_mut();
            partial.completed_tools.push(CompletedTool {
                tool_call_id: tool_call_id.to_string(),
                name: name.to_string(),
                input: pending_input_opt,
                output: output.to_string(),
                is_error,
            });
        }
    }
}
