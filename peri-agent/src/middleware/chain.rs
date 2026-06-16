use crate::{
    agent::{
        react::{AgentOutput, Reasoning, ToolCall, ToolResult},
        state::State,
    },
    error::AgentResult,
    middleware::r#trait::Middleware,
    tools::BaseTool,
};

/// 中间件链 - 按顺序执行所有中间件
pub struct MiddlewareChain<S: State> {
    middlewares: Vec<Box<dyn Middleware<S>>>,
}

impl<S: State> MiddlewareChain<S> {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    /// 添加中间件（追加到链尾）
    pub fn add(&mut self, middleware: Box<dyn Middleware<S>>) {
        self.middlewares.push(middleware);
    }

    /// 中间件数量
    pub fn len(&self) -> usize {
        self.middlewares.len()
    }

    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    /// 获取所有中间件名称
    pub fn names(&self) -> Vec<&str> {
        self.middlewares.iter().map(|m| m.name()).collect()
    }

    /// 收集所有中间件提供的工具（按注册顺序，后注册的同名工具覆盖先注册的）
    pub fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        self.middlewares
            .iter()
            .flat_map(|m| m.collect_tools(cwd))
            .collect()
    }

    /// 顺序执行 before_agent 钩子
    pub async fn run_before_agent(&self, state: &mut S) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.before_agent(state).await?;
        }
        Ok(())
    }

    /// 顺序执行 before_tool 钩子（每个中间件可修改 tool_call）
    pub async fn run_before_tool(
        &self,
        state: &mut S,
        tool_call: ToolCall,
    ) -> AgentResult<ToolCall> {
        let mut current = tool_call;
        for middleware in &self.middlewares {
            current = middleware.before_tool(state, &current).await?;
        }
        Ok(current)
    }

    /// 批量执行 before_tool 钩子（优化路径）
    ///
    /// 对每个中间件依次调用其 `before_tools_batch` 方法。
    /// 中间件的 batch 实现可将多个 tool call 合并处理（如 HITL 批量审批）。
    /// 当所有中间件都使用默认逐条实现时，效果等同于逐个调用 `run_before_tool`。
    ///
    /// 返回结果按输入顺序一一对应。若某个中间件返回非 `ToolRejected` 错误，
    /// 链式处理中断，后续中间件不再执行，其余位置填充相同错误。
    pub async fn run_before_tools_batch(
        &self,
        state: &mut S,
        calls: Vec<ToolCall>,
    ) -> Vec<AgentResult<ToolCall>> {
        let mut results: Vec<AgentResult<ToolCall>> = calls.into_iter().map(Ok).collect();

        for middleware in &self.middlewares {
            let current_calls: Vec<ToolCall> = results
                .iter()
                .filter_map(|r| r.as_ref().ok().cloned())
                .collect();
            if current_calls.is_empty() {
                break;
            }

            let batch_results = middleware.before_tools_batch(state, &current_calls).await;

            // 将 batch 结果按位置回写（消费结果，避免 AgentError::Clone 要求）
            let mut batch_iter = batch_results.into_iter();
            for result in results.iter_mut() {
                if result.is_ok() {
                    if let Some(batch_result) = batch_iter.next() {
                        *result = batch_result;
                    }
                }
            }
        }

        results
    }

    /// 顺序执行 after_tool 钩子
    pub async fn run_after_tool(
        &self,
        state: &mut S,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.after_tool(state, tool_call, result).await?;
        }
        Ok(())
    }

    /// 顺序执行 after_tools_batch 钩子
    ///
    /// 在一批并行工具调用全部完成并写入 state 后触发。
    /// 每个中间件按注册顺序依次执行，遇错即停。
    pub async fn run_after_tools_batch(
        &self,
        state: &mut S,
        results: &[(ToolCall, ToolResult)],
    ) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.after_tools_batch(state, results).await?;
        }
        Ok(())
    }

    /// 顺序执行 before_model 钩子
    ///
    /// 在每个 ReAct step 的 LLM 调用前执行。
    /// 遇错即停——后续中间件不执行，错误向上传播。
    pub async fn run_before_model(&self, state: &mut S) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.before_model(state).await?;
        }
        Ok(())
    }

    /// 顺序执行 after_model 钩子
    ///
    /// 在 LLM 调用返回后、工具分发或最终答案处理前执行。
    /// 传入完整的 `Reasoning`（思考文本、工具调用、最终答案）供中间件检查。
    /// 遇错即停。
    pub async fn run_after_model(&self, state: &mut S, reasoning: &Reasoning) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.after_model(state, reasoning).await?;
        }
        Ok(())
    }

    /// 顺序执行 after_agent 钩子（每个中间件可修改 output）
    pub async fn run_after_agent(
        &self,
        state: &mut S,
        output: AgentOutput,
    ) -> AgentResult<AgentOutput> {
        let mut current = output;
        for middleware in &self.middlewares {
            current = middleware.after_agent(state, &current).await?;
        }
        Ok(current)
    }

    /// 顺序执行 on_error 钩子
    pub async fn run_on_error(
        &self,
        state: &mut S,
        error: &crate::error::AgentError,
    ) -> AgentResult<()> {
        for middleware in &self.middlewares {
            middleware.on_error(state, error).await?;
        }
        Ok(())
    }
}

impl<S: State> Default for MiddlewareChain<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::{
        agent::state::AgentState,
        error::{AgentError, AgentResult},
        messages::{BaseMessage, ContentBlock, MessageId},
        middleware::r#trait::{Middleware, NoopMiddleware},
    };
    include!("chain_test.rs");
}
