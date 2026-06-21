//! # tool_integration
//!
//! 演示工具注册和 ReAct 循环中的工具调用
//! 工具使用自主的 BaseTool trait（invoke 方法）

use async_trait::async_trait;
use peri_agent::prelude::*;

/// 简单计算器工具（实现 BaseTool trait）
struct CalculatorTool;

#[async_trait]
impl BaseTool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "执行基本数学运算。支持 add、sub、mul、div 操作。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["add", "sub", "mul", "div"]
                },
                "a": { "type": "number" },
                "b": { "type": "number" }
            },
            "required": ["operation", "a", "b"]
        })
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let op = input["operation"].as_str().unwrap_or("add");
        let a = input["a"].as_f64().unwrap_or(0.0);
        let b = input["b"].as_f64().unwrap_or(0.0);

        let result = match op {
            "add" => a + b,
            "sub" => a - b,
            "mul" => a * b,
            "div" => {
                if b == 0.0 {
                    return Err("Division by zero".into());
                }
                a / b
            }
            _ => return Err(format!("Unknown operation: {op}").into()),
        };

        Ok(format!("{a} {op} {b} = {result}"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let llm = MockLLM::tool_then_answer(
        "calculator",
        serde_json::json!({ "operation": "add", "a": 42, "b": 58 }),
        "计算结果是 100",
    );

    let agent = ReActAgent::new(llm)
        .register_tool(Box::new(CalculatorTool))
        .add_middleware(Box::new(LoggingMiddleware::new().verbose()))
        .add_middleware(Box::new(MetricsMiddleware::new()));

    let mut state = AgentState::new("/workspace");
    let output = agent
        .execute(AgentInput::text("请计算 42 + 58"), &mut state, None)
        .await?;

    println!("\n=== Final Answer ===");
    println!("{}", output.text);
    println!(
        "Steps: {}, Tool calls: {}",
        output.steps,
        output.tool_calls.len()
    );

    Ok(())
}
