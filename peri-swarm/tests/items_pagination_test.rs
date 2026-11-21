use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{ItemKind, ItemSource};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceConfig};
use peri_swarm::store::{MemorySwarmStore, SwarmItemRow, SwarmStore};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

struct Harness {
    router: Router,
    store: Arc<MemorySwarmStore>,
    _workspace: TempDir,
}

fn harness() -> Harness {
    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let store = Arc::new(MemorySwarmStore::new());
    let service = Arc::new(InMemorySwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ServiceConfig::default(),
    ));
    let router = build_router(AppState::new(service, Arc::new(config)));
    Harness {
        router,
        store,
        _workspace: workspace,
    }
}

async fn send(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

async fn seed_five_items(harness: &Harness) {
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    for index in 1..=5 {
        let (status, _, value) = send(
            &harness.router,
            Method::POST,
            "/v1/agents/sessions/agent-01/events",
            Some(json!({
                "inputId": uuid::Uuid::now_v7().to_string(),
                "text": format!("message {index}")
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    }
}

fn seqs(value: &Value) -> Vec<u64> {
    value["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["seq"].as_u64().expect("seq"))
        .collect()
}

#[tokio::test]
async fn ascending_pages_cover_every_item_once() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["object"], json!("list"));
    assert_eq!(first["logicalId"], json!("agent-01"));
    assert_eq!(seqs(&first), [1, 2]);
    assert_eq!(first["nextCursor"], json!(2));
    assert_eq!(first["hasMore"], json!(true));
    // Five turns, each appending a user message and a fake assistant reply.
    assert_eq!(first["lastItemSeq"], json!(10));
    assert_eq!(first["order"], json!("asc"));

    let (_, _, second) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&after=2",
        None,
    )
    .await;
    assert_eq!(seqs(&second), [3, 4]);
    assert_eq!(second["nextCursor"], json!(4));

    let (_, _, third) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&afterSeq=8",
        None,
    )
    .await;
    assert_eq!(seqs(&third), [9, 10]);
    assert_eq!(third["nextCursor"], json!(null));
    assert_eq!(third["hasMore"], json!(false));
    assert_eq!(third["lastItemSeq"], json!(10));

    let mut collected = Vec::new();
    let mut cursor: Option<u64> = None;
    loop {
        let uri = match cursor {
            Some(cursor) => format!("/v1/agents/sessions/agent-01/items?limit=2&afterSeq={cursor}"),
            None => "/v1/agents/sessions/agent-01/items?limit=2".to_owned(),
        };
        let (_, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        collected.extend(seqs(&page));
        match page["nextCursor"].as_u64() {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
}

#[tokio::test]
async fn descending_pages_walk_backwards_with_a_stable_cursor() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, newest) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{newest}");
    assert_eq!(seqs(&newest), [10, 9]);
    assert_eq!(newest["nextCursor"], json!(9));
    assert_eq!(newest["hasMore"], json!(true));
    assert_eq!(newest["order"], json!("desc"));

    let (_, _, older) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc&after=9",
        None,
    )
    .await;
    assert_eq!(seqs(&older), [8, 7]);

    let (_, _, oldest) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc&after=3",
        None,
    )
    .await;
    assert_eq!(seqs(&oldest), [2, 1]);
    assert_eq!(oldest["nextCursor"], json!(null));
    assert_eq!(oldest["hasMore"], json!(false));
}

#[tokio::test]
async fn item_shape_carries_stable_identity() {
    let harness = harness();
    seed_five_items(&harness).await;
    let (_, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    let user = &page["items"][0];
    assert_eq!(user["seq"], json!(1));
    assert_eq!(user["id"], user["itemId"]);
    let input_id = user["payload"]["inputId"]
        .as_str()
        .expect("inputId links the user frame");
    assert_eq!(user["itemId"], json!(format!("itim_input_{input_id}")));
    assert_eq!(user["kind"], json!("message"));
    assert_eq!(user["source"]["agentId"], json!("root"));
    assert_eq!(user["source"]["taskId"], json!(null));
    assert_eq!(user["payload"]["role"], json!("user"));
    assert_eq!(user["payload"]["text"], json!("message 1"));
    assert_eq!(user["payload"]["inputId"], json!(input_id));
    assert_eq!(user["payload"]["complete"], json!(null));
    let created_at = user["createdAt"].as_str().expect("createdAt");
    assert!(created_at.contains('T'), "{created_at}");
    assert!(
        created_at.ends_with('Z') || created_at.ends_with("+00:00"),
        "createdAt must be UTC RFC3339: {created_at}"
    );
    assert_eq!(user["truncated"], json!(false));

    let assistant = &page["items"][1];
    assert_eq!(assistant["seq"], json!(2));
    assert_eq!(assistant["kind"], json!("message"));
    assert_eq!(assistant["payload"]["role"], json!("assistant"));
    assert_eq!(assistant["payload"]["text"], json!("fake-echo: message 1"));
    assert!(assistant["payload"]["messageId"].is_string());
}

#[tokio::test]
async fn after_aliases_must_agree_and_limits_are_bounded() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (_, _, agreed) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?after=2&afterSeq=2",
        None,
    )
    .await;
    assert_eq!(seqs(&agreed), [3, 4, 5, 6, 7, 8, 9, 10]);

    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?after=2&afterSeq=3",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("conflicting_cursor"));

    for query in [
        "limit=0",
        "limit=501",
        "limit=abc",
        "order=sideways",
        "bogus=1",
        "after=not-a-cursor",
    ] {
        let uri = format!("/v1/agents/sessions/agent-01/items?{query}");
        let (status, _, value) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(value["error"]["code"], json!("invalid_query"), "{query}");
    }
}

