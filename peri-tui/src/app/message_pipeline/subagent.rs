//! SubAgent 执行状态管理及后台 agent 同步。

use crate::{app::tool_display, ui::message_view::MessageViewModel};

use super::SubAgentState;

impl super::MessagePipeline {
    /// SubAgent 内部工具调用（路由进指定 SubAgentGroup）
    pub(crate) fn push_tool_start_to_subagent(
        sub: &mut SubAgentState,
        tool_call_id: &str,
        name: &str,
        input: &serde_json::Value,
        cwd: &str,
    ) {
        let display = tool_display::format_tool_name(name);
        let args = tool_display::format_tool_args(name, input, Some(cwd));
        let vm = MessageViewModel::tool_block_with_id(
            tool_call_id.to_string(),
            name.to_string(),
            display,
            args,
            false,
        );
        sub.total_steps += 1;
        if sub.recent_messages.len() >= 4 {
            sub.recent_messages.remove(0);
        }
        sub.recent_messages.push(vm);
    }

    /// SubAgent 内部 chunk（路由进指定 SubAgentGroup）
    pub(crate) fn push_chunk_to_subagent(sub: &mut SubAgentState, chunk: &str) {
        match sub.recent_messages.last_mut() {
            Some(m) if m.is_assistant() => m.append_chunk(chunk),
            _ => {
                sub.total_steps += 1;
                if sub.recent_messages.len() >= 4 {
                    sub.recent_messages.remove(0);
                }
                let mut bubble = MessageViewModel::assistant();
                bubble.append_chunk(chunk);
                sub.recent_messages.push(bubble);
            }
        }
    }

    /// SubAgent 内部 ToolEnd 更新（路由进指定 SubAgentGroup）
    pub(crate) fn update_tool_end_in_subagent(
        sub: &mut SubAgentState,
        tool_call_id: &str,
        output: &str,
        is_error: bool,
    ) {
        for vm in sub.recent_messages.iter_mut().rev() {
            if let MessageViewModel::ToolBlock {
                tool_call_id: tc_id,
                content,
                is_error: err,
                ..
            } = vm
            {
                if tc_id == tool_call_id {
                    *content = output.to_string();
                    *err = is_error;
                    vm.recompute_hash();
                    break;
                }
            }
        }
    }

    /// 根据 instance_id 查找 subagent_stack 中正在运行的 SubAgent
    pub(crate) fn find_running_subagent_mut(
        &mut self,
        instance_id: &str,
    ) -> Option<&mut SubAgentState> {
        self.subagent_stack
            .iter_mut()
            .find(|s| s.instance_id == instance_id && s.is_running)
    }

    /// 清理 subagent_stack：只推入**未**在 tool_end_internal 中 freeze 的残留条目。
    ///
    /// `tool_end_internal` 在 SubAgentEnd 时已将 finalized_vm 推入 frozen_subagent_vms，
    /// 这里只处理异常情况（SubAgent 未正常结束，如被 Interrupted/Error 打断时仍在运行）。
    /// 已 finalized 的条目不重复推入，避免 frozen 列表膨胀导致 merge_frozen_subagents 错位。
    pub(crate) fn drain_subagent_stack(&mut self) {
        for sub in self.subagent_stack.drain(..) {
            if sub.finalized_vm.is_none() && !sub.is_running {
                // 未 finalized 但已停止：异常残留，构建一个基本 VM 保留显示
                let mut vm = MessageViewModel::SubAgentGroup {
                    agent_id: sub.agent_id,
                    task_preview: sub.task_preview,
                    total_steps: sub.total_steps,
                    recent_messages: sub.recent_messages,
                    is_running: false,
                    collapsed: false,
                    final_result: None,
                    is_error: false,
                    is_background: sub.is_background,
                    bg_hash: sub.bg_hash,
                    batch_agents: Vec::new(),
                    instance_id: Some(sub.instance_id),
                    content_hash: 0,
                };
                vm.recompute_hash();
                self.frozen_subagent_vms.push(vm);
            } else if sub.finalized_vm.is_none() && sub.is_running && sub.is_background {
                // 后台 agent 仍在运行：冻结以保留当前 recent_messages，
                // 后续 BackgroundTaskCompleted 会直接更新 view_messages
                let mut vm = MessageViewModel::SubAgentGroup {
                    agent_id: sub.agent_id,
                    task_preview: sub.task_preview,
                    total_steps: sub.total_steps,
                    recent_messages: sub.recent_messages,
                    is_running: true,
                    collapsed: false,
                    final_result: None,
                    is_error: false,
                    is_background: true,
                    bg_hash: sub.bg_hash,
                    batch_agents: Vec::new(),
                    instance_id: Some(sub.instance_id),
                    content_hash: 0,
                };
                vm.recompute_hash();
                self.frozen_subagent_vms.push(vm);
            }
            // 已 finalized（finalized_vm.is_some()）的不推入——tool_end_internal 已处理
            // 仍在运行的前台 agent（is_running && !is_background）不推入
        }
    }

