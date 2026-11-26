//! HTTP-surface regression: `sessions.delete` on a `type=none` session.
//!
//! `type=none` is the default shape (WeBots secretaries) and its
//! `environment_json` carries no `id`. The binding-lock lookup therefore returns
//! **one row with a SQL NULL**; reading that row as a non-null `String` failed
//! the delete transaction before any row was removed. The store-level half of
//! this regression lives in `store/postgres/contract_test.rs`; this test drives
//! the real axum router over the real PostgreSQL store and asserts the official
//! `DELETE` → `GET 404` sequence.
//!
//! Set `PERI_SWARM_SESSION_STORE` and `PERI_SWARM_STATE_STORE` to PostgreSQL DSNs
//! of two databases; without them the test prints one skip line and returns.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{EnvironmentRef, InputState, ItemKind, ItemSource, SessionState};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::SwarmService;
use peri_swarm::store::{
    SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmStateStore, SwarmStore,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use crate::state_store_support::{required_env, state_store_config};

const TOKEN: &str = "test-deployment-key";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires two PostgreSQL databases"]
async fn deleting_a_none_session_succeeds_and_the_session_becomes_404() {
    let Some((_session_store_dsn, state_store_dsn)) = required_env() else {
        return;
    };
    let store = Arc::new(
        SwarmStateStore::open(&state_store_config(&state_store_dsn))
            .await
            .expect("open the independent swarm state store"),
    );

    let logical_id = format!("none-delete-{}", Uuid::new_v4());
    let now = "2026-11-24T00:00:00Z";
    store
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: logical_id.clone(),
            root_session_id: format!("root-{logical_id}"),
            actor_key: format!("agent:{logical_id}"),
            cwd: "/workspace/none".to_owned(),
            state: SessionState::Idle,
            model: Some("stub-model".to_owned()),
            instructions: None,
            mcp_tools: Vec::new(),
            environment: EnvironmentRef::none(),
            metadata: BTreeMap::from([("tenant".to_owned(), "beta".to_owned())]),
            prompt_cache_options: None,
            created_at: now.to_owned(),
            last_activity_at: now.to_owned(),
            last_item_seq: 1,
        })
        .await
        .expect("seed a `type=none` session");
    store
        .swarm_append_item(SwarmItemRow {
            logical_id: logical_id.clone(),
            seq: 1,
            item_id: "item-1".to_owned(),
            kind: ItemKind::Message,
            source: ItemSource::root(),
            payload: json!({ "text": "hi" }),
            created_at: now.to_owned(),
            truncated: false,
        })
        .await
        .expect("seed an item");
    store
        .swarm_put_input(SwarmInputRow {
            logical_id: logical_id.clone(),
            input_id: Uuid::new_v4().to_string(),
            fingerprint: "fp".to_owned(),
            state: InputState::Accepted,
            created_at: now.to_owned(),
        })
        .await
        .expect("seed a receipt");

    let service: Arc<dyn SwarmService> = Arc::new(AcpSwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ActorServiceConfig::default(),
    ));
    let workspace = tempfile::tempdir().expect("workspace root");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config =
        Arc::new(SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config"));
    let router = build_router(AppState::new(service, config));

    let deleted = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/agents/sessions/{logical_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .expect("delete request"),
        )
        .await
        .expect("delete response");
    assert_eq!(
        deleted.status(),
        StatusCode::OK,
        "a `type=none` session deletes through the official route"
    );
    let body = axum::body::to_bytes(deleted.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["deleted"], json!(true));

    let read = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/agents/sessions/{logical_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .expect("get request"),
        )
        .await
        .expect("get response");
    assert_eq!(
        read.status(),
        StatusCode::NOT_FOUND,
        "the deleted session is gone, not a partial row"
    );

    assert!(store
        .swarm_get_session(&logical_id)
        .await
        .expect("read")
        .is_none());
    store.close().await.expect("close");
}
