//! Subagent approval-scope inheritance (regression: delegation must not bypass
//! a parent session's declared external-MCP approval range).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::interaction::InteractionAttribution;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_agent::agent::react::ToolCall;
use peri_agent::error::AgentError;
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
    UserInteractionBroker,
};
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::capabilities::{
    BeforeToolState, BoundToolOrigin, HookOutputState, StateView,
};
use peri_agent::session::subagent::{SubagentChainAssembler, SubagentChainContext};

use crate::subagent::tool::session_binding::SessionBoundAssembler;
use crate::subagent::tool::SubAgentTool;
use crate::subagent::tool::SubagentChainAssemblerImpl;

struct CountingBroker(AtomicUsize);

#[async_trait::async_trait]
impl UserInteractionBroker for CountingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        self.0.fetch_add(1, Ordering::SeqCst);
        match ctx {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Approve { source: None })
                    .collect(),
            ),
            _ => InteractionResponse::Decisions(vec![]),
        }
    }
}

struct ChildApprovalState {
    origin: Option<BoundToolOrigin>,
}

impl StateView for ChildApprovalState {
    fn execution_binding(&self) -> Option<peri_acp_types::session::ExecutionBinding> {
        Some(peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        })
    }

    fn session_id(&self) -> Option<&str> {
        Some("child-session")
    }

    fn interaction_attribution(&self) -> Option<InteractionAttribution> {
        Some(InteractionAttribution::model_turn(
            "child-session",
            peri_acp_types::session::ExecutionBinding {
                turn_id: peri_acp_types::session::TurnId::new(),
                attempt_id: peri_acp_types::identity::AttemptId::new(),
            },
        ))
    }

    fn cwd(&self) -> &str {
        "/tmp"
    }

    fn messages(&self) -> &[BaseMessage] {
        &[]
    }

    fn current_step(&self) -> Option<usize> {
        Some(0)
    }
}

impl BeforeToolState for ChildApprovalState {
    fn tool_origin(&self, _call_id: &str) -> Option<BoundToolOrigin> {
        self.origin.clone()
    }
}

impl HookOutputState for ChildApprovalState {
    fn enqueue_hook_model_reminder(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("fixture has no hook delivery path");
    }

    fn enqueue_hook_client_notice(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("fixture has no hook delivery path");
    }

    fn enqueue_session_start_message(
        &self,
        _reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        panic!("fixture has no hook delivery path");
    }
}

fn external_origin() -> BoundToolOrigin {
    BoundToolOrigin {
        mcp_server_name: Some("declared".to_owned()),
        mcp_tool_name: Some("write".to_owned()),
        builtin_mcp_instance: None,
    }
}

fn other_server_origin() -> BoundToolOrigin {
    BoundToolOrigin {
        mcp_server_name: Some("other".to_owned()),
        mcp_tool_name: Some("read".to_owned()),
        builtin_mcp_instance: None,
    }
}

fn scoped_mode() -> Arc<SharedPermissionMode> {
    let mode = SharedPermissionMode::new(PermissionMode::Bypass);
    mode.set_mcp_approval_servers(["declared".to_owned()]);
    mode
}

/// The parent scope is copied into the child chain context; the child's bound
/// external MCP call reaches the same approval broker instead of running.
#[tokio::test]
async fn a_subagent_inherits_the_parent_scope_and_asks_for_approval() {
    let broker = Arc::new(CountingBroker(AtomicUsize::new(0)));
    let context = SubagentChainContext {
        cwd: "/tmp".to_owned(),
        permission_mode: Some(scoped_mode()),
        interaction_broker: Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
        ..SubagentChainContext::default()
    };
    let chain = SubagentChainAssemblerImpl::new().assemble(&context);
    let mut state = ChildApprovalState {
        origin: Some(external_origin()),
    };
    let results = chain
        .run_before_tools_batch(
            &mut state,
            vec![ToolCall::new(
                "call-1",
                "mcp__declared__write",
                serde_json::json!({}),
            )],
        )
        .await;
    assert!(results[0].is_ok(), "{:?}", results[0]);
    assert_eq!(broker.0.load(Ordering::SeqCst), 1);
}

