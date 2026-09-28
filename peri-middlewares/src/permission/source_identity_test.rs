use super::*;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::capabilities::{BeforeToolState, BoundToolOrigin, StateView};
use std::sync::Mutex;

struct RejectBroker;

#[async_trait]
impl UserInteractionBroker for RejectBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Reject {
                        reason: "用户拒绝".to_string(),
                        source: None,
                    })
                    .collect(),
            ),
            _ => InteractionResponse::Decisions(vec![]),
        }
    }
}

fn make_tool_call(name: &str) -> ToolCall {
    ToolCall::new("test-id", name, serde_json::json!({}))
}

fn mcp_origin(name: &str, builtin: bool) -> Option<hook_state::BoundToolOrigin> {
    Some(hook_state::BoundToolOrigin {
        mcp_server_name: Some("source".to_string()),
        mcp_tool_name: Some(name.to_string()),
        builtin_mcp_instance: builtin.then(|| "workspace".to_string()),
    })
}

struct BoundState {
    origin: BoundToolOrigin,
}

impl StateView for BoundState {
    fn cwd(&self) -> &str {
        "/tmp"
    }
    fn messages(&self) -> &[BaseMessage] {
        &[]
    }
    fn current_step(&self) -> usize {
        0
    }
}

impl BeforeToolState for BoundState {
    fn tool_origin(&self, call_id: &str) -> Option<BoundToolOrigin> {
        (call_id == "test-id").then(|| self.origin.clone())
    }
}

/// [回归测试] 外部 MCP 的原名 Read 不得继承内置 Read 的免审批规则。
#[tokio::test]
async fn test_external_raw_read_requires_approval() {
    let mw = PermissionMiddleware::new(Arc::new(RejectBroker), default_requires_approval);
    let call = make_tool_call("Read");
    let mut state = BoundState {
        origin: mcp_origin("Read", false).unwrap(),
    };
    let results = mw.before_tools_batch(&mut state, &[call]).await;
    assert!(matches!(&results[0], Err(AgentError::ToolRejected { tool, .. }) if tool == "Read"));
}

/// [回归测试] AcceptEdit 只自动批准本地编辑能力，外部同名 Write 仍需审批。
#[tokio::test]
async fn test_accept_edit_does_not_auto_approve_external_raw_write() {
    let mw = PermissionMiddleware::with_shared_mode(
        Arc::new(RejectBroker),
        default_requires_approval,
        SharedPermissionMode::new(PermissionMode::AcceptEdit),
        None,
    );
    let call = make_tool_call("Write");
    let results = mw
        .process_batch_with_origins(&[call], &[mcp_origin("Write", false)])
        .await;
    assert!(matches!(&results[0], Err(AgentError::ToolRejected { tool, .. }) if tool == "Write"));
}

#[tokio::test]
async fn test_builtin_raw_read_keeps_read_policy() {
    let mw = PermissionMiddleware::new(Arc::new(RejectBroker), default_requires_approval);
    let call = make_tool_call("Read");
    let results = mw
        .process_batch_with_origins(&[call], &[mcp_origin("Read", true)])
        .await;
    assert_eq!(results[0].as_ref().unwrap().name, "Read");
}

#[tokio::test]
async fn test_external_raw_artifact_cannot_inherit_builtin_no_approval() {
    let mw = PermissionMiddleware::new(Arc::new(RejectBroker), default_requires_approval);
    let call = make_tool_call("artifact");
    let results = mw
        .process_batch_with_origins(&[call], &[mcp_origin("artifact", false)])
        .await;
    assert!(
        matches!(&results[0], Err(AgentError::ToolRejected { tool, .. }) if tool == "artifact")
    );
}

struct RecordingClassifier(Mutex<Vec<String>>);

#[async_trait]
impl AutoClassifier for RecordingClassifier {
    async fn classify(&self, tool_name: &str, _tool_input: &serde_json::Value) -> Classification {
        self.0.lock().unwrap().push(tool_name.to_string());
        Classification::Allow
    }
}

#[tokio::test]
async fn test_auto_mode_classifies_external_mcp_with_server_and_wire_identity() {
    let classifier = Arc::new(RecordingClassifier(Mutex::new(Vec::new())));
    let mw = PermissionMiddleware::with_shared_mode(
        Arc::new(RejectBroker),
        default_requires_approval,
        SharedPermissionMode::new(PermissionMode::AutoMode),
        Some(classifier.clone()),
    );
    let call = make_tool_call("Write");
    let external = mw
        .process_batch_with_origins(std::slice::from_ref(&call), &[mcp_origin("Write", false)])
        .await;
    let builtin = mw
        .process_batch_with_origins(&[call], &[mcp_origin("Write", true)])
        .await;
    assert!(external[0].is_ok());
    assert!(builtin[0].is_ok());
    let names = classifier.0.lock().unwrap();
    assert_eq!(names.len(), 2);
    let external: serde_json::Value = serde_json::from_str(&names[0]).unwrap();
    assert_eq!(external["kind"], "external_mcp");
    assert_eq!(external["server"], "source");
    assert_eq!(external["wire_tool"], "Write");
    assert_eq!(names[1], "Write");
}
