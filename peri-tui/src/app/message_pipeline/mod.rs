//! 统一消息渲染管线 (Unified Message Rendering Pipeline)
//!
//! 核心设计：所有 `MessageViewModel` 的产生都经过单一转换函数
//! `messages_to_view_models(base_messages, cwd)`。
//!
//! # 单一数据源架构（v3 重构）
//!
//! Pipeline 持有两类状态：
//! - **规范源 `transcript: Vec<BaseMessage>`**：会话级权威消息历史，由 v2 stages
//!   在每个 ReAct 迭代边界（`TurnCommitted` 事件）整体替换。
//! - **当前迭代增量 `partial: Option<PartialAiMessage>`**：流式 LLM 输出与
//!   工具调用状态，懒初始化于第一个流式事件，在 `commit_iteration` 整体丢弃。
//!
//! 视图派生是纯函数：`view = messages_to_view_models(transcript) ⊕ partial_bubble`。
//! 历史恢复与流式渲染走同一路径（`restore_completed` 等价于一次 `commit_iteration`）。
//!
//! # 两条路径统一
//!
//! ```text
//!   流式事件 ──→ partial（增量）     ─┐
//!                                       ├─→ build_tail_vms ──→ MessageViewModel[]
//!   TurnCommitted ──→ transcript（全量）─┘
//!   历史恢复 ──→ restore_completed ──→ transcript = msgs, partial = None
//! ```

use std::time::Instant;

use peri_agent::messages::{BaseMessage, ToolCallRequest};

use crate::app::events::AgentEvent;
use crate::ui::message_view::MessageViewModel;

mod lifecycle;
mod reconcile;
mod state;
mod streaming;
mod subagent;
mod throttle;
mod tools;
mod transform;

pub use reconcile::PipelineAction;
#[cfg(test)]
use reconcile::{extract_tail_lines, merge_frozen_subagents};

#[allow(unused_imports)]
pub(crate) use throttle::{AdaptiveChunkingPolicy, ChunkingMode, DrainPlan};

#[allow(unused_imports)]
pub(crate) use state::CompletedTool;
pub(crate) use state::{PartialAiMessage, PendingTool};
pub(crate) use streaming::StreamingMode;

pub use crate::ui::message_view::aggregate_batch_groups;

// ─── 管线内部状态（SubAgent / Batch） ────────────────────────────────────────

/// 活跃 SubAgent 执行状态
pub(crate) struct SubAgentState {
    /// subagent_type，仅用于显示
    pub agent_id: String,
    /// 唯一实例标识符，用于路由
    pub instance_id: String,
    pub task_preview: String,
    pub total_steps: usize,
    /// 流式期间的内部消息（不持久化）
    pub recent_messages: Vec<MessageViewModel>,
    pub is_running: bool,
    /// SubAgentEnd 时固化的完整 VM（含 recent_messages、final_result 等）
    pub finalized_vm: Option<MessageViewModel>,
    /// 是否为后台 agent
    pub is_background: bool,
    /// Agent 实例的短显示标识符（6 位十六进制）
    pub bg_hash: Option<String>,
}

/// 批次检测状态：跟踪连续的 SubAgentStart/SubAgentEnd
pub(crate) struct BatchInfo {
    /// 已开始的 agent 数
    pub started: usize,
    /// 已完成的 agent 数
    pub completed: usize,
}

// ─── MessagePipeline ─────────────────────────────────────────────────────────

