use async_trait::async_trait;

use crate::{
    agent::react::{AgentOutput, Reasoning, ToolCall, ToolResult},
    error::{AgentError, AgentResult},
    hitl::BatchItem,
    middleware::state::MiddlewareState,
    tools::BaseTool,
};

/// 中间件 trait - 与 TypeScript AgentMiddleware 对齐（v2 扩展）
///
/// 所有钩子用 `&mut dyn MiddlewareState`，MiddlewareChain 不泛型，v2 stages 直接调用。
///
/// ## 生命周期钩子执行顺序
///
/// ── Session 级 ──
/// 1.  on_session_start       - Session 创建时
/// 17. on_session_end         - Session 销毁时
///
/// ── 用户输入 ──
/// 2.  on_user_prompt         - 用户提交 prompt 时
///
/// ── Agent 生命周期级 ──
/// 3.  before_agent           - Agent 开始执行前
///
/// ── 每轮 ReAct 迭代 ──
/// 4.  before_model           - 每轮 LLM 调用前
/// 5.  after_model            - 每轮 LLM 调用后
/// 6.  before_tools_batch     - 批量工具调用前
/// 7.  before_tool            - 每次工具调用前
/// 8.  after_tool             - 每次工具调用后
/// 9.  after_tools_batch      - 批量结果写入后
/// ── 每轮 ReAct 迭代 ──
///
/// 10. after_agent            - Agent 完成后
/// 11. on_turn_end            - 每轮 ReAct 结束时
///
/// ── Compact（before_model 之前的条件性步骤）──
/// 12. before_compact          - Compact 启动前
/// 13. after_compact           - Compact 完成后
///
/// ── 观测层 ──
/// 14. on_permission_request   - 权限审批请求时（只读）
/// 15. on_subagent_start       - SubAgent 启动时
/// 16. on_subagent_stop        - SubAgent 结束时
/// 17. on_notification         - 通知事件
///
/// ── 错误 ──
/// 18. on_error                - 发生错误时
#[async_trait]
pub trait Middleware: Send + Sync {
    /// 中间件名称（用于日志和调试）
    fn name(&self) -> &str;

