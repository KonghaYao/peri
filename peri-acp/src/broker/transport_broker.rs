use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol_schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
    ElicitationContentValue, ElicitationFormMode, ElicitationSchema, ElicitationSessionScope,
    EnumOption, MultiSelectPropertySchema, StringPropertySchema,
};
use async_trait::async_trait;
use peri_agent::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse, QuestionAnswer,
    QuestionItem, UserInteractionBroker,
};

use crate::transport::AcpTransport;

/// A broker that uses [`AcpTransport`] to relay HITL and AskUser interactions
/// to the ACP client via `RequestPermission` and `elicitation/create` RPCs.
///
/// Each approval item is sent as a separate `RequestPermission` request.
/// Questions are aggregated into a single `elicitation/create` form.
pub struct AcpTransportBroker {
    transport: Arc<dyn AcpTransport>,
    session_id: SessionId,
}

impl AcpTransportBroker {
    pub fn new(transport: Arc<dyn AcpTransport>, session_id: SessionId) -> Self {
        Self {
            transport,
            session_id,
        }
    }
}

#[async_trait]
impl UserInteractionBroker for AcpTransportBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => self.handle_approval(items).await,
            InteractionContext::Questions { requests } => self.handle_questions(requests).await,
        }
    }
}

impl AcpTransportBroker {
    async fn handle_approval(&self, items: Vec<ApprovalItem>) -> InteractionResponse {
        let mut decisions = Vec::with_capacity(items.len());

        for item in &items {
            let tool_update = ToolCallUpdate::new(
                item.tool_call_id.clone(),
                ToolCallUpdateFields::new()
                    .title(item.tool_name.clone())
                    .status(ToolCallStatus::Pending)
                    .raw_input(item.tool_input.clone()),
            );

            let options = vec![
                PermissionOption::new("allow_once", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject_once", "Reject", PermissionOptionKind::RejectOnce),
            ];

            let request =
                RequestPermissionRequest::new(self.session_id.clone(), tool_update, options);
            let params = serde_json::to_value(&request).unwrap_or_default();

            match self
                .transport
                .send_request("session/request_permission", params)
                .await
            {
                Ok(response) => {
                    let decision = match serde_json::from_value::<RequestPermissionResponse>(
                        response,
                    ) {
                        Ok(resp) => map_permission_response(resp),
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to parse RequestPermission response");
                            ApprovalDecision::Reject {
                                reason: format!("Invalid response: {e}"),
                                source: None,
                            }
                        }
                    };
                    decisions.push(decision);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "RequestPermission transport error");
                    decisions.push(ApprovalDecision::Reject {
                        reason: format!("Permission request failed: {e}"),
                        source: None,
                    });
                }
            }
        }

        InteractionResponse::Decisions(decisions)
    }

    async fn handle_questions(&self, requests: Vec<QuestionItem>) -> InteractionResponse {
        // Build an elicitation form schema from the questions
        let mut schema = ElicitationSchema::new();

        for q in &requests {
            if q.multi_select && !q.options.is_empty() {
                let options: Vec<EnumOption> = q
                    .options
                    .iter()
                    .map(|o| EnumOption::new(&o.label, &o.label))
                    .collect();
                let prop = MultiSelectPropertySchema::titled(options)
                    .title(q.header.clone())
                    .description(q.question.clone());
                schema = schema.property(&q.id, prop, false);
            } else if !q.options.is_empty() {
                let options: Vec<EnumOption> = q
                    .options
                    .iter()
                    .map(|o| EnumOption::new(&o.label, &o.label))
                    .collect();
                let prop = StringPropertySchema::new()
                    .one_of(options)
                    .title(q.header.clone())
                    .description(q.question.clone());
                schema = schema.property(&q.id, prop, false);
            } else {
                let prop = StringPropertySchema::new()
                    .title(q.header.clone())
                    .description(q.question.clone());
                schema = schema.property(&q.id, prop, false);
            }
        }

        let scope = ElicitationSessionScope::new(self.session_id.clone());
        let form_mode = ElicitationFormMode::new(scope, schema);
        let request =
            CreateElicitationRequest::new(form_mode, "Please provide the requested information");
        let mut params = serde_json::to_value(&request).unwrap_or_default();

        // EnumOption only has const+title, no description field.
        // Inject description into each option's JSON so the TUI can read it.
        inject_option_descriptions(&mut params, &requests);

        match self
            .transport
            .send_request("elicitation/create", params)
            .await
        {
            Ok(response) => match serde_json::from_value::<CreateElicitationResponse>(response) {
                Ok(resp) => match resp.action {
                    ElicitationAction::Accept(accept) => {
                        let content = accept.content.unwrap_or_default();
                        let answers: Vec<QuestionAnswer> = requests
                            .into_iter()
                            .map(|q| map_elicitation_answer(q, &content))
                            .collect();
                        InteractionResponse::Answers(answers)
                    }
                    ElicitationAction::Decline => {
                        tracing::info!("Elicitation declined by user");
                        InteractionResponse::Rejected
                    }
                    ElicitationAction::Cancel => {
                        tracing::info!("Elicitation cancelled by user");
                        InteractionResponse::Answers(empty_answers(requests))
                    }
                    _ => {
                        tracing::warn!("Unknown elicitation action, returning empty answers");
                        InteractionResponse::Answers(empty_answers(requests))
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to parse elicitation response");
                    InteractionResponse::Answers(empty_answers(requests))
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "Elicitation request failed, returning empty answers");
                InteractionResponse::Answers(empty_answers(requests))
            }
        }
    }
}

