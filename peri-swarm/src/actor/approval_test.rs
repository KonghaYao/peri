//! MCP approval → `requires_action` behaviour at the item-log boundary.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::broadcast;

use super::{
    ApprovalClaim, ApprovalCompletion, ApprovalReply, ItemLog, PermissionStart, TurnOutcome,
};
use crate::domain::{
    ApprovalDecision, ItemKind, McpToolSpec, SessionState, SettlementReason, StreamEvent,
};
use crate::store::{MemorySwarmStore, SwarmSessionRow, SwarmStore};

fn tool(label: &str, require_approval: serde_json::Value) -> McpToolSpec {
    let mut spec = McpToolSpec::new(label, "https://mcp.example.test/mcp");
    spec.require_approval = Some(require_approval);
    spec
}

fn session_row(tools: Vec<McpToolSpec>) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: "agent-1".to_owned(),
        root_session_id: "0192f0c5-0000-7000-8000-000000000001".to_owned(),
        actor_key: "agent/agent-1".to_owned(),
        cwd: "/tmp".to_owned(),
        state: SessionState::Idle,
        model: None,
        instructions: None,
        mcp_tools: tools,
        environment: crate::domain::EnvironmentRef::none(),
        metadata: Default::default(),
        prompt_cache_options: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
        last_item_seq: 0,
    }
}

struct Fixture {
    log: Arc<ItemLog>,
    store: Arc<dyn SwarmStore>,
    events: broadcast::Receiver<StreamEvent>,
}

async fn harness(tools: Vec<McpToolSpec>) -> Fixture {
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    store
        .swarm_upsert_session(session_row(tools.clone()))
        .await
        .expect("session row");
    let (channel, events) = broadcast::channel(64);
    let log = Arc::new(
        ItemLog::new(
            "agent-1",
            Arc::clone(&store),
            channel,
            Arc::new(AtomicU64::new(0)),
        )
        .with_approval_declarations(&tools),
    );
    Fixture { log, store, events }
}

fn permission(server: &str, tool_name: &str) -> serde_json::Value {
    json!({
        "_meta": { "peri.mcp": { "server": server, "tool": tool_name } },
        "toolCall": {
            "toolCallId": "call-1",
            "title": tool_name,
            "rawInput": { "query": "x" },
        },
    })
}

fn drain_official(
    events: &mut broadcast::Receiver<StreamEvent>,
) -> Vec<crate::domain::OfficialEvent> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let StreamEvent::Official(official) = event {
            out.push(official);
        }
    }
    out
}

async fn open_turn(log: &ItemLog) -> String {
    let request_id = "prompt-1".to_owned();
    log.begin_turn(&request_id, "turn_1")
        .await
        .expect("turn opens");
    request_id
}

