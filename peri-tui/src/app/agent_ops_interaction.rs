use peri_acp::transport::types::RequestId;
use peri_middlewares::hitl::BatchItem; // P4b: type-dependency, oneshot channels

use super::*;

impl App {
    /// Handle ACP RequestPermission: create HITL approval dialog.
    pub(crate) fn handle_acp_request_permission(
        &mut self,
        id: RequestId,
        params: serde_json::Value,
    ) -> (bool, bool, bool) {
        use agent_client_protocol::schema::RequestPermissionRequest;
        use tokio::sync::oneshot;

        let req = match serde_json::from_value::<RequestPermissionRequest>(params) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "Failed to parse RequestPermissionRequest");
                return (false, false, false);
            }
        };

        let tool_name = req
            .tool_call
            .fields
            .title
            .unwrap_or_else(|| "unknown".to_string());
        let tool_input = req
            .tool_call
            .fields
            .raw_input
            .unwrap_or(serde_json::Value::Null);

        let batch_items = vec![BatchItem {
            tool_name,
            input: tool_input,
        }];

        // Create oneshot bridge — the confirm() handler will call bridge_tx.send(decisions)
        let (bridge_tx, _bridge_rx) = oneshot::channel::<Vec<HitlDecision>>();

        // Store ACP request id for response dispatch in hitl_ops.rs
        self.session_mgr.current_mut().agent.pending_acp_request_id = Some(id);

        let prompt = HitlBatchPrompt::new(batch_items, bridge_tx);
        self.session_mgr.current_mut().agent.interaction_prompt =
            Some(InteractionPrompt::Approval(prompt));

        (true, true, false) // pause event consumption, wait for user confirmation
    }

    /// Handle ACP elicitation/create: create AskUser dialog.
    pub(crate) fn handle_acp_elicitation(
        &mut self,
        id: RequestId,
        params: serde_json::Value,
    ) -> (bool, bool, bool) {
        use agent_client_protocol_schema::{CreateElicitationRequest, ElicitationMode};
        use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData};
        use tokio::sync::oneshot;

        let req = match serde_json::from_value::<CreateElicitationRequest>(params.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "Failed to parse CreateElicitationRequest");
                return (false, false, false);
            }
        };

        let mut questions = Vec::new();

        if let ElicitationMode::Form(form) = req.mode {
            for (prop_id, prop) in &form.requested_schema.properties {
                let (title, description, is_multi, mut options) = match prop {
                    agent_client_protocol_schema::ElicitationPropertySchema::String(s) => (
                        s.title.clone(),
                        s.description.clone(),
                        false,
                        s.one_of
                            .as_ref()
                            .map(|opts| {
                                opts.iter()
                                    .map(|o| AskUserOption {
                                        label: o.title.clone(),
                                        description: None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    ),
                    agent_client_protocol_schema::ElicitationPropertySchema::Array(a) => (
                        a.title.clone(),
                        a.description.clone(),
                        true,
                        match &a.items {
                            agent_client_protocol_schema::MultiSelectItems::Titled(t) => t
                                .options
                                .iter()
                                .map(|o| AskUserOption {
                                    label: o.title.clone(),
                                    description: None,
                                })
                                .collect(),
                            _ => vec![],
                        },
                    ),
                    _ => continue,
                };

                // 从原始 JSON 中提取被 EnumOption 丢弃的 description
                let opt_descs = extract_option_descriptions(&params, prop_id, is_multi);
                for (i, desc) in opt_descs.into_iter().enumerate() {
                    if let Some(opt) = options.get_mut(i) {
                        if opt.description.is_none() {
                            opt.description = desc;
                        }
                    }
                }

                questions.push(AskUserQuestionData {
                    tool_call_id: prop_id.clone(),
                    question: description.unwrap_or_default(),
                    header: title.unwrap_or_default(),
                    multi_select: is_multi,
                    options,
                });
            }
        }

        // Create oneshot bridge — confirm() handler will call bridge_tx.send(answers)
        let (bridge_tx, _bridge_rx) = oneshot::channel::<Vec<String>>();

        // Store ACP request id for response dispatch in ask_user_ops.rs
        self.session_mgr.current_mut().agent.pending_acp_request_id = Some(id);
        self.session_mgr.current_mut().agent.pending_ask_user = Some(false);

        let (batch_req, _) = AskUserBatchRequest::new(questions);
        let batch_req_bridged = AskUserBatchRequest {
            questions: batch_req.questions,
            response_tx: bridge_tx,
        };
        self.session_mgr.current_mut().agent.interaction_prompt = Some(
            InteractionPrompt::Questions(AskUserBatchPrompt::from_request(batch_req_bridged)),
        );

        (true, true, false) // pause event consumption, wait for user input
    }

    /// Handle AgentEvent::InteractionRequest: create Approval or Questions dialog.
    pub(crate) fn handle_interaction_request(
        &mut self,
        ctx: peri_agent::interaction::InteractionContext,
        response_tx: tokio::sync::oneshot::Sender<peri_agent::interaction::InteractionResponse>,
    ) -> (bool, bool, bool) {
        use peri_agent::interaction::{
            ApprovalDecision, InteractionContext, InteractionResponse, QuestionAnswer,
        };
        use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData};
        use tokio::sync::oneshot;

        match ctx {
            InteractionContext::Approval { items } => {
                let batch_items: Vec<BatchItem> = items
                    .iter()
                    .map(|i| BatchItem {
                        tool_name: i.tool_name.clone(),
                        input: i.tool_input.clone(),
                    })
                    .collect();
                let (bridge_tx, bridge_rx) = oneshot::channel::<Vec<HitlDecision>>();
                tokio::spawn(async move {
                    if let Ok(decisions) = bridge_rx.await {
                        let approval_decisions: Vec<ApprovalDecision> = decisions
                            .into_iter()
                            .map(|d| match d {
                                HitlDecision::Approve => ApprovalDecision::Approve { source: None },
                                HitlDecision::Reject => ApprovalDecision::Reject {
                                    reason: "User rejected".to_string(),
                                    source: None,
                                },
                                HitlDecision::Edit(v) => ApprovalDecision::Edit { new_input: v },
                                HitlDecision::Respond(msg) => {
                                    ApprovalDecision::Respond { message: msg }
                                }
                            })
                            .collect();
                        let _ =
                            response_tx.send(InteractionResponse::Decisions(approval_decisions));
                    }
                });
                self.session_mgr.current_mut().agent.interaction_prompt = Some(
                    InteractionPrompt::Approval(HitlBatchPrompt::new(batch_items, bridge_tx)),
                );
                (true, true, false) // 暂停消费，等待用户确认
            }
            InteractionContext::Questions { requests } => {
                let ask_questions: Vec<AskUserQuestionData> = requests
                    .iter()
                    .map(|q| AskUserQuestionData {
                        tool_call_id: q.id.clone(),
                        question: q.question.clone(),
                        header: q.header.clone(),
                        multi_select: q.multi_select,
                        options: q
                            .options
                            .iter()
                            .map(|o| AskUserOption {
                                label: o.label.clone(),
                                description: o.description.clone(),
                            })
                            .collect(),
                    })
                    .collect();
                let (bridge_tx, bridge_rx) = oneshot::channel::<Vec<String>>();
                let ids: Vec<String> = requests.iter().map(|q| q.id.clone()).collect();
                tokio::spawn(async move {
                    if let Ok(answers) = bridge_rx.await {
                        let question_answers: Vec<QuestionAnswer> = ids
                            .into_iter()
                            .zip(answers)
                            .map(|(id, answer)| QuestionAnswer {
                                id,
                                selected: vec![answer.clone()],
                                text: Some(answer),
                            })
                            .collect();
                        let _ = response_tx.send(InteractionResponse::Answers(question_answers));
                    }
                });
                self.session_mgr.current_mut().agent.pending_ask_user = Some(false);
                let (batch_req, _) = AskUserBatchRequest::new(ask_questions);
                let batch_req_bridged = AskUserBatchRequest {
                    questions: batch_req.questions,
                    response_tx: bridge_tx,
                };
                self.session_mgr.current_mut().agent.interaction_prompt =
                    Some(InteractionPrompt::Questions(
                        AskUserBatchPrompt::from_request(batch_req_bridged),
                    ));
                (true, true, false) // 暂停消费，等待用户输入
            }
        }
    }
}

/// 从 Elicitation JSON 中提取每个属性的选项 description。
/// `inject_option_descriptions` (transport_broker) 在 JSON 层面注入了 description，
/// 但 `EnumOption` 结构体无此字段，反序列化后丢失。
fn extract_option_descriptions(
    params: &serde_json::Value,
    prop_id: &str,
    is_multi: bool,
) -> Vec<Option<String>> {
    let container_key = if is_multi { "anyOf" } else { "oneOf" };
    let Some(arr) = params
        // 对齐 transport_broker::inject_option_descriptions 的 JSON 路径：
        // requestedSchema 在顶层，不在 mode 下面（mode 是 serde flatten 的字符串 "form"）
        .get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(prop_id))
        .and_then(|prop| {
            // multi-select: options 在 items 下面；single: options 在 prop 下面
            if is_multi {
                prop.get("items")
            } else {
                Some(prop)
            }
        })
        .and_then(|p| p.get(container_key))
        .and_then(|v| v.as_array())
    else {
        return vec![];
    };
    arr.iter()
        .map(|opt| {
            opt.get("description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}
