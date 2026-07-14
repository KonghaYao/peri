use async_trait::async_trait;

use crate::{
    agent::react::{AgentOutput, ToolCall, ToolResult},
    error::{AgentError, AgentResult},
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

/// 日志中间件 - 记录 Agent 执行过程
pub struct LoggingMiddleware {
    name: String,
    /// 是否打印工具调用详情
    verbose: bool,
}

impl LoggingMiddleware {
    pub fn new() -> Self {
        Self {
            name: "logging".to_string(),
            verbose: false,
        }
    }

    pub fn verbose(mut self) -> Self {
        self.verbose = true;
        self
    }
}

impl Default for LoggingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for LoggingMiddleware {
    fn name(&self) -> &str {
        &self.name
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        tracing::info!(name = %self.name, cwd = %state.cwd(), "Agent starting");
        Ok(())
    }

    async fn before_tool(
        &self,
        state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let step = state.current_step();
        if self.verbose {
            tracing::info!(
                name = %self.name,
                step,
                tool = %tool_call.name,
                input = %tool_call.input,
                "Calling tool"
            );
        } else {
            tracing::info!(
                name = %self.name,
                step,
                tool = %tool_call.name,
                "Calling tool"
            );
        }
        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        if result.is_error {
            tracing::warn!(
                name = %self.name,
                tool = %tool_call.name,
                output = %result.output,
                "Tool failed"
            );
        } else if self.verbose {
            tracing::info!(
                name = %self.name,
                tool = %tool_call.name,
                output = %result.output,
                "Tool succeeded"
            );
        } else {
            tracing::info!(
                name = %self.name,
                tool = %tool_call.name,
                "Tool succeeded"
            );
        }
        Ok(())
    }

    async fn after_agent(
        &self,
        _state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        tracing::info!(name = %self.name, steps = output.steps, "Agent completed");
        Ok(output.clone())
    }

    async fn on_error(
        &self,
        _state: &mut dyn MiddlewareState,
        error: &AgentError,
    ) -> AgentResult<()> {
        tracing::warn!(name = %self.name, error = %error, "Agent error");
        Ok(())
    }
}

/// 步骤计数中间件 - 追踪执行指标
pub struct MetricsMiddleware {
    name: String,
}

impl MetricsMiddleware {
    pub fn new() -> Self {
        Self {
            name: "metrics".to_string(),
        }
    }
}

impl Default for MetricsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for MetricsMiddleware {
    fn name(&self) -> &str {
        &self.name
    }

    async fn after_agent(
        &self,
        _state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        tracing::info!(
            name = %self.name,
            tool_calls = output.tool_calls.len(),
            steps = output.steps,
            "Total tool calls"
        );
        Ok(output.clone())
    }
}