#[tokio::test]
async fn declared_always_opens_a_pending_approval_and_broadcasts_requires_action() {
    let mut fixture = harness(vec![tool("server-a", json!("always"))]).await;
    let request_id = open_turn(&fixture.log).await;
    let start = fixture
        .log
        .begin_permission(&json!(7), &permission("server-a", "search"))
        .await
        .expect("permission recorded");
    assert_eq!(start, PermissionStart::Pending);

    let required = fixture.log.required_actions();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0]["type"], "mcp_approval_request");
    assert_eq!(required[0]["request"]["type"], "mcp_tool_call");
    assert_eq!(required[0]["request"]["server_label"], "server-a");
    assert_eq!(required[0]["request"]["name"], "search");
    assert!(required[0]["request"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("search") && message.contains("server-a")));
    assert_eq!(required[0]["turn_id"], "turn_1");
    assert!(required[0]["request_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("appr_")));

    let official = drain_official(&mut fixture.events);
    let requires_action = official
        .iter()
        .find(|event| event.event_type() == "agent.session.requires_action")
        .expect("requires_action frame");
    assert_eq!(requires_action.event_id(), "evt_item_1_requires_action");
    let session = match requires_action {
        crate::domain::OfficialEvent::RequiresAction { session, .. } => session,
        other => panic!("unexpected frame: {other:?}"),
    };
    assert_eq!(session.status, "requires_action");
    assert_eq!(session.required_actions.len(), 1);

    // The opening fact is durable and readable on reconnect.
    let items = fixture
        .store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("items");
    let approval = items
        .iter()
        .find(|item| item.kind == ItemKind::Approval)
        .expect("pending approval item");
    assert_eq!(approval.payload["state"], "pending");
    assert_eq!(approval.payload["turnRequestId"], request_id);
    assert_eq!(approval.payload["serverLabel"], "server-a");
}

#[tokio::test]
async fn a_declared_never_tool_is_auto_approved_and_leaves_no_pending_fact() {
    let mut fixture = harness(vec![tool(
        "server-a",
        json!({ "always": { "tool_names": ["write"] }, "never": { "tool_names": ["read"] } }),
    )])
    .await;
    open_turn(&fixture.log).await;
    drain_official(&mut fixture.events);
    let start = fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "read"))
        .await
        .expect("permission handled");
    assert_eq!(start, PermissionStart::AutoApprove);
    assert!(fixture.log.required_actions().is_empty());
    assert!(drain_official(&mut fixture.events).is_empty());
}

#[tokio::test]
async fn a_scoped_tool_without_a_rule_waits_and_a_never_server_is_not_escalated() {
    let tools = vec![
        tool("server-a", json!({ "always": { "tool_names": ["write"] } })),
        tool("server-b", json!("never")),
    ];
    // Only the declaring server enters the host escalation scope: the same
    // tool name on server-b is never routed to approval.
    assert_eq!(
        crate::domain::escalation_servers(&tools),
        ["server-a".to_owned()].into_iter().collect()
    );
    let fixture = harness(tools).await;
    open_turn(&fixture.log).await;
    // `search` is not named by the object form: fail-closed.
    assert_eq!(
        fixture
            .log
            .begin_permission(&json!(1), &permission("server-a", "search"))
            .await
            .expect("handled"),
        PermissionStart::Pending
    );
    // An unattributable origin is also fail-closed.
    assert_eq!(
        fixture
            .log
            .begin_permission(&json!(2), &json!({ "toolCall": { "title": "search" } }))
            .await
            .expect("handled"),
        PermissionStart::Pending
    );
}

#[tokio::test]
async fn an_approval_is_settled_once_and_the_same_idempotency_key_replays_its_receipt() {
    let mut fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(42), &permission("server-a", "search"))
        .await
        .expect("pending");
    let request_id = fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();

    let first = fixture
        .log
        .respond_approval(
            &request_id,
            ApprovalDecision::Approve,
            Some("0192f0c5-0000-7000-8000-0000000000aa".to_owned()),
        )
        .await;
    let (acp_id, receipt) = match first {
        ApprovalReply::Applied {
            acp_request_id,
            receipt,
        } => (acp_request_id, receipt),
        other => panic!("expected applied, got {other:?}"),
    };
    // The answer goes back to the original JSON-RPC id, not a new turn.
    assert_eq!(acp_id, json!(42));
    assert_eq!(receipt.disposition, "inserted");
    assert_eq!(receipt.decision.as_deref(), Some("approve"));

    let replay = fixture
        .log
        .respond_approval(
            &request_id,
            ApprovalDecision::Approve,
            Some("0192f0c5-0000-7000-8000-0000000000aa".to_owned()),
        )
        .await;
    assert!(matches!(replay, ApprovalReply::Duplicate { .. }));

    let conflicting = fixture
        .log
        .respond_approval(&request_id, ApprovalDecision::Deny, None)
        .await;
    assert!(matches!(
        conflicting,
        ApprovalReply::Settled {
            reason: SettlementReason::Answered
        }
    ));
    assert!(fixture.log.required_actions().is_empty());
    let official = drain_official(&mut fixture.events);
    let settled = official
        .iter()
        .rev()
        .find(|event| event.event_type() == "agent.session.requires_action")
        .expect("settlement snapshot");
    let session = match settled {
        crate::domain::OfficialEvent::RequiresAction { session, .. } => session,
        other => panic!("unexpected frame: {other:?}"),
    };
    assert!(session.required_actions.is_empty());
}