/// Empty scope keeps the child chain at the pre-approval baseline: the same
/// external call executes without consulting any broker.
#[tokio::test]
async fn an_empty_parent_scope_keeps_the_subagent_baseline() {
    let broker = Arc::new(CountingBroker(AtomicUsize::new(0)));
    let context = SubagentChainContext {
        cwd: "/tmp".to_owned(),
        permission_mode: Some(SharedPermissionMode::new(PermissionMode::Bypass)),
        interaction_broker: Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
        ..SubagentChainContext::default()
    };
    let chain = SubagentChainAssemblerImpl::new().assemble(&context);
    let mut state = ChildApprovalState {
        origin: Some(external_origin()),
    };
    let results = chain
        .run_before_tools_batch(
            &mut state,
            vec![ToolCall::new(
                "call-1",
                "mcp__declared__write",
                serde_json::json!({}),
            )],
        )
        .await;
    assert!(results[0].is_ok(), "{:?}", results[0]);
    assert_eq!(broker.0.load(Ordering::SeqCst), 0);
}

/// Production wiring: the `SubAgentTool` carries the parent mode/broker, and
/// `SessionBoundAssembler` copies them into the child chain context.
#[tokio::test]
async fn the_bound_subagent_tool_forwards_its_scope_to_the_child_chain() {
    let broker = Arc::new(CountingBroker(AtomicUsize::new(0)));
    let mut tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| panic!("llm factory is not used by chain assembly")),
        "/tmp".to_owned(),
    );
    tool.permission_mode = Some(scoped_mode());
    tool.broker = Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>);

    let assembler = SessionBoundAssembler::new(&tool);
    let chain = assembler.assemble(&SubagentChainContext {
        cwd: "/tmp".to_owned(),
        ..SubagentChainContext::default()
    });
    let mut state = ChildApprovalState {
        origin: Some(external_origin()),
    };
    let results = chain
        .run_before_tools_batch(
            &mut state,
            vec![ToolCall::new(
                "call-1",
                "mcp__declared__write",
                serde_json::json!({}),
            )],
        )
        .await;
    assert!(results[0].is_ok(), "{:?}", results[0]);
    assert_eq!(broker.0.load(Ordering::SeqCst), 1);
}

/// The assembly branch "parent scope present, no inherited broker" must be
/// fail-closed: the child chain denies a call inside the declared range instead
/// of executing it silently, while calls outside the range keep the baseline.
#[tokio::test]
async fn a_scoped_subagent_without_a_broker_fails_closed() {
    let context = SubagentChainContext {
        cwd: "/tmp".to_owned(),
        permission_mode: Some(scoped_mode()),
        interaction_broker: None,
        ..SubagentChainContext::default()
    };
    let chain = SubagentChainAssemblerImpl::new().assemble(&context);
    let mut state = ChildApprovalState {
        origin: Some(external_origin()),
    };
    let results = chain
        .run_before_tools_batch(
            &mut state,
            vec![ToolCall::new(
                "call-1",
                "mcp__declared__write",
                serde_json::json!({}),
            )],
        )
        .await;
    assert!(
        matches!(
            &results[0],
            Err(AgentError::ToolRejected {
                cause: peri_acp_types::error::ToolRejectionCause::PolicyDenied,
                ..
            })
        ),
        "a scoped child call without a broker must be policy-denied: {:?}",
        results[0]
    );

    // The branch only closes the declared range; a call bound to another
    // server keeps the baseline and is not denied by the scope-only middleware.
    let mut other = ChildApprovalState {
        origin: Some(other_server_origin()),
    };
    let results = chain
        .run_before_tools_batch(
            &mut other,
            vec![ToolCall::new(
                "call-2",
                "mcp__other__read",
                serde_json::json!({ "path": "/tmp/x" }),
            )],
        )
        .await;
    assert!(results[0].is_ok(), "{:?}", results[0]);
}