    /// 声明此中间件提供的工具列表（根据工作目录动态生成）
    ///
    /// 默认返回空列表（无工具的中间件无需实现）。
    /// v2 stages 在 `build_stage_context` 入口自动收集所有中间件的工具并合并到 `shared_tools`。
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![]
    }

    /// Agent 执行前调用
    /// 可用于初始化状态、注入上下文等
    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    /// 首轮用户 turn 的一次性受控通知（可选实现）。
    ///
    /// Executor 仅在首个模型可见 turn（history 为空且非 continuation/keepgoing）
    /// 的 Prompt 消息之前调用一次，收集所有中间件的非空文本并作为 Info 消息
    /// （`<system-reminder>` 包裹）先行入队——首轮 Receive 即消费，模型首轮
    /// 可见。
    ///
    /// 约定：文本应短小精炼（摘要级）；返回 `None` 表示无贡献。纯生成无记账——
    /// 入队前失败/取消不产生副作用，下个首 turn 重新生成即可。
    async fn first_turn_reminder(
        &self,
        _state: &mut dyn MiddlewareState,
    ) -> AgentResult<Option<String>> {
        Ok(None)
    }

    /// 工具调用前调用
    /// 返回可能被修改的 ToolCall（用于参数注入、权限检查等）
    async fn before_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        Ok(tool_call.clone())
    }

    /// 批量工具调用前处理（可选优化路径）
    ///
    /// 当中间件可对多个工具调用进行合并处理时（如 HITL 批量审批），
    /// 应覆盖此方法。默认实现回退到逐个调用 `before_tool`。
    ///
    /// 返回值：`Vec<AgentResult<ToolCall>>`，与输入 `calls` 按顺序一一对应。
    /// 返回的错误可以是 `ToolRejected`（不中断流程）或其它错误（中断流程）。
    async fn before_tools_batch(
        &self,
        state: &mut dyn MiddlewareState,
        calls: &[ToolCall],
    ) -> Vec<AgentResult<ToolCall>> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.before_tool(state, call).await);
        }
        results
    }

    /// 工具调用后调用
    /// 可用于日志记录、结果转换等
    async fn after_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        _tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        Ok(())
    }

    /// 一批并行工具调用全部写入 state 后触发（每个 batch 一次）。
    /// 可用于聚合检查、批量日志等。
    async fn after_tools_batch(
        &self,
        _state: &mut dyn MiddlewareState,
        _results: &[(ToolCall, ToolResult)],
    ) -> AgentResult<()> {
        Ok(())
    }

    /// LLM 调用前调用（在每轮 ReAct 循环的 call_llm 之前）
    ///
    /// 可用于上下文压缩、token 预算检查等预处理操作。
    /// 默认空实现。
    async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    /// LLM 调用后调用（call_llm 返回后、工具分发或最终答案处理前）
    ///
    /// `reasoning` 包含模型的完整响应（思考文本、工具调用列表、最终答案）。
    /// 可用于响应后处理、token 累积校验、日志记录等。
    /// 默认空实现。
    async fn after_model(
        &self,
        _state: &mut dyn MiddlewareState,
        _reasoning: &Reasoning,
    ) -> AgentResult<()> {
        Ok(())
    }

    /// Agent 执行后调用
    /// 返回可能被修改的 AgentOutput（用于后处理、格式化等）
    async fn after_agent(
        &self,
        _state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        Ok(output.clone())
    }

    /// 错误处理
    /// 可用于记录错误、触发告警等
    async fn on_error(
        &self,
        _state: &mut dyn MiddlewareState,
        _error: &AgentError,
    ) -> AgentResult<()> {
        Ok(())
    }

    // ── 声明式 Prompt 贡献 ──

    /// 声明此中间件对 System Prompt 的文本贡献。
    ///
    /// Executor 在 `before_agent` 完成后收集所有中间件的贡献，
    /// 拼接后追加到 frozen system prompt 之后。
    /// 不再通过 `prepend_message` 注入——保持 prompt cache 前缀稳定。
    fn prompt_contribution(&self) -> Option<String> {
        None
    }

    // ── Session 生命周期 ──

    /// Session 创建时触发（`session/new` 完成后、ReAct 循环启动前）。
    ///
    /// 可用于初始化会话级状态、注册一次性资源等。
    async fn on_session_start(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    /// Session 销毁时触发。
    ///
    /// 可用于资源释放、孤儿 Agent 清理等。
    async fn on_session_end(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    // ── 用户输入 ──

    /// 用户提交 prompt 时触发。
    ///
    /// 可用于 prompt 预处理、意图识别、上下文注入等。
    async fn on_user_prompt(
        &self,
        _state: &mut dyn MiddlewareState,
        _prompt: &str,
    ) -> AgentResult<()> {
        Ok(())
    }

    // ── Compact（before_model 之前的条件性步骤）──

    /// Compact 启动前触发（观测层）。
    ///
    /// 可用于外部监听压缩开始事件，不修改 State。
    async fn before_compact(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    /// Compact 完成后触发（观测层）。
    ///
    /// 可用于外部监听压缩结束事件、验证压缩结果等。
    async fn after_compact(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    // ── 权限审批（观测层）──

    /// 权限审批请求时触发（观测层，只读）。
    ///
    /// 可用于审计日志、审批遥测上报等。
    async fn on_permission_request(
        &self,
        _state: &mut dyn MiddlewareState,
        _request: &BatchItem,
    ) -> AgentResult<()> {
        Ok(())
    }

    // ── SubAgent 生命周期 ──

    /// SubAgent 启动时触发（观测层）。
    ///
    /// 可用于子 Agent 生命周期追踪、资源分配等。
    async fn on_subagent_start(
        &self,
        _state: &mut dyn MiddlewareState,
        _agent_id: &str,
        _name: &str,
    ) -> AgentResult<()> {
        Ok(())
    }

    /// SubAgent 结束时触发（观测层）。
    ///
    /// `reason` 描述子 Agent 的退出原因（正常完成/错误/中断等）。
    async fn on_subagent_stop(
        &self,
        _state: &mut dyn MiddlewareState,
        _agent_id: &str,
        _reason: &str,
    ) -> AgentResult<()> {
        Ok(())
    }

    // ── Turn 结束 ──

    /// 每轮 ReAct 迭代结束时触发（在 `after_agent` 之后）。
    ///
    /// 可用于 turn 边界标记、Langfuse 遥测上报等。
    async fn on_turn_end(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }

    // ── 通知 ──

    /// 通知事件触发（外部通知、系统消息等）。
    ///
    /// 可用于将外部事件桥接到 Agent 上下文。
    async fn on_notification(
        &self,
        _state: &mut dyn MiddlewareState,
        _message: &str,
    ) -> AgentResult<()> {
        Ok(())
    }
}

/// 空中间件 - 所有钩子均为 no-op，用于测试或占位
pub struct NoopMiddleware {
    name: String,
}

impl NoopMiddleware {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl Middleware for NoopMiddleware {
    fn name(&self) -> &str {
        &self.name
    }
}