#[tokio::test]
async fn official_cursor_page_uses_data_and_item_id_after() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (_, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    assert_eq!(seqs(&first), [1, 2]);
    assert_eq!(first["data"], first["items"], "CursorPage reads `data`");
    assert_eq!(
        first["has_more"],
        json!(true),
        "CursorPage reads `has_more`"
    );
    let cursor = first["items"][1]["id"]
        .as_str()
        .expect("item id")
        .to_owned();
    assert!(cursor.starts_with("itim_"), "{cursor}");

    // `CursorPage` sends the last item's `id` as the next `after`; it resolves
    // to that row's seq, so the second page continues where the first stopped.
    let uri = format!("/v1/agents/sessions/agent-01/items?limit=2&after={cursor}");
    let (status, _, second) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(seqs(&second), [3, 4]);
    assert_eq!(second["nextCursor"], json!(4));
    assert_eq!(second["has_more"], json!(true));

    // A numeric `afterSeq` must agree with the seq the id resolves to.
    let user_cursor = first["items"][0]["id"]
        .as_str()
        .expect("user id")
        .to_owned();
    let uri = format!("/v1/agents/sessions/agent-01/items?limit=2&after={user_cursor}&afterSeq=1");
    let (_, _, agreed) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(seqs(&agreed), [2, 3]);

    let uri = format!("/v1/agents/sessions/agent-01/items?limit=2&after={user_cursor}&afterSeq=2");
    let (status, _, conflict) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(conflict["error"]["code"], json!("conflicting_cursor"));
}

#[tokio::test]
async fn cursor_beyond_the_tail_is_an_empty_page() {
    let harness = harness();
    seed_five_items(&harness).await;
    let (status, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?afterSeq=99",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), Vec::<u64>::new());
    assert_eq!(page["nextCursor"], json!(null));
    assert_eq!(page["hasMore"], json!(false));
    assert_eq!(page["lastItemSeq"], json!(10));
}

#[tokio::test]
async fn unknown_agent_items_is_404() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/missing/items",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], json!("unknown_agent"));
}

/// Persist one raw row through the store the HTTP service reads. Turn
/// structure is irrelevant here: the pagination contract is about item rows.
async fn append_row(harness: &Harness, seq: u64, item_id: &str, role: &str, extra: Value) {
    let mut payload = json!({ "role": role, "text": format!("{role}-{seq}") });
    if let Some(object) = extra.as_object() {
        for (key, value) in object {
            payload[key] = value.clone();
        }
    }
    harness
        .store
        .swarm_append_item(SwarmItemRow {
            logical_id: "agent-01".to_owned(),
            seq,
            item_id: item_id.to_owned(),
            kind: ItemKind::Message,
            source: ItemSource::root(),
            payload,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            truncated: false,
        })
        .await
        .expect("append");
}