/// 统一消息渲染管线。
///
/// 维护规范 `transcript: Vec<BaseMessage>`（全量会话历史）与
/// `partial: Option<PartialAiMessage>`（当前迭代增量）两类状态，
/// 通过单一转换函数 `messages_to_view_models()` 产生 `MessageViewModel`。
pub struct MessagePipeline {
    cwd: String,
    /// 规范状态：会话级权威 BaseMessage 列表，由 `commit_iteration` 整体替换。
    transcript: Vec<BaseMessage>,
    /// 当前 ReAct 迭代进行中的增量（流式文本/推理/工具调用）。
    /// `None` = 无进行中迭代；`Some(..)` = 流式事件已懒初始化。
    partial: Option<PartialAiMessage>,
    /// SubAgent 栈
    subagent_stack: Vec<SubAgentState>,
    /// 冻结的 SubAgentGroup VMs（SubAgentEnd 时构建，done() 时收集）
    frozen_subagent_vms: Vec<MessageViewModel>,
    /// 批次检测状态（连续的 SubAgentStart/SubAgentEnd 跟踪）
    active_batch: Option<BatchInfo>,
    // ── 节流状态 ──
    /// 自适应分块策略（替代固定 100ms 节流）
    adaptive_policy: AdaptiveChunkingPolicy,
    /// 上次节流发射的时间（Smooth 模式下的最小间隔守卫）
    throttle_last_fire: Option<Instant>,
    // ── 流式渲染模式 ──
    /// 当前流式渲染模式
    streaming_mode: StreamingMode,
    // ── Block 模式缓冲 ──
    /// Block 模式下累积未完成 block 的 chunk
    block_buffer: String,
    /// Block 模式下是否处于代码围栏内部
    inside_code_fence: bool,
    /// Block 模式下是否有待 flush 的内容
    block_pending_flush: bool,
    // ── 轮次追踪 ──
    /// 本轮开始时 transcript 的长度（用于派生 prefix_len）
    transcript_len_at_round_start: usize,
    /// 本轮是否已收到过 TurnCommitted / set_completed 提交信号
    has_committed_this_round: bool,
}

