//! Explicit remote activation and content/effective-tool-bound approval.
use peri_mcp_core::agent_definition::ClaudeAgent;

use crate::mcp::McpAgentRegistry;

impl super::SubAgentTool {
    pub(crate) async fn load_and_approve_mcp_agent(
        &self,
        agent_id: &str,
        approval_owner: Option<&peri_agent::interaction::TrustedInteractionOwner>,
    ) -> Result<ClaudeAgent, Box<dyn std::error::Error + Send + Sync>> {
        use peri_acp_types::error::ToolRejectionCause;
        use peri_agent::interaction::{
            ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
        };

        let registry = self
            .mcp_agent_registry
            .as_ref()
            .ok_or("MCP Agents are not available in this session")?;
        let activated = registry.activate(agent_id, true).await?;
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
            // 归属缺失（无 canonical turn 事实）⇒ 显式拒绝，不伪造归属。
            let Some(owner) = approval_owner.cloned() else {
                return Err(crate::interaction_owner::untrusted_owner_error(
                    "MCP Agent activation",
                )
                .into());
            };
            let response = broker
                .request(
                    owner,
                    InteractionContext::Approval {
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
                    },
                )
                .await;
            match response {
                InteractionResponse::Decisions(decisions)
                    if matches!(decisions.first(), Some(ApprovalDecision::Approve { .. })) =>
                {
                    registry.approve(approval_key);
                }
                InteractionResponse::Decisions(decisions) => {
                    // 逐条按「谁作出的决定」分类（裁决 R9）：拒绝 / 文本作答是用户
                    // 已作出的决定；无法兑现的改参与空决定是本端 fail-closed。
                    let (reason, cause) = match decisions.first() {
                        Some(ApprovalDecision::Reject { reason, .. }) => {
                            (reason.clone(), ToolRejectionCause::UserRejected)
                        }
                        Some(ApprovalDecision::Respond { message }) => {
                            (message.clone(), ToolRejectionCause::UserRejected)
                        }
                        Some(ApprovalDecision::Edit { .. }) => (
                            "MCP Agent activation approval cannot be edited".to_string(),
                            ToolRejectionCause::PolicyDenied,
                        ),
                        _ => (
                            "MCP Agent activation was not approved".to_string(),
                            ToolRejectionCause::PolicyDenied,
                        ),
                    };
                    return Err(peri_agent::error::AgentError::ToolRejected {
                        tool: "MCP Agent activation".into(),
                        reason,
                        cause,
                    }
                    .into());
                }
                // 无人可作答：如实转述原因，不冒充用户决定（裁决 R9）。
                InteractionResponse::Unanswered { cause } => {
                    return Err(peri_agent::error::AgentError::ToolRejected {
                        tool: "MCP Agent activation".into(),
                        reason: cause.reason_text().to_string(),
                        cause: ToolRejectionCause::ApprovalUnavailable(cause),
                    }
                    .into());
                }
                // 协议异常（无法识别的响应形状）：fail-closed 拒绝，不冒充任何人的决定。
                _ => {
                    return Err(peri_agent::error::AgentError::ToolRejected {
                        tool: "MCP Agent activation".into(),
                        reason: "MCP Agent activation was rejected".into(),
                        cause: ToolRejectionCause::PolicyDenied,
                    }
                    .into());
                }
            }
        }
        Ok(activated.definition)
    }
}