async fn seed_streamed_items(harness: &Harness) {
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    // One long streamed message (three rows), a user item, then a short one.
    append_row(
        harness,
        1,
        "itim_msg_long",
        "assistant",
        json!({"delta": true}),
    )
    .await;
    append_row(
        harness,
        2,
        "itim_msg_long",
        "assistant",
        json!({"delta": true}),
    )
    .await;
    append_row(
        harness,
        3,
        "itim_msg_long",
        "assistant",
        json!({"complete": true}),
    )
    .await;
    append_row(harness, 4, "itim_user_2", "user", json!({})).await;
    append_row(
        harness,
        5,
        "itim_msg_2",
        "assistant",
        json!({"delta": true}),
    )
    .await;
    append_row(
        harness,
        6,
        "itim_msg_2",
        "assistant",
        json!({"complete": true}),
    )
    .await;
}

fn data_ids(value: &Value) -> Vec<String> {
    value["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

fn data_seqs(value: &Value) -> Vec<u64> {
    value["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|item| item["seq"].as_u64().expect("seq"))
        .collect()
}

#[tokio::test]
async fn item_grouped_pages_keep_data_shorter_than_items() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let (status, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    // Two items were selected, so every row of both is in `items`; `data` has
    // one row per item (its last frame).
    assert_eq!(seqs(&first), [1, 2, 3, 4]);
    assert_eq!(data_ids(&first), ["itim_msg_long", "itim_user_2"]);
    assert_eq!(data_seqs(&first), [3, 4]);
    assert_eq!(first["nextCursor"], json!(4));
    assert_eq!(first["has_more"], json!(true));
    assert_eq!(first["lastItemSeq"], json!(6));
}

#[tokio::test]
async fn ascending_item_pages_advance_strictly_and_terminate() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let mut cursors = Vec::new();
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..10 {
        let uri = match after.as_deref() {
            Some(after) => format!("/v1/agents/sessions/agent-01/items?limit=1&after={after}"),
            None => "/v1/agents/sessions/agent-01/items?limit=1".to_owned(),
        };
        let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page["data"].as_array().expect("data").len() <= 1);
        // The long message needs three rows while `limit` is one.
        assert!(!page["items"].as_array().expect("items").is_empty());
        seen.extend(data_ids(&page));
        match page["data"].as_array().and_then(|data| data.last()) {
            Some(item) if page["has_more"] == json!(true) => {
                cursors.push(item["seq"].as_u64().expect("seq"));
                after = Some(item["id"].as_str().expect("id").to_owned());
            }
            _ => break,
        }
    }
    assert_eq!(
        seen,
        ["itim_msg_long", "itim_user_2", "itim_msg_2"],
        "`after=<itemId>` advances past the whole item, never inside it"
    );
    assert_eq!(cursors, [3, 4], "the cursor is the item's greatest seq");
}

#[tokio::test]
async fn descending_item_pages_advance_strictly_and_terminate() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let first = {
        let (status, _, page) = send(
            &harness.router,
            Method::GET,
            "/v1/agents/sessions/agent-01/items?limit=1&order=desc",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        page
    };
    assert_eq!(data_seqs(&first), [6]);
    assert_eq!(seqs(&first), [6, 5], "the page keeps every row of the item");

    // `after` is the item's greatest seq and the next page takes items strictly
    // below it, so the same item cannot reappear.
    let uri = format!(
        "/v1/agents/sessions/agent-01/items?limit=1&order=desc&after={}",
        first["data"][0]["id"].as_str().expect("id")
    );
    let (_, _, second) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(data_ids(&second), ["itim_user_2"]);
    assert_eq!(second["nextCursor"], json!(4));

    let uri = format!(
        "/v1/agents/sessions/agent-01/items?limit=1&order=desc&after={}",
        second["data"][0]["id"].as_str().expect("id")
    );
    let (_, _, third) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(data_ids(&third), ["itim_msg_long"]);
    assert_eq!(seqs(&third), [3, 2, 1]);
    assert_eq!(third["has_more"], json!(false));
    assert_eq!(third["nextCursor"], json!(null));
}
