use async_trait::async_trait;

use crate::{
    agent::{
        react::{AgentOutput, Reasoning, ToolCall, ToolResult},
        state::State,
    },
    error::{AgentError, AgentResult},
    tools::BaseTool,
};

/// 中间件 trait - 与 TypeScript AgentMiddleware 对齐
///
/// 生命周期钩子执行顺序：
/// ── Agent 生命周期级 ──
/// 1. before_agent  - Agent 开始执行前
///
/// ── 每轮 ReAct 迭代 ──
/// 2. before_model  - 每轮 LLM 调用前（在 call_llm 之前）
/// 3. after_model   - 每轮 LLM 调用后（call_llm 返回后、工具分发/最终答案前）
/// 4. before_tool   - 每次工具调用前（可修改工具调用参数）
/// 5. after_tool    - 每次工具调用后
/// ── 每轮 ReAct 迭代 ──
///
/// 6. after_agent   - Agent 完成后（可修改最终输出）
/// 7. on_error      - 发生错误时
#[async_trait]
pub trait Middleware<S: State>: Send + Sync {
    /// 中间件名称（用于日志和调试）
    fn name(&self) -> &str;

    /// 声明此中间件提供的工具列表（根据工作目录动态生成）
    ///
    /// 默认返回空列表（无工具的中间件无需实现）。
    /// `ReActAgent` 在 `execute` 开始时自动收集所有中间件的工具并合并到工具表。
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![]
    }

    /// Agent 执行前调用
    /// 可用于初始化状态、注入上下文等
    async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
        let _ = state;
        Ok(())
    }

    /// 工具调用前调用
    /// 返回可能被修改的 ToolCall（用于参数注入、权限检查等）
    async fn before_tool(&self, state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
        let _ = state;
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
        state: &mut S,
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
        state: &mut S,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        let _ = (state, tool_call, result);
        Ok(())
    }

    /// 一批并行工具调用全部写入 state 后触发（每个 batch 一次）。
    /// 可用于聚合检查、批量日志等。
    async fn after_tools_batch(
        &self,
        _state: &mut S,
        _results: &[(ToolCall, ToolResult)],
    ) -> AgentResult<()> {
        let _ = (_state, _results);
        Ok(())
    }

    /// LLM 调用前调用（在每轮 ReAct 循环的 call_llm 之前）
    ///
    /// 可用于上下文压缩、token 预算检查等预处理操作。
    /// 默认空实现。
    async fn before_model(&self, state: &mut S) -> AgentResult<()> {
        let _ = state;
        Ok(())
    }

    /// LLM 调用后调用（call_llm 返回后、工具分发或最终答案处理前）
    ///
    /// `reasoning` 包含模型的完整响应（思考文本、工具调用列表、最终答案）。
    /// 可用于响应后处理、token 累积校验、日志记录等。
    /// 默认空实现。
    async fn after_model(&self, state: &mut S, reasoning: &Reasoning) -> AgentResult<()> {
        let _ = (state, reasoning);
        Ok(())
    }

    /// Agent 执行后调用
    /// 返回可能被修改的 AgentOutput（用于后处理、格式化等）
    async fn after_agent(&self, state: &mut S, output: &AgentOutput) -> AgentResult<AgentOutput> {
        let _ = state;
        Ok(output.clone())
    }

    /// 错误处理
    /// 可用于记录错误、触发告警等
    async fn on_error(&self, state: &mut S, error: &AgentError) -> AgentResult<()> {
        let _ = (state, error);
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
impl<S: State> Middleware<S> for NoopMiddleware {
    fn name(&self) -> &str {
        &self.name
    }
}