impl MessagePipeline {
    pub fn new(cwd: String) -> Self {
        Self {
            cwd,
            transcript: Vec::new(),
            partial: None,
            subagent_stack: Vec::new(),
            frozen_subagent_vms: Vec::new(),
            active_batch: None,
            adaptive_policy: AdaptiveChunkingPolicy::new(),
            throttle_last_fire: None,
            streaming_mode: StreamingMode::default(),
            block_buffer: String::new(),
            inside_code_fence: false,
            block_pending_flush: false,
            transcript_len_at_round_start: 0,
            has_committed_this_round: false,
        }
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 获取或初始化 partial（流式事件到达时调用）。
    pub(crate) fn partial_mut(&mut self) -> &mut PartialAiMessage {
        self.partial.get_or_insert_with(Default::default)
    }

    /// 不可变访问 partial（若存在）。
    #[cfg(test)]
    pub(crate) fn partial_ref(&self) -> Option<&PartialAiMessage> {
        self.partial.as_ref()
    }

    // ── 流式状态访问（测试兼容层） ────────────────────────────────────────────
    // 生产代码通过 `partial.as_ref()/as_mut()` 直接访问，这些方法仅为旧测试保留。

    /// 当前迭代流式文本（无 partial 时返回空字符串切片）
    #[cfg(test)]
    pub(crate) fn current_ai_text(&self) -> &str {
        self.partial.as_ref().map(|p| p.text.as_str()).unwrap_or("")
    }

    /// 当前迭代的 tool_calls
    #[cfg(test)]
    pub(crate) fn current_ai_tool_calls(&self) -> &[ToolCallRequest] {
        self.partial
            .as_ref()
            .map(|p| p.tool_calls.as_slice())
            .unwrap_or(&[])
    }

    // ── 测试友好访问（partial 内部状态） ─────────────────────────────────────

    /// 当前迭代 pending_tools 是否包含指定 tool_call_id（无 partial 时返回 false）
    #[cfg(test)]
    pub(crate) fn pending_tools_contains(&self, id: &str) -> bool {
        self.partial
            .as_ref()
            .is_some_and(|p| p.pending_tools.contains_key(id))
    }

    /// 当前迭代 completed_tools 是否为空（无 partial 时视为空）
    #[cfg(test)]
    pub(crate) fn completed_tools_is_empty(&self) -> bool {
        self.partial
            .as_ref()
            .map(|p| p.completed_tools.is_empty())
            .unwrap_or(true)
    }

    /// 统一事件处理入口：将 AgentEvent 转换为 PipelineAction 列表。
    /// 所有事件只更新 pipeline 内部状态，返回 None。
    /// RebuildAll 由 agent_ops 通过 `check_throttle()` 或 `build_rebuild_all()` 显式触发。
    pub fn handle_event(&mut self, event: AgentEvent) -> Vec<PipelineAction> {
        match event {
            AgentEvent::AssistantChunk {
                chunk,
                source_agent_id,
            } => {
                if !chunk.is_empty() {
                    if let Some(ref aid) = source_agent_id {
                        if let Some(sub) = self.find_running_subagent_mut(aid) {
                            Self::push_chunk_to_subagent(sub, &chunk);
                            self.adaptive_policy.on_chunk(&chunk);
                        }
                    } else if self.in_subagent() {
                        // 顺序执行时 last() 就是当前 subagent（事件顺序到达）
                        if let Some(sub) = self.subagent_stack.last_mut() {
                            Self::push_chunk_to_subagent(sub, &chunk);
                            self.adaptive_policy.on_chunk(&chunk);
                        }
                    } else {
                        self.push_chunk(&chunk);
                        // push_chunk 内部已调用 adaptive_policy.on_chunk()
                    }
                }
                vec![PipelineAction::None]
            }
            AgentEvent::AiReasoning(text) => {
                if self.in_subagent() {
                    // SubAgent 内部推理：更新 subagent 状态，通知策略
                    if let Some(_sub) = self.subagent_stack.last_mut() {
                        self.adaptive_policy.on_reasoning_chunk();
                    }
                } else {
                    self.push_reasoning(&text);
                    // push_reasoning 内部已调用 adaptive_policy.on_reasoning_chunk()
                }
                vec![PipelineAction::None]
            }
            AgentEvent::ToolStart {
                tool_call_id,
                name,
                display: _,
                args: _,
                input,
                source_agent_id,
            } => {
                // 仅解除 throttle，不在此处触发 RebuildAll。
                // agent_ops 中的 request_rebuild() 会以正确的 prefix_len
                // (= round_start_vm_idx) 触发重建，同时包含流式文本和工具调用。
                // 之前此处使用 prefix_len: 0 会导致 view_messages 被全部替换，
                // 随后 request_rebuild() 用旧的 round_start_vm_idx 做 drain 时 panic。
                self.adaptive_policy.drain();
                self.force_flush_block();

                if let Some(ref aid) = source_agent_id {
                    let cwd = self.cwd.clone();
                    if let Some(sub) = self.find_running_subagent_mut(aid) {
                        Self::push_tool_start_to_subagent(sub, &tool_call_id, &name, &input, &cwd);
                    }
                } else if self.in_subagent() {
                    // 顺序执行时 last() 就是当前 subagent
                    let cwd = self.cwd.clone();
                    if let Some(sub) = self.subagent_stack.last_mut() {
                        Self::push_tool_start_to_subagent(sub, &tool_call_id, &name, &input, &cwd);
                    }
                } else if name == "Agent" {
                    // 父 Agent 调用 Agent 工具：只注册 tool_call 和 pending_tool，
                    // 不创建 SubAgentState（SubAgentStart 事件会处理）。
                    // 避免与 SubAgentStart 的 tool_start_internal 产生重复条目。
                    self.finalize_current_ai();
                    let partial = self.partial_mut();
                    partial.tool_calls.push(ToolCallRequest::new(
                        &tool_call_id,
                        &name,
                        input.clone(),
                    ));
                    partial.pending_tools.insert(
                        tool_call_id.to_string(),
                        PendingTool {
                            tool_call_id: tool_call_id.to_string(),
                            name: name.to_string(),
                            input,
                        },
                    );
                } else {
                    self.tool_start_internal(&tool_call_id, &name, input, false);
                }

                vec![PipelineAction::None]
            }
            AgentEvent::ToolEnd {
                tool_call_id,
                name,
                output,
                is_error,
                source_agent_id,
            } => {
                self.adaptive_policy.drain();
                self.force_flush_block();
                if let Some(ref aid) = source_agent_id {
                    if let Some(sub) = self.find_running_subagent_mut(aid) {
                        Self::update_tool_end_in_subagent(sub, &tool_call_id, &output, is_error);
                    }
                } else if self.in_subagent() {
                    // 顺序执行时 last() 就是当前 subagent
                    if let Some(sub) = self.subagent_stack.last_mut() {
                        Self::update_tool_end_in_subagent(sub, &tool_call_id, &output, is_error);
                    }
                } else {
                    self.tool_end_internal(&tool_call_id, &name, &output, is_error);
                }
                vec![PipelineAction::None]
            }
            AgentEvent::SubAgentStart {
                agent_id,
                instance_id,
                task_preview,
                is_background,
            } => {
                let input =
                    serde_json::json!({"subagent_type": &agent_id, "prompt": &task_preview});
                self.tool_start_internal(&instance_id, "Agent", input, is_background);
                vec![PipelineAction::None]
            }
            AgentEvent::SubAgentEnd {
                result,
                is_error,
                agent_id: _,
                instance_id,
            } => {
                let tc_id = if let Some(ref iid) = instance_id {
                    // 按 instance_id 精确查找 RUNNING 的 SubAgent
                    self.subagent_stack
                        .iter()
                        .find(|s| s.instance_id == *iid && s.is_running)
                        .map(|s| s.instance_id.clone())
                        .unwrap_or_else(|| "subagent_end".to_string())
                } else {
                    // 防御性回退：instance_id=None 仅当旧版事件到达
                    self.subagent_stack
                        .last()
                        .map(|s| s.instance_id.clone())
                        .unwrap_or_else(|| "subagent_end".to_string())
                };
                self.tool_end_internal(&tc_id, "Agent", &result, is_error);
                vec![PipelineAction::None]
            }
            AgentEvent::Done => {
                if self.in_subagent() {
                    // Child agent done during tool execution — ignore to avoid
                    // finalizing parent state or corrupting the subagent_stack.
                    vec![PipelineAction::None]
                } else {
                    self.done();
                    vec![PipelineAction::None]
                }
            }
            AgentEvent::Interrupted => {
                if self.in_subagent() {
                    // Child agent interrupted — ignore; parent tool call will
                    // handle the result (including interruption) when it returns.
                    vec![PipelineAction::None]
                } else {
                    self.interrupt();
                    vec![PipelineAction::None]
                }
            }
            AgentEvent::TurnCommitted { messages, .. } => {
                if self.in_subagent() {
                    // 子 Agent 的 TurnCommitted 不应修改父 Agent 的 transcript，
                    // 否则子 Agent 的全部内部消息会污染父 Agent 的消息历史。
                    vec![PipelineAction::None]
                } else {
                    self.force_flush_block();
                    self.commit_iteration(messages);
                    vec![PipelineAction::None]
                }
            }
            AgentEvent::StateSnapshot(msgs) => {
                // v1 残留路径（v2 不再 emit）。保留以兼容旧 executor 路径与持久化场景。
                if self.in_subagent() {
                    vec![PipelineAction::None]
                } else {
                    self.force_flush_block();
                    self.commit_iteration(msgs);
                    vec![PipelineAction::None]
                }
            }
            AgentEvent::SubagentLifecycle { .. } => {
                // SubagentLifecycle 仅由 agent_ops 处理（spinner + request_rebuild），
                // Pipeline 不修改状态，直接返回 None
                vec![PipelineAction::None]
            }
            AgentEvent::StateSnapshotMeta { .. } => {
                // v2 轻量级元数据快照：不携带消息列表，Pipeline 不修改 transcript 状态。
                // 元数据（context 使用率/步数）由 agent_ops 自行消费（如未来需要）。
                vec![PipelineAction::None]
            }
            // 以下事件由 agent_ops 直接处理，Pipeline 返回 None
            AgentEvent::Error(_)
            | AgentEvent::InteractionRequest { .. }
            | AgentEvent::TodoUpdate(_)
            | AgentEvent::CompactStarted
            | AgentEvent::CompactCompleted { .. }
            | AgentEvent::CompactError(_)
            | AgentEvent::RewindCompleted { .. }
            | AgentEvent::TokenUsageUpdate { .. }
            | AgentEvent::LlmRetrying { .. }
            | AgentEvent::ContextWarning { .. }
            | AgentEvent::OAuthAuthorizationNeeded { .. }
            | AgentEvent::OAuthAuthorizationCompleted { .. }
            | AgentEvent::OAuthAuthorizationFailed { .. }
            | AgentEvent::BackgroundTaskCompleted { .. }
            | AgentEvent::McpActionCompleted { .. }
            | AgentEvent::PluginActionCompleted { .. }
            | AgentEvent::LspDiagnostics { .. }
            | AgentEvent::BgToolStep { .. }
            | AgentEvent::WorkflowProgress(_) => {
                vec![PipelineAction::None]
            }
        }
    }
}

#[cfg(test)]
#[path = "message_pipeline_test.rs"]
mod tests;
