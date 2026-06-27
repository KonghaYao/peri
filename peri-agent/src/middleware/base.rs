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
        println!("[{}] Agent starting | cwd: {}", self.name, state.cwd());
        Ok(())
    }

    async fn before_tool(
        &self,
        state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let step = state.current_step();
        if self.verbose {
            println!(
                "[{}] Step {step} | Calling tool: {} | input: {}",
                self.name, tool_call.name, tool_call.input
            );
        } else {
            println!(
                "[{}] Step {step} | Calling tool: {}",
                self.name, tool_call.name
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
            eprintln!(
                "[{}] Tool {} failed: {}",
                self.name, tool_call.name, result.output
            );
        } else if self.verbose {
            println!(
                "[{}] Tool {} succeeded: {}",
                self.name, tool_call.name, result.output
            );
        } else {
            println!("[{}] Tool {} succeeded", self.name, tool_call.name);
        }
        Ok(())
    }

    async fn after_agent(
        &self,
        _state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        println!("[{}] Agent completed in {} steps", self.name, output.steps);
        Ok(output.clone())
    }

    async fn on_error(
        &self,
        _state: &mut dyn MiddlewareState,
        error: &AgentError,
    ) -> AgentResult<()> {
        eprintln!("[{}] Agent error: {}", self.name, error);
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
        println!(
            "[{}] Total tool calls: {} | Steps: {}",
            self.name,
            output.tool_calls.len(),
            output.steps
        );
        Ok(output.clone())
    }
}