// ─── helpers ────────────────────────────────────────────────────────────────────

fn map_permission_response(resp: RequestPermissionResponse) -> ApprovalDecision {
    match resp.outcome {
        RequestPermissionOutcome::Selected(selected) => {
            let SelectedPermissionOutcome { option_id, .. } = selected;
            match option_id.0.as_ref() {
                "allow_once" | "allow_always" => ApprovalDecision::Approve { source: None },
                _ => ApprovalDecision::Reject {
                    reason: format!("User selected {option_id}"),
                    source: None,
                },
            }
        }
        RequestPermissionOutcome::Cancelled => ApprovalDecision::Reject {
            reason: "Cancelled by user".into(),
            source: None,
        },
        _ => ApprovalDecision::Reject {
            reason: "Unknown response".into(),
            source: None,
        },
    }
}

fn map_elicitation_answer(
    q: QuestionItem,
    content: &std::collections::BTreeMap<String, ElicitationContentValue>,
) -> QuestionAnswer {
    let mut selected = Vec::new();
    let mut text = None;

    if let Some(val) = content.get(&q.id) {
        match val {
            ElicitationContentValue::String(s) => {
                if q.multi_select {
                    selected.push(s.clone());
                } else {
                    text = Some(s.clone());
                }
            }
            ElicitationContentValue::StringArray(arr) => {
                selected = arr.clone();
            }
            ElicitationContentValue::Boolean(b) => {
                text = Some(b.to_string());
            }
            ElicitationContentValue::Integer(n) => {
                text = Some(n.to_string());
            }
            ElicitationContentValue::Number(n) => {
                text = Some(n.to_string());
            }
            _ => {
                // Non-exhaustive: future variants default to text
                text = None;
            }
        }
    }

    QuestionAnswer {
        id: q.id,
        selected,
        text,
    }
}

fn empty_answers(requests: Vec<QuestionItem>) -> Vec<QuestionAnswer> {
    requests
        .into_iter()
        .map(|q| QuestionAnswer {
            id: q.id,
            selected: vec![],
            text: Some(String::new()),
        })
        .collect()
}

/// Inject `description` from `QuestionOption` into the serialized JSON's
/// `oneOf`/`anyOf` arrays. `EnumOption` (external crate) only has `const` + `title`,
/// so we patch the JSON value post-serialization.
fn inject_option_descriptions(params: &mut serde_json::Value, requests: &[QuestionItem]) {
    let Some(props) = params
        .get_mut("requestedSchema")
        .and_then(|s| s.get_mut("properties"))
        .and_then(|p| p.as_object_mut())
    else {
        return;
    };

    for q in requests {
        if q.options.is_empty() {
            continue;
        }
        let Some(prop) = props.get_mut(&q.id) else {
            continue;
        };
        let key = if prop.get("type").and_then(|t| t.as_str()) == Some("array") {
            "anyOf" // MultiSelectPropertySchema: items.anyOf
        } else {
            "oneOf" // StringPropertySchema: oneOf
        };
        // For array type, options are nested under "items"
        let container = if key == "anyOf" {
            prop.get_mut("items").and_then(|i| i.as_object_mut())
        } else {
            prop.as_object_mut()
        };
        let Some(container) = container else {
            continue;
        };
        if let Some(arr) = container.get_mut(key).and_then(|v| v.as_array_mut()) {
            for (opt_json, opt_data) in arr.iter_mut().zip(q.options.iter()) {
                if let Some(desc) = &opt_data.description {
                    opt_json["description"] = serde_json::Value::String(desc.clone());
                }
            }
        }
    }
}