#[tokio::test]
async fn two_concurrent_answers_have_exactly_one_winner() {
    let log = harness(vec![tool("server-a", json!("always"))]).await.log;
    open_turn(&log).await;
    log.begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    let request_id = log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();

    let (left, right) = tokio::join!(
        log.respond_approval(&request_id, ApprovalDecision::Approve, None),
        log.respond_approval(&request_id, ApprovalDecision::Deny, None),
    );
    let winners = [&left, &right]
        .into_iter()
        .filter(|reply| matches!(reply, ApprovalReply::Applied { .. }))
        .count();
    assert_eq!(winners, 1);
}

#[tokio::test]
async fn a_parallel_approval_survives_answering_another() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "first"))
        .await
        .expect("pending");
    fixture
        .log
        .begin_permission(&json!(2), &permission("server-a", "second"))
        .await
        .expect("pending");
    let ids: Vec<String> = fixture
        .log
        .required_actions()
        .iter()
        .map(|action| action["request_id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(ids.len(), 2);

    let first = fixture
        .log
        .respond_approval(&ids[0], ApprovalDecision::Deny, None)
        .await;
    assert!(matches!(first, ApprovalReply::Applied { .. }));
    let remaining = fixture.log.required_actions();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["request_id"], ids[1]);

    // The settlement snapshot still reports `requires_action` for the other
    // pending approval; only its last frame clears the state.
    let mut events = fixture.events;
    let official = drain_official(&mut events);
    let snapshot = official
        .iter()
        .rev()
        .find(|event| event.event_type() == "agent.session.requires_action")
        .expect("settlement snapshot");
    let session = match snapshot {
        crate::domain::OfficialEvent::RequiresAction { session, .. } => session,
        other => panic!("unexpected frame: {other:?}"),
    };
    assert_eq!(session.status, "requires_action");
    assert_eq!(session.required_actions.len(), 1);
}

#[tokio::test]
async fn a_turn_terminal_settles_its_pending_approvals() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    let request_id = open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    let approval = fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();

    fixture
        .log
        .finish_turn(&request_id, TurnOutcome::Completed("end_turn".to_owned()))
        .await;
    assert!(fixture.log.required_actions().is_empty());
    assert!(matches!(
        fixture
            .log
            .respond_approval(&approval, ApprovalDecision::Approve, None)
            .await,
        ApprovalReply::Settled {
            reason: SettlementReason::TurnEnded
        }
    ));

    let items = fixture
        .store
        .swarm_items_after("agent-1", 0, 50)
        .await
        .expect("items");
    let settlement = items
        .iter()
        .filter(|item| item.kind == ItemKind::Approval)
        .find(|item| item.payload["state"] == "settled")
        .expect("settled item");
    assert_eq!(settlement.payload["reason"], "turn_ended");
}

#[tokio::test]
async fn an_expired_approval_is_not_projected_and_cannot_be_answered() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    let approval = fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();
    fixture.log.expire_approval_now(&approval);
    assert!(fixture.log.required_actions().is_empty());
    assert_eq!(
        fixture
            .log
            .settle_expired_approvals()
            .await
            .expect("settle expired"),
        1
    );
    assert!(matches!(
        fixture
            .log
            .respond_approval(&approval, ApprovalDecision::Approve, None)
            .await,
        ApprovalReply::Settled {
            reason: SettlementReason::Expired
        }
    ));
}