    /// BackgroundTaskCompleted 到达后，同步更新管线状态。
    ///
    /// 更新 subagent_stack 中匹配的后台 SubAgentState（标记 is_running=false、
    /// push finalized VM 到 frozen_subagent_vms），同时更新 frozen_subagent_vms
    /// 中已冻结但未完成的 SubAgentGroup VM（Done 先于 BG Complete 到达的情况）。
    ///
    pub fn notify_bg_completed(
        &mut self,
        instance_id: Option<&str>,
        agent_name: &str,
        output: &str,
        success: bool,
        steps: usize,
    ) {
        // 1. 更新 subagent_stack 中仍在运行的匹配 SubAgentState
        //    优先按 instance_id 精确匹配，回退到 agent_name
        let sub_pos = instance_id
            .and_then(|iid| {
                self.subagent_stack
                    .iter()
                    .position(|s| s.instance_id == iid && s.is_running && s.is_background)
            })
            .or_else(|| {
                self.subagent_stack
                    .iter()
                    .position(|s| s.agent_id == agent_name && s.is_running && s.is_background)
            });

        if let Some(pos) = sub_pos {
            let sub = &mut self.subagent_stack[pos];
            sub.is_running = false;
            // 仿照前台 agent 的 tool_end_internal 路径：
            // 创建 finalized VM 并推入 frozen_subagent_vms，标记 finalized_vm
            // 防止 drain_subagent_stack 重复创建。
            let mut vm = MessageViewModel::SubAgentGroup {
                agent_id: sub.agent_id.clone(),
                task_preview: sub.task_preview.clone(),
                total_steps: steps,
                recent_messages: std::mem::take(&mut sub.recent_messages),
                is_running: false,
                collapsed: false,
                final_result: Some(output.to_string()),
                is_error: !success,
                is_background: true,
                bg_hash: sub.bg_hash.clone(),
                batch_agents: Vec::new(),
                instance_id: Some(sub.instance_id.clone()),
                content_hash: 0,
            };
            vm.recompute_hash();
            sub.finalized_vm = Some(vm.clone());
            self.frozen_subagent_vms.push(vm);
        }

        // 2. 更新 frozen_subagent_vms 中已冻结但 is_running=true 的 VM
        //    （Done → drain_subagent_stack 先于 BG Complete 的情况）
        //    两遍匹配：优先 instance_id 精确匹配，回退 agent_name
        if let Some(iid) = instance_id {
            for vm in &mut self.frozen_subagent_vms {
                match vm {
                    MessageViewModel::SubAgentGroup {
                        instance_id: Some(vm_iid),
                        is_running,
                        is_background,
                        final_result,
                        is_error,
                        total_steps,
                        ..
                    } if *is_running && *is_background && vm_iid == iid => {
                        *is_running = false;
                        *final_result = Some(output.to_string());
                        *is_error = !success;
                        *total_steps = steps;
                        vm.recompute_hash();
                        return;
                    }
                    _ => {}
                }
            }
        }
        // 兜底：按 agent_name 匹配
        for vm in &mut self.frozen_subagent_vms {
            match vm {
                MessageViewModel::SubAgentGroup {
                    agent_id,
                    is_running,
                    is_background,
                    final_result,
                    is_error,
                    total_steps,
                    ..
                } if *is_running && *is_background && agent_id == agent_name => {
                    *is_running = false;
                    *final_result = Some(output.to_string());
                    *is_error = !success;
                    *total_steps = steps;
                    vm.recompute_hash();
                    break;
                }
                _ => {}
            }
        }
    }
}
