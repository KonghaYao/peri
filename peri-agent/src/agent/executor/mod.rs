mod final_answer;
mod llm_step;
mod tool_dispatch;
mod tool_setup;

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
pub use tokio_util::sync::CancellationToken as AgentCancellationToken;
use tracing::instrument;

use crate::{
    agent::{
        events::{AgentEvent, AgentEventHandler, BackgroundTaskResult},
        react::{AgentInput, AgentOutput, ReactLLM, ToolCall, ToolResult},
        state::State,
    },
    error::{AgentError, AgentResult},
    messages::{message::MessageId, BaseMessage},
    middleware::{chain::MiddlewareChain, r#trait::Middleware},
    tools::BaseTool,
};

#[allow(clippy::type_complexity)]
/// Agent 执行器 - 管理 ReAct 循环
pub struct ReActAgent<L, S>
where
    L: ReactLLM,
    S: State,
{
    pub(crate) llm: L,
    pub(crate) tools: HashMap<String, Box<dyn BaseTool>>,
    pub(crate) chain: MiddlewareChain<S>,
    pub(crate) max_iterations: usize,
    /// 可选事件回调：在工具调用、答案生成等关键节点触发
    pub(crate) event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// 固定系统提示词：在所有中间件 before_agent 执行完毕后 prepend，无顺序约束
    pub(crate) system_prompt: Option<String>,
    /// 上下文窗口预算配置（用于监控 token 用量和触发 compact 建议）
    pub(crate) context_budget: Option<crate::agent::token::ContextBudget>,
    /// 后台任务通知接收端：后台 agent 完成时推送结果
    pub notification_rx:
        Option<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<BackgroundTaskResult>>>,
    /// 工具过滤器：返回 true 的工具从 LLM 可见列表中移除（None = 不过滤，向后兼容）
    pub(crate) tool_filter: Option<fn(&str) -> bool>,
    /// 共享工具注册表：包含所有工具（包括 deferred），供 ExecuteExtraTool 代理执行使用
    pub(crate) shared_tools: Option<Arc<parking_lot::RwLock<HashMap<String, Arc<dyn BaseTool>>>>>,
    /// micro_compact 配置（None = 不在循环内自动压缩）
    pub(crate) compact_config: Option<crate::agent::compact::CompactConfig>,
}

impl<L: ReactLLM, S: State> ReActAgent<L, S> {
    pub fn new(llm: L) -> Self {
        Self {
            llm,
            tools: HashMap::new(),
            chain: MiddlewareChain::new(),
            max_iterations: 10,
            event_handler: None,
            system_prompt: None,
            context_budget: None,
            notification_rx: None,
            tool_filter: None,
            shared_tools: None,
            compact_config: None,
        }
    }

    pub fn max_iterations(mut self, n: usize) -> Self {
        self.max_iterations = n;
        self
    }

    pub fn register_tool(mut self, tool: Box<dyn BaseTool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    pub fn add_middleware(mut self, middleware: Box<dyn Middleware<S>>) -> Self {
        self.chain.add(middleware);
        self
    }

    /// 注入事件回调（链式 builder）
    pub fn with_event_handler(mut self, handler: Arc<dyn AgentEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    /// 设置固定系统提示词
    ///
    /// 在所有中间件 `before_agent` 执行完毕之后、LLM 循环开始之前，
    /// 将 system 消息 prepend 到 state 消息列表最前。
    /// 不依赖中间件注册顺序，可在 builder 链任意位置调用。
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// 设置上下文窗口预算配置
    ///
    /// 用于监控 token 用量：当 context 使用率超过 `warning_threshold` 时发出日志警告，
    /// 提示用户使用 `/compact` 压缩上下文。设置为 None 则禁用监控。
    pub fn with_context_budget(mut self, budget: crate::agent::token::ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    /// 设置后台任务通知接收端
    ///
    /// 后台 agent 完成时通过此通道推送 `BackgroundTaskResult`，
    /// 主 agent 在 ReAct 循环中消费通知并注入到消息流。
    pub fn with_notification_rx(
        mut self,
        rx: tokio::sync::mpsc::UnboundedReceiver<BackgroundTaskResult>,
    ) -> Self {
        self.notification_rx = Some(tokio::sync::Mutex::new(rx));
        self
    }

    /// 设置工具过滤器
    ///
    /// 返回 `true` 的工具从 LLM 可见列表中移除（延迟加载），
    /// 返回 `false` 或 `None` 时保留所有工具（向后兼容）。
    pub fn with_tool_filter(mut self, filter: fn(&str) -> bool) -> Self {
        self.tool_filter = Some(filter);
        self
    }

    /// 设置共享工具注册表
    ///
    /// 包含所有工具（包括 deferred tools），供 ExecuteExtraTool 代理执行使用。
    /// executor 在工具收集完成后将所有工具写入此注册表。
    pub fn with_shared_tools(
        mut self,
        tools: Arc<parking_lot::RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    ) -> Self {
        self.shared_tools = Some(tools);
        self
    }

    /// 设置 micro_compact 配置
    ///
    /// 启用后，ReAct 循环在每次工具调用完成后检查上下文用量，
    /// 超过 warning 阈值时自动执行 micro_compact（压缩旧工具结果）。
    pub fn with_compact_config(mut self, config: crate::agent::compact::CompactConfig) -> Self {
        self.compact_config = Some(config);
        self
    }

    /// 更新事件回调（用于同一 agent 实例的新轮次）
    pub fn set_event_handler(&mut self, handler: Arc<dyn AgentEventHandler>) {
        self.event_handler = Some(handler);
    }

    /// 更新系统提示词（用于同一 agent 实例的新轮次）
    ///
    /// **[TRAP]** 系统提示词应在 `session/new` 时一次性冻结（第一优先级不变量）。
    /// 多次调用会触发 warning + debug_assert，防止误用破坏 prompt cache。
    /// 不返回 `Result` 以保持调用方签名简洁（误用非致命，但有调试断言）。
    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        if self.system_prompt.is_some() {
            tracing::warn!(
                "set_system_prompt called more than once — system prompt should be frozen at session/new"
            );
            debug_assert!(
                false,
                "set_system_prompt called more than once — violates prompt stability invariant"
            );
        }
        self.system_prompt = Some(prompt.into());
    }

    /// 更新后台任务通知接收端（用于同一 agent 实例的新轮次）
    pub fn set_notification_rx(
        &mut self,
        rx: tokio::sync::mpsc::UnboundedReceiver<BackgroundTaskResult>,
    ) {
        self.notification_rx = Some(tokio::sync::Mutex::new(rx));
    }

    pub fn middleware_names(&self) -> Vec<&str> {
        self.chain.names()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 发出事件（无 handler 时静默忽略）
    pub(crate) fn emit(&self, event: AgentEvent) {
        if let Some(h) = &self.event_handler {
            h.on_event(event);
        }
    }

    /// 执行 Agent（ReAct 循环主入口）
    ///
    /// `cancel` 可选；触发后：
    /// - LLM 请求进行中 → 立即返回 `AgentError::Interrupted`
    /// - 工具执行进行中 → 所有未完成工具以 error 结果写入状态，然后返回 `AgentError::Interrupted`
    #[instrument(name = "agent.execute", skip(self, input, state, cancel),
        fields(max_iterations = self.max_iterations))]
    pub async fn execute(
        &self,
        input: AgentInput,
        state: &mut S,
        cancel: Option<CancellationToken>,
    ) -> AgentResult<AgentOutput> {
        // 若未提供 token，创建一个永不触发的占位符，简化后续逻辑
        let cancel = cancel.unwrap_or_default();

        let human_msg = BaseMessage::human(input.content);
        // snapshot_anchor 指向 Human 消息之前的最后一条消息（通常是上一轮的 AI 消息或 System 消息）。
        // index_after_id 返回 anchor 之后的位置，即 Human 消息的索引，确保 StateSnapshot 包含 Human 消息。
        // 空 state 时生成随机 sentinel ID，index_after_id 找不到时 fallback 到 0，从 Human 消息开始。
        let sentinel = MessageId::new(); // 不会匹配任何已有消息
        let pre_human_anchor = state.messages().last().map(|m| m.id());
        state.add_message(human_msg.clone());
        self.emit(AgentEvent::MessageAdded(human_msg));
        let mut snapshot_anchor: MessageId = pre_human_anchor.unwrap_or(sentinel);

        // 从中间件收集工具，手动注册的同名工具优先级最高
        let middleware_tools = self.chain.collect_tools(state.cwd());

        // 将所有工具转为 Arc 并收集
        let tool_arcs: Vec<Arc<dyn BaseTool>> = middleware_tools
            .into_iter()
            .map(self::tool_setup::box_to_arc)
            .collect();

        // 将所有工具写入共享注册表（供 ExecuteExtraTool 代理执行使用）
        // 每轮 clear 防止已断开的 MCP/SubAgent 工具永久残留（P0-2）
        if let Some(ref shared) = self.shared_tools {
            let mut map = shared.write();
            map.clear();
            for arc in &tool_arcs {
                map.insert(arc.name().to_string(), Arc::clone(arc));
            }
        }

        // 构建引用 map（用于 executor 内部工具查找）
        let mut all_tools: HashMap<String, &dyn BaseTool> = HashMap::new();
        for arc in &tool_arcs {
            all_tools.insert(arc.name().to_string(), arc.as_ref());
        }
        for (name, tool) in &self.tools {
            all_tools.insert(name.clone(), tool.as_ref());
        }

        let mut tool_refs: Vec<&dyn BaseTool> = if let Some(filter) = self.tool_filter {
            all_tools
                .values()
                .copied()
                .filter(|t| !filter(t.name()))
                .collect()
        } else {
            all_tools.values().copied().collect()
        };
        tool_refs.sort_by_key(|t| t.name());

        tracing::debug!(
            total_tools = all_tools.len(),
            middleware_tools = tool_arcs.len(),
            registered_tools = self.tools.len(),
            visible_tools = tool_refs.len(),
            tool_names = ?tool_refs.iter().map(|t| t.name()).collect::<Vec<_>>(),
            has_filter = self.tool_filter.is_some(),
            "agent: final tool set after collect"
        );

        self.chain.run_before_agent(state).await?;

        // 固定 system prompt：在所有中间件 before_agent 之后 prepend，无顺序约束
        if let Some(ref prompt) = self.system_prompt {
            state.prepend_message(BaseMessage::system(prompt.clone()));
        }

        // 收集所有 prepend 的消息 ID 用于 execute 结束时清理。
        // before_agent 中的中间件通过 prepend_message(insert(0)) 注入 System 消息，
        // 它们全部集中在 messages 头部、连续排列、均为 System 类型。
        // before_agent 中的 add_message(push) 注入的消息在尾部（如 SkillPreload 的
        // Ai[ToolUse]+Tool[ToolResult]），不属于 prepend，不应被 cleanup。
        //
        // 用 take_while(System) 收集头部连续 System 消息是安全的，因为：
        // 1. 所有 prepend 都只插入 System 消息
        // 2. 原始消息的头部不应有 System（compact 注入的 System 用 add_message 追加到尾部）
        // 3. SkillPreload 的 add_message(Ai/Tool) 不在头部
        let prepended_ids: Vec<MessageId> = state
            .messages()
            .iter()
            .take_while(|m| m.is_system())
            .map(|m| m.id())
            .collect();

        // try_break! — 替换 ? 传播，将错误捕获到 loop_error 中并在循环后统一处理。
        // 这确保 cleanup_prepended 无论成功/失败/循环耗尽都会执行，
        // 防止 before_agent + with_system_prompt 注入的 system 消息泄漏到 state。
        macro_rules! try_break {
            ($expr:expr, $err:ident) => {
                match $expr {
                    Ok(v) => v,
                    Err(e) => {
                        $err = Some(e);
                        break;
                    }
                }
            };
        }

        let mut all_tool_calls: Vec<(ToolCall, ToolResult)> = Vec::new();
        let mut final_result: Option<AgentOutput> = None;
        let mut consecutive_failures: HashMap<String, usize> = HashMap::new();
        let mut loop_error: Option<AgentError> = None;

        for step in 0..self.max_iterations {
            state.set_current_step(step);

            // 钩子: before_model — LLM 调用前（compact 检查点）
            state.set_current_step(step);

            try_break!(self.chain.run_before_model(state).await, loop_error);

            let reasoning = try_break!(
                self::llm_step::call_llm(self, state, &tool_refs, step, &cancel).await,
                loop_error
            );

            try_break!(
                self.chain.run_after_model(state, &reasoning).await,
                loop_error
            );

            if reasoning.needs_tool_call() {
                // 工具分发
                let step_calls = try_break!(
                    self::tool_dispatch::dispatch_tools(
                        self,
                        state,
                        &reasoning,
                        &all_tools,
                        &cancel,
                        &mut consecutive_failures,
                    )
                    .await,
                    loop_error
                );
                all_tool_calls.extend(step_calls);

                // StateSnapshot + 通知消费
                self::final_answer::emit_snapshot_and_drain_notifications(
                    self,
                    state,
                    &mut snapshot_anchor,
                )
                .await;

                // compact 已由 CompactMiddleware（before_model 钩子）在 call_llm 前处理
            } else {
                // 最终回答（clone all_tool_calls 避免移动，MaxIterationsExceeded 路径仍需借用）
                let output = try_break!(
                    self::final_answer::handle_final_answer(
                        self,
                        state,
                        &reasoning,
                        all_tool_calls.clone(),
                        &mut snapshot_anchor,
                        step,
                    )
                    .await,
                    loop_error
                );
                if let Some(reason) = &output.block_continue {
                    tracing::info!(
                        reason = %reason,
                        step = step + 1,
                        "Stop hook block_continue: resuming agent loop"
                    );
                    // 发出快照 + 消费通知后再继续循环
                    self::final_answer::emit_snapshot_and_drain_notifications(
                        self,
                        state,
                        &mut snapshot_anchor,
                    )
                    .await;
                    continue;
                }
                final_result = Some(output);
                break;
            }
        }

        // 清理临时 prepend 的 system 消息（before_agent + with_system_prompt 注入）
        // compact 可能已替换所有消息（此时 prepended_ids 中的 ID 不存在，retain 无操作）
        // 未发生 compact 时，移除 prepend 的 system 消息防止累积到 history。
        // try_break! 确保无论循环正常退出、break（最终回答）、还是 break（错误捕获），
        // cleanup 始终执行。
        Self::cleanup_prepended(state, &prepended_ids);

        // 传播循环内 try_break! 捕获的错误
        if let Some(e) = loop_error {
            return Err(e);
        }

        if let Some(output) = final_result {
            return Ok(output);
        }

        // MaxIterationsExceeded 路径：循环自然耗尽，all_tool_calls 未被 move
        {
            // 安全网快照：仅覆盖 MaxIterationsExceeded 路径（循环自然耗尽）。
            let safety_start =
                self::final_answer::index_after_id(state.messages(), snapshot_anchor);
            let safety_msgs: Vec<BaseMessage> = state.messages()[safety_start..]
                .iter()
                .filter(|m| !m.is_system())
                .cloned()
                .collect();
            if !safety_msgs.is_empty() {
                self.emit(AgentEvent::StateSnapshot(safety_msgs));
            }

            tracing::warn!(
                max_iterations = self.max_iterations,
                tool_call_count = all_tool_calls.len(),
                last_tools = ?all_tool_calls.iter().rev().take(3)
                    .map(|(_, r)| r.tool_name.as_str())
                    .collect::<Vec<_>>(),
                "ReAct 循环达到最大迭代次数"
            );
            crate::metrics::emit(
                "threshold.llm_calls_exceeded",
                serde_json::json!({
                    "count": self.max_iterations,
                    "limit": self.max_iterations,
                }),
                state.get_context("session_id"),
                state.get_context("run_id"),
            );
            return Err(AgentError::MaxIterationsExceeded(self.max_iterations));
        }
    }

    /// 移除 execute() 开头通过 before_agent + with_system_prompt prepend 的临时 system 消息。
    /// compact 发生时这些 ID 已不存在于 state 中，retain 无操作。
    ///
    /// [TRAP] 额外检查 `m.is_system()` 是防御性加固：prepended_ids 来自 take_while(System)，
    /// 仅头部 System 消息进入此集合（mod.rs:275）。当前实现依赖 UUIDv7 唯一性保证
    /// retain 不会误删非 prepended 消息。增加 role 检查可防止未来若引入确定性/测试 ID
    /// 时的潜在误删——SC#5 加固。
    fn cleanup_prepended(state: &mut S, ids: &[MessageId]) {
        if ids.is_empty() {
            return;
        }
        state
            .messages_mut()
            .retain(|m| !(ids.contains(&m.id()) && m.is_system()));
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
