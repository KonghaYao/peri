use peri_swarm::actor::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::domain::{turn_id_for_item, EnvironmentRef, TurnStatus};

use super::*;

const SESSION: &str = "/v1/agents/sessions/agent-cancel";
const EVENTS: &str = "/v1/agents/sessions/agent-cancel/events";
const CANCEL_ID: &str = "0192f0c5-0000-7000-8000-000000000071";

async fn get(router: &Router, suffix: &str) -> Value {
    let (status, _, body) = send(
        router,
        Method::GET,
        &format!("{SESSION}{suffix}"),
        None,
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn submit_cancel(router: &Router, key: &str) -> (StatusCode, Value) {
    let (status, _, body) = send(
        router,
        Method::POST,
        EVENTS,
        Some(json!({"events": [{"type": "agent.session.input.cancel"}]})),
        Some(TOKEN),
        &[("idempotency-key", key)],
    )
    .await;
    (status, body)
}

/// `Manual` 让 fake 保持一个 live turn，用来复现 actor 的“有 slot 才能取消”。
///
/// 它不代表 `Immediate`（默认策略）：那里 turn 在受理内同步完成，随后的 cancel
/// 看到的是空 slot，只能作为 202 no-op。
fn manual_harness() -> Harness {
    let service = Arc::new(InMemorySwarmService::in_memory(ServiceConfig {
        turn_policy: TurnPolicy::Manual,
        ..ServiceConfig::default()
    }));
    let (router, workspace) = harness_with_service(service, 15_000);
    Harness { router, workspace }
}

async fn start_turn(router: &Router, index: u8) -> String {
    let (status, _, body) = send(router, Method::POST, EVENTS,
        Some(json!({"inputId": format!("0192f0c5-0000-7000-8000-0000000000{index:02x}"), "text": "hold"})),
        Some(TOKEN), &[],
    ).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let items = get(router, "/items?order=desc").await;
    let opener = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["payload"]["role"] == "user")
        .unwrap();
    let turn_id = turn_id_for_item(opener["itemId"].as_str().unwrap());
    assert_eq!(
        get(router, &format!("/turns/{turn_id}")).await["status"],
        "in_progress"
    );
    turn_id
}

#[tokio::test]
async fn cancel_closes_the_active_turn_without_creating_an_item_and_retry_spares_the_next_turn() {
    let harness = manual_harness();
    let (status, _) = create_session(&harness, json!({"logicalId": "agent-cancel"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let router = &harness.router;
    let first_turn = start_turn(router, 1).await;
    assert_eq!(get(router, "").await["status"], "in_progress");
    let before = get(router, "/items").await;
    let (status, accepted) = submit_cancel(router, CANCEL_ID).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(
        accepted,
        json!({"inputId": CANCEL_ID, "accepted": true, "disposition": "inserted",
        "mode": "turn", "lastItemSeq": before["lastItemSeq"]})
    );
    assert_eq!(get(router, "").await["status"], "idle");
    let cancelled = get(router, &format!("/turns/{first_turn}")).await;
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["completed_at"].is_number());
    assert_eq!(get(router, "/items").await["items"], before["items"]);
    assert_eq!(
        get(router, "/turns").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let second_turn = start_turn(router, 2).await;
    assert_ne!(second_turn, first_turn);
    let before_retry = get(router, "/items").await;
    let (status, replay) = submit_cancel(router, CANCEL_ID).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{replay}");
    assert_eq!(replay["disposition"], "existing");
    assert_eq!(replay["inputId"], CANCEL_ID);
    assert_eq!(get(router, "").await["status"], "in_progress");
    assert_eq!(
        get(router, &format!("/turns/{second_turn}")).await["status"],
        "in_progress"
    );
    assert_eq!(get(router, "/items").await["items"], before_retry["items"]);
}

#[tokio::test]
async fn idle_cancel_is_a_receipted_no_op() {
    let harness = manual_harness();
    let (status, _) = create_session(&harness, json!({"logicalId": "agent-cancel"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, accepted) = submit_cancel(&harness.router, CANCEL_ID).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(accepted["disposition"], "inserted");
    assert_eq!(get(&harness.router, "").await["status"], "idle");
    assert_eq!(get(&harness.router, "/items").await["items"], json!([]));
    assert_eq!(get(&harness.router, "/turns").await["data"], json!([]));
}

#[tokio::test]
async fn actor_cancel_without_a_host_refuses_a_persisted_open_turn_before_receipting() {
    let store = Arc::new(MemorySwarmStore::new());
    let service = Arc::new(AcpSwarmService::new(
        store.clone(),
        ActorServiceConfig::default(),
    ));
    let (router, workspace) = harness_with_service(service, 15_000);
    let id = "agent-cancel";
    store
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: id.into(),
            root_session_id: CANCEL_ID.into(),
            actor_key: format!("agent/{id}"),
            cwd: workspace.path().display().to_string(),
            state: SessionState::Idle,
            model: None,
            instructions: None,
            mcp_tools: vec![],
            environment: EnvironmentRef::none(),
            metadata: Default::default(),
            prompt_cache_options: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            last_activity_at: "2026-01-01T00:00:00Z".into(),
            last_item_seq: 0,
        })
        .await
        .unwrap();
    store
        .swarm_upsert_turn(SwarmTurnRow {
            logical_id: id.into(),
            turn_id: "turn_interrupted".into(),
            status: TurnStatus::Waiting,
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            error: None,
            usage: None,
            subagent_id: None,
        })
        .await
        .unwrap();

    let (status, refusal) = submit_cancel(&router, CANCEL_ID).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{refusal}");
    assert_eq!(refusal["error"]["code"], "upstream_unavailable");
    assert_eq!(
        refusal["error"]["details"]["operation"],
        "session_resume_required"
    );
    assert!(store
        .swarm_get_input(id, CANCEL_ID)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        get(&router, "/turns/turn_interrupted").await["status"],
        "waiting"
    );
}

/// fake 与 actor 的取消门禁一致：没有 live turn 但持久 turn 未完成时，
/// `session/cancel` 没有可达目标，不写“已受理”的收据。
#[tokio::test]
async fn in_memory_cancel_without_a_live_turn_refuses_a_persisted_open_turn() {
    let store = Arc::new(MemorySwarmStore::new());
    let service = Arc::new(InMemorySwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ServiceConfig::default(),
    ));
    let (router, workspace) = harness_with_service(service, 15_000);
    let id = "agent-cancel";
    store
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: id.into(),
            root_session_id: CANCEL_ID.into(),
            actor_key: format!("agent/{id}"),
            cwd: workspace.path().display().to_string(),
            state: SessionState::Idle,
            model: None,
            instructions: None,
            mcp_tools: vec![],
            environment: EnvironmentRef::none(),
            metadata: Default::default(),
            prompt_cache_options: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            last_activity_at: "2026-01-01T00:00:00Z".into(),
            last_item_seq: 0,
        })
        .await
        .unwrap();
    store
        .swarm_upsert_turn(SwarmTurnRow {
            logical_id: id.into(),
            turn_id: "turn_interrupted".into(),
            status: TurnStatus::Waiting,
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            error: None,
            usage: None,
            subagent_id: None,
        })
        .await
        .unwrap();

    let (status, refusal) = submit_cancel(&router, CANCEL_ID).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{refusal}");
    assert_eq!(refusal["error"]["code"], "upstream_unavailable");
    assert_eq!(
        refusal["error"]["details"]["operation"],
        "session_resume_required"
    );
    assert!(store
        .swarm_get_input(id, CANCEL_ID)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        get(&router, "/turns/turn_interrupted").await["status"],
        "waiting"
    );
}