#[tokio::test]
async fn actor_loss_invalidates_every_pending_approval() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    assert_eq!(
        fixture
            .log
            .invalidate_approvals(SettlementReason::ActorLost)
            .await
            .expect("invalidate approvals"),
        1
    );
    assert!(fixture.log.required_actions().is_empty());
}

#[tokio::test]
async fn another_session_never_knows_this_approval_id() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    let other = harness(vec![tool("server-a", json!("always"))]).await;
    let other_log = Arc::clone(&other.log);
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    let approval = fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert!(matches!(
        other_log
            .respond_approval(&approval, ApprovalDecision::Approve, None)
            .await,
        ApprovalReply::Unknown
    ));
}

#[tokio::test]
async fn a_delivery_failure_release_keeps_the_action_answerable() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending");
    let request_id = fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Phase one reserves the action but does not settle it.
    let claim = fixture.log.claim_approval(&request_id, None).await;
    assert!(matches!(claim, ApprovalClaim::Claimed { .. }));
    assert_eq!(
        fixture.log.required_actions().len(),
        1,
        "a claimed-but-undelivered action stays projected"
    );

    // A concurrent answer is told the delivery is in flight; it does not delete
    // the action.
    assert!(matches!(
        fixture.log.claim_approval(&request_id, None).await,
        ApprovalClaim::InFlight
    ));
    assert_eq!(fixture.log.required_actions().len(), 1);

    // Delivery failed: release the reservation without settling.
    fixture.log.release_approval_claim(&request_id);
    assert_eq!(fixture.log.required_actions().len(), 1);

    // The retry can claim and complete; only now is the action removed.
    assert!(matches!(
        fixture.log.claim_approval(&request_id, None).await,
        ApprovalClaim::Claimed { .. }
    ));
    let completed = fixture
        .log
        .complete_approval_claim(&request_id, ApprovalDecision::Approve, None)
        .await;
    assert!(matches!(
        completed,
        ApprovalCompletion::Completed { ref receipt }
            if receipt.disposition == "inserted" && receipt.decision.as_deref() == Some("approve")
    ));
    assert!(fixture.log.required_actions().is_empty());

    let items = fixture
        .store
        .swarm_items_after("agent-1", 0, 50)
        .await
        .expect("items");
    assert!(
        items
            .iter()
            .any(|item| item.payload["state"] == "settled" && item.payload["reason"] == "answered"),
        "successful delivery writes the answered settlement"
    );
}

/// A call whose source cannot be determined stays typed (`server_label: ""` is
/// the only non-null value the official `string` field allows) and its message
/// says so; an empty wire tool name is projected as `name: null`, never `""`.
#[tokio::test]
async fn an_unattributable_permission_projects_an_empty_server_and_a_null_name() {
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(
            &json!(1),
            &json!({
                "toolCall": {
                    "toolCallId": "call-unknown",
                    "title": "",
                    "rawInput": { "query": "x" },
                },
            }),
        )
        .await
        .expect("fail-closed pending");

    let required = fixture.log.required_actions();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0]["request"]["server_label"], "");
    assert!(required[0]["request"]["name"].is_null());
    let message = required[0]["request"]["message"].as_str().expect("message");
    assert!(
        message.contains("could not be determined"),
        "the empty label is explained, not invented: {message}"
    );

    // An explicitly empty `peri.mcp` origin is unknown too, not a server named
    // the empty string.
    let fixture = harness(vec![tool("server-a", json!("always"))]).await;
    open_turn(&fixture.log).await;
    fixture
        .log
        .begin_permission(
            &json!(2),
            &json!({
                "_meta": { "peri.mcp": { "server": "", "tool": "" } },
                "toolCall": {
                    "toolCallId": "call-empty",
                    "title": "",
                    "rawInput": {},
                },
            }),
        )
        .await
        .expect("fail-closed pending");
    let required = fixture.log.required_actions();
    assert_eq!(required[0]["request"]["server_label"], "");
    assert!(required[0]["request"]["name"].is_null());
}
