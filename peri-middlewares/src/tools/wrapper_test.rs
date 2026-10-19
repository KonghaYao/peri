use super::*;
use peri_agent::tools::{ContextRetention, ToolContext};

struct PolicyTool;

#[async_trait]
impl BaseTool for PolicyTool {
    fn name(&self) -> &str {
        "PolicyTool"
    }
    fn description(&self) -> &str {
        "策略测试工具"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn visible_to_model(&self) -> bool {
        false
    }
    fn output_char_limit(&self) -> Option<usize> {
        Some(7)
    }
    fn prefers_persist(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::Recomputable
    }
    fn title(&self) -> Option<&str> {
        Some("策略标题")
    }
    fn namespace(&self) -> Option<&str> {
        Some("policy")
    }
    fn prompt_declaration(&self) -> Option<String> {
        Some("策略声明".into())
    }
    fn aliases(&self) -> &[&str] {
        &["PolicyAlias"]
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("invoke 委托".into())
    }
    async fn invoke_output(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ToolOutput::from_legacy("output 委托"))
    }
}

/// [回归测试] 包装器曾回落 trait 默认值，重新暴露隐藏工具并丢失策略。
#[tokio::test]
async fn test_wrappers_forward_policy_and_metadata() {
    let wrappers: Vec<Box<dyn BaseTool>> = vec![
        Box::new(BoxToolWrapper(Box::new(PolicyTool))),
        Box::new(ArcToolWrapper(Arc::new(PolicyTool))),
    ];
    for tool in wrappers {
        assert_eq!(tool.name(), "PolicyTool");
        assert_eq!(tool.description(), PolicyTool.description());
        assert_eq!(tool.parameters(), PolicyTool.parameters());
        assert!(tool.is_direct());
        assert!(!tool.visible_to_model());
        assert_eq!(tool.output_char_limit(), Some(7));
        assert!(tool.prefers_persist());
        assert_eq!(tool.context_retention(), ContextRetention::Recomputable);
        assert_eq!(tool.title(), Some("策略标题"));
        assert_eq!(tool.namespace(), Some("policy"));
        assert_eq!(tool.prompt_declaration().as_deref(), Some("策略声明"));
        assert_eq!(tool.aliases(), &["PolicyAlias"]);
        assert_eq!(tool.tool_description(), PolicyTool.tool_description());
        assert_eq!(
            tool.invoke(serde_json::json!({}), ToolContext::new(&[], "."))
                .await
                .unwrap(),
            "invoke 委托"
        );
        assert_eq!(
            tool.invoke_output(serde_json::json!({}), ToolContext::new(&[], "."))
                .await
                .unwrap()
                .text,
            "output 委托"
        );
    }
}
