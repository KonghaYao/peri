//! Explicit remote activation and content/effective-tool-bound approval.
use crate::{claude_agent_parser::ClaudeAgent, mcp::McpAgentRegistry};

impl super::SubAgentTool {
    pub(crate) async fn load_and_approve_mcp_agent(
        &self,
        agent_id: &str,
    ) -> Result<ClaudeAgent, Box<dyn std::error::Error + Send + Sync>> {
        use peri_agent::interaction::{
            ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
        };

        let registry = self
            .mcp_agent_registry
            .as_ref()
            .ok_or("MCP Agents are not available in this session")?;
        let activated = registry.activate(agent_id).await?;
        let effective_tools: Vec<String> = self
            .filter_tools(
                &activated.definition.frontmatter.tools,
                &activated.definition.frontmatter.disallowed_tools,
            )
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let approval_key = McpAgentRegistry::approval_key(&activated, &effective_tools);
        if !registry.is_approved(&approval_key) {
            let broker = self
                .broker
                .as_ref()
                .ok_or("MCP Agent activation requires an interaction broker")?;
            let response = broker
                .request(InteractionContext::Approval {
                    items: vec![ApprovalItem {
                        tool_call_id: format!("mcp-agent:{}", activated.metadata.id),
                        tool_name: "MCP Agent activation".to_string(),
                        tool_input: serde_json::json!({
                            "origin": activated.metadata.origin,
                            "name": activated.metadata.name,
                            "uri": activated.metadata.uri,
                            "digest": activated.digest,
                            "effective_tools": effective_tools,
                        }),
                    }],
                })
                .await;
            match response {
                InteractionResponse::Decisions(decisions)
                    if matches!(decisions.first(), Some(ApprovalDecision::Approve { .. })) =>
                {
                    registry.approve(approval_key);
                }
                InteractionResponse::Decisions(decisions) => {
                    let reason = match decisions.first() {
                        Some(ApprovalDecision::Reject { reason, .. }) => reason.as_str(),
                        Some(ApprovalDecision::Respond { message }) => message.as_str(),
                        Some(ApprovalDecision::Edit { .. }) => {
                            "MCP Agent activation approval cannot be edited"
                        }
                        _ => "MCP Agent activation was not approved",
                    };
                    return Err(reason.to_string().into());
                }
                _ => return Err("MCP Agent activation was rejected".into()),
            }
        }
        Ok(activated.definition)
    }
}
