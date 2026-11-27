use std::collections::{HashMap, HashSet};
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
async fn default_items_order_is_desc() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        seqs(&page),
        [10, 9],
        "official ItemListParams defaults to desc"
    );
    assert_eq!(page["order"], json!("desc"));
    assert_eq!(page["nextCursor"], json!(9));
    assert_eq!(page["hasMore"], json!(true));
}

#[tokio::test]
async fn explicit_ascending_pages_cover_every_item_once() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
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
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&after=2",
        None,
    )
    .await;
    assert_eq!(seqs(&second), [3, 4]);
    assert_eq!(second["nextCursor"], json!(4));

    let (_, _, third) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&afterSeq=8",
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
            Some(cursor) => {
                format!("/v1/agents/sessions/agent-01/items?limit=2&order=asc&afterSeq={cursor}")
            }
            None => "/v1/agents/sessions/agent-01/items?limit=2&order=asc".to_owned(),
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
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
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
        "/v1/agents/sessions/agent-01/items?order=asc&after=2&afterSeq=2",
        None,
    )
    .await;
    assert_eq!(seqs(&agreed), [3, 4, 5, 6, 7, 8, 9, 10]);

    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?order=asc&after=2&afterSeq=3",
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
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
        None,
    )
    .await;
    assert_eq!(seqs(&first), [1, 2]);
    let user_id = first["items"][0]["id"].as_str().unwrap();
    let assistant_id = first["items"][1]["id"].as_str().unwrap();
    let turn_id = peri_swarm::domain::turn_id_for_item(user_id);
    assert_eq!(data_ids(&first), [user_id, assistant_id]);
    assert_eq!(
        first["data"],
        json!([
            {"type":"message", "id":user_id, "turn_id":turn_id, "role":"user", "phase":null,
             "status":"completed", "content":[{"type":"input_text", "text":"message 1"}]},
            {"type":"message", "id":assistant_id, "turn_id":turn_id, "role":"assistant", "phase":"final_answer",
             "status":"completed", "content":[{"type":"output_text", "text":"fake-echo: message 1"}]}
        ])
    );
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
    let uri = format!("/v1/agents/sessions/agent-01/items?limit=2&order=asc&after={cursor}");
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
    let uri = format!(
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&after={user_cursor}&afterSeq=1"
    );
    let (_, _, agreed) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(seqs(&agreed), [2, 3]);

    let uri = format!(
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&after={user_cursor}&afterSeq=2"
    );
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
        "/v1/agents/sessions/agent-01/items?order=asc&afterSeq=99",
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

async fn append_row(harness: &Harness, seq: u64, item_id: &str, role: &str, extra: Value) {
    let mut payload = json!({ "role": role, "text": format!("{role}-{seq}") });
    if let Some(object) = extra.as_object() {
        for (key, value) in object {
            payload[key] = value.clone();
        }
    }
    append_item(harness, seq, item_id, ItemKind::Message, payload).await;
}

async fn append_item(harness: &Harness, seq: u64, item_id: &str, kind: ItemKind, payload: Value) {
    harness
        .store
        .swarm_append_item(SwarmItemRow {
            logical_id: "agent-01".to_owned(),
            seq,
            item_id: item_id.to_owned(),
            kind,
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
    // 历史形状：一个开 turn 的用户条目，三帧长消息，再一个新 turn。
    append_row(harness, 1, "itim_user_1", "user", json!({})).await;
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
        json!({"delta": true}),
    )
    .await;
    append_row(
        harness,
        4,
        "itim_msg_long",
        "assistant",
        json!({"complete": true}),
    )
    .await;
    append_row(harness, 5, "itim_user_2", "user", json!({})).await;
    append_row(
        harness,
        6,
        "itim_msg_2",
        "assistant",
        json!({"delta": true}),
    )
    .await;
    append_row(
        harness,
        7,
        "itim_msg_2",
        "assistant",
        json!({"complete": true}),
    )
    .await;
}

fn data_ids(value: &Value) -> Vec<String> {
    let rows = value["items"].as_array().expect("items");
    let raw_ids: HashSet<_> = rows
        .iter()
        .map(|row| row["itemId"].as_str().unwrap())
        .collect();
    let data = value["data"].as_array().expect("data");
    let ids: Vec<String> = data
        .iter()
        .map(|item| {
            assert!(matches!(
                item["type"].as_str(),
                Some("message" | "reasoning" | "function_call" | "function_call_output")
            ));
            assert!(item["turn_id"].is_string());
            let id = item["id"].as_str().expect("id");
            assert!(raw_ids.contains(id));
            id.to_owned()
        })
        .collect();
    // `data` 每个 itemId 至多一项；具体顺序与内容仍由各用例断言。
    let unique: HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "data repeats an itemId: {ids:?}");
    ids
}

#[tokio::test]
async fn item_grouped_pages_keep_data_shorter_than_items() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let (status, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(seqs(&first), [1, 2, 3, 4]);
    assert_eq!(data_ids(&first), ["itim_user_1", "itim_msg_long"]);
    assert_eq!(
        first["data"][1]["content"],
        json!([{"type":"output_text","text":"assistant-4"}])
    );
    assert_eq!(first["data"][1]["turn_id"], first["data"][0]["turn_id"]);
    assert_eq!(first["nextCursor"], json!(4));
    assert_eq!(first["has_more"], json!(true));
    assert_eq!(first["lastItemSeq"], json!(7));

    let (status, _, second) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&afterSeq=4",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(seqs(&second), [5, 6, 7]);
    assert_eq!(data_ids(&second), ["itim_user_2", "itim_msg_2"]);
    assert_eq!(second["data"][1]["content"][0]["text"], "assistant-7");
    assert_ne!(second["data"][1]["turn_id"], first["data"][1]["turn_id"]);
    assert_eq!(second["has_more"], json!(false));
    assert_eq!(second["nextCursor"], json!(null));
}

/// `limit` is also the aggregate row budget: a long first item fills the page,
/// and the next item waits for the following page. `has_more` still counts
/// items.
#[tokio::test]
async fn row_budget_stops_before_the_next_item() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    for seq in 1..=3 {
        append_row(
            &harness,
            seq,
            "itim_msg_a",
            "assistant",
            json!({"delta": true}),
        )
        .await;
    }
    append_row(
        &harness,
        4,
        "itim_msg_b",
        "assistant",
        json!({"complete": true}),
    )
    .await;

    let (_, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
        None,
    )
    .await;
    assert_eq!(seqs(&first), [1, 2, 3], "only the long item fits");
    assert!(
        data_ids(&first).is_empty(),
        "没有 opener 的 assistant 不可投影"
    );
    assert_eq!(first["nextCursor"], json!(3));
    assert_eq!(first["has_more"], json!(true), "the next item still exists");

    let (_, _, second) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&afterSeq=3",
        None,
    )
    .await;
    assert_eq!(seqs(&second), [4]);
    assert!(data_ids(&second).is_empty());
    assert_eq!(second["has_more"], json!(false));
    assert_eq!(second["nextCursor"], json!(null));
}

/// `after=<itemId>` is resolved to that item's `MAX(seq)` **at request time**.
/// Frames appended after the first page are therefore behind the cursor and are
/// not returned by it; resuming from the last row `seq` (`afterSeq`) still
/// re-delivers the item, whose final row is then visible.
#[tokio::test]
async fn item_id_cursor_does_not_cover_frames_appended_later() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    append_row(
        &harness,
        1,
        "itim_msg_a",
        "assistant",
        json!({"delta": true}),
    )
    .await;
    append_row(
        &harness,
        2,
        "itim_msg_a",
        "assistant",
        json!({"delta": true}),
    )
    .await;

    let (_, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc",
        None,
    )
    .await;
    assert_eq!(seqs(&first), [1, 2]);
    let cursor = first["items"][1]["id"].as_str().expect("id").to_owned();

    // The message finishes after the page was read.
    append_row(
        &harness,
        3,
        "itim_msg_a",
        "assistant",
        json!({"complete": true}),
    )
    .await;

    let uri = format!("/v1/agents/sessions/agent-01/items?limit=2&order=asc&after={cursor}");
    let (status, _, after_id) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{after_id}");
    assert!(
        seqs(&after_id).is_empty(),
        "the cursor now names seq 3, so the appended frame is behind it: {after_id}"
    );

    // Resuming from the last rendered row seq still re-delivers the item.
    let (_, _, by_seq) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=asc&afterSeq=2",
        None,
    )
    .await;
    assert_eq!(seqs(&by_seq), [1, 2, 3]);
    assert_eq!(
        by_seq["items"][2]["payload"]["complete"], true,
        "the final frame is visible"
    );
    assert!(data_ids(&by_seq).is_empty());
}

#[tokio::test]
async fn ascending_item_pages_advance_strictly_and_terminate() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let mut cursors = Vec::new();
    let mut seen = Vec::new();
    let mut after = 0;
    for _ in 0..10 {
        let uri = format!("/v1/agents/sessions/agent-01/items?limit=1&order=asc&after={after}");
        let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let rows = page["items"].as_array().expect("items");
        let id = rows.first().expect("nonempty page")["itemId"]
            .as_str()
            .unwrap();
        assert!(rows.iter().all(|row| row["itemId"] == id));
        seen.push(id.to_owned());
        if page["has_more"] == false {
            assert!(page["nextCursor"].is_null());
            break;
        }
        let next = page["nextCursor"].as_u64().expect("nextCursor");
        assert_eq!(Some(next), seqs(&page).into_iter().max());
        assert!(next > after);
        cursors.push(next);
        after = next;
    }
    assert_eq!(
        seen,
        ["itim_user_1", "itim_msg_long", "itim_user_2", "itim_msg_2"]
    );
    assert_eq!(cursors, [1, 4, 5], "the cursor is the item's greatest seq");
}

#[tokio::test]
async fn descending_item_pages_advance_strictly_and_terminate() {
    let harness = harness();
    seed_streamed_items(&harness).await;

    let mut after = None;
    for (id, expected_rows, expected_next) in [
        ("itim_msg_2", &[7, 6][..], Some(7)),
        ("itim_user_2", &[5][..], Some(5)),
        ("itim_msg_long", &[4, 3, 2][..], Some(4)),
        ("itim_user_1", &[1][..], None),
    ] {
        let mut uri = "/v1/agents/sessions/agent-01/items?limit=1&order=desc".to_owned();
        if let Some(seq) = after {
            uri.push_str(&format!("&after={seq}"));
        }
        let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(seqs(&page), expected_rows);
        assert_eq!(data_ids(&page), [id]);
        assert_eq!(page["has_more"], expected_next.is_some());
        assert_eq!(page["nextCursor"], json!(expected_next));
        after = expected_next;
    }
    let (_, _, full) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?order=desc",
        None,
    )
    .await;
    assert_eq!(
        data_ids(&full),
        ["itim_msg_2", "itim_user_2", "itim_msg_long", "itim_user_1"]
    );
}

#[tokio::test]
async fn internal_only_page_keeps_items_and_next_cursor() {
    let harness = harness();
    let (status, _, _) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({"logicalId":"agent-01"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    append_row(&harness, 1, "itim_opener", "user", json!({})).await;
    append_item(
        &harness,
        2,
        "itim_receipt",
        ItemKind::Receipt,
        json!({"_raw":true}),
    )
    .await;
    append_item(
        &harness,
        3,
        "itim_approval",
        ItemKind::Approval,
        json!({"state":"pending"}),
    )
    .await;
    append_row(
        &harness,
        4,
        "itim_answer",
        "assistant",
        json!({"complete":true}),
    )
    .await;
    let (status, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?order=asc&limit=2&after=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [2, 3]);
    assert_eq!(page["items"][0]["kind"], "receipt");
    assert_eq!(page["items"][1]["kind"], "approval");
    assert_eq!(page["data"], json!([]));
    assert_eq!(page["has_more"], true);
    assert_eq!(page["nextCursor"], 3);
    let uri = format!(
        "/v1/agents/sessions/agent-01/items?order=asc&limit=2&after={}",
        page["nextCursor"]
    );
    let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [4]);
    assert_eq!(data_ids(&page), ["itim_answer"]);
    assert_eq!(page["has_more"], false);
}

fn item_ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|row| row["itemId"].as_str().unwrap().to_owned())
        .collect()
}

/// Count how often one raw item id is delivered across the walked pages.
fn seen(pages: &[Value], item_id: &str) -> usize {
    pages
        .iter()
        .map(|page| {
            item_ids(page)
                .iter()
                .filter(|id| id.as_str() == item_id)
                .count()
        })
        .sum()
}

/// Walk `/items` exactly like the official `CursorPage`: the next cursor is
/// `data[last].id`, and an empty `data` page (or `has_more=false`) ends the run.
async fn sdk_pages(harness: &Harness, order: &str, limit: u32) -> Vec<Value> {
    let mut pages = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let uri = match after.as_deref() {
            Some(id) => {
                format!("/v1/agents/sessions/agent-01/items?order={order}&limit={limit}&after={id}")
            }
            None => format!("/v1/agents/sessions/agent-01/items?order={order}&limit={limit}"),
        };
        let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let next = page["data"]
            .as_array()
            .and_then(|data| data.last())
            .map(|item| item["id"].as_str().unwrap().to_owned());
        let has_more = page["hasMore"].as_bool().unwrap();
        pages.push(page);
        match next {
            Some(id) if has_more => after = Some(id),
            _ => break,
        }
    }
    pages
}

/// 官方 SDK 的自动翻页游标是 `data[last].id`（网关解析为该 item 的 `MAX(seq)`），
/// 不是 `nextCursor`。desc 页里排在最后一个可映射条目**之前**（更新）的未映射
/// 条目落在游标的已越过侧，下一页不会再返回它；排在它**之后**（更旧）的会在
/// 下一页重读。需要全量原始条目时用 `nextCursor`/`afterSeq` 翻页并读 `items`。
#[tokio::test]
async fn sdk_data_cursor_skips_newer_unmapped_items_in_desc() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    append_row(&harness, 1, "itim_u1", "user", json!({})).await;
    append_item(
        &harness,
        2,
        "itim_r2",
        ItemKind::Receipt,
        json!({"state": "delivered"}),
    )
    .await;
    append_row(
        &harness,
        3,
        "itim_a3",
        "assistant",
        json!({"complete": true}),
    )
    .await;
    append_item(
        &harness,
        4,
        "itim_r4",
        ItemKind::Receipt,
        json!({"state": "delivered"}),
    )
    .await;
    append_row(&harness, 5, "itim_u5", "user", json!({})).await;
    append_item(
        &harness,
        6,
        "itim_r6",
        ItemKind::Receipt,
        json!({"state": "delivered"}),
    )
    .await;

    let pages = sdk_pages(&harness, "desc", 3).await;
    assert_eq!(
        pages.iter().map(item_ids).collect::<Vec<_>>(),
        vec![
            vec!["itim_r6", "itim_u5", "itim_r4"],
            vec!["itim_r4", "itim_a3", "itim_r2"],
            vec!["itim_r2", "itim_u1"],
        ]
    );
    assert_eq!(
        pages.iter().map(data_ids).collect::<Vec<_>>(),
        vec![vec!["itim_u5"], vec!["itim_a3"], vec!["itim_u1"]],
        "SDK 游标取 data 末项：页里更旧的未映射条目不在其中"
    );
    assert_eq!(
        seen(&pages, "itim_r6"),
        1,
        "desc：排在最后一个可映射条目之前（更新），下一页不再返回"
    );
    assert_eq!(
        seen(&pages, "itim_r4"),
        2,
        "desc：排在它之后（更旧），下一页重读"
    );
}

/// asc 下的镜像：比 `data` 末项更旧的未映射条目只在这一页出现，更新的会重读。
#[tokio::test]
async fn sdk_data_cursor_skips_older_unmapped_items_in_asc() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    append_item(
        &harness,
        1,
        "itim_r1",
        ItemKind::Receipt,
        json!({"state": "delivered"}),
    )
    .await;
    append_row(&harness, 2, "itim_u2", "user", json!({})).await;
    append_row(
        &harness,
        3,
        "itim_a3",
        "assistant",
        json!({"complete": true}),
    )
    .await;
    append_item(
        &harness,
        4,
        "itim_r4",
        ItemKind::Receipt,
        json!({"state": "delivered"}),
    )
    .await;
    append_row(&harness, 5, "itim_u5", "user", json!({})).await;

    let pages = sdk_pages(&harness, "asc", 2).await;
    assert_eq!(
        pages.iter().map(item_ids).collect::<Vec<_>>(),
        vec![
            vec!["itim_r1", "itim_u2"],
            vec!["itim_a3", "itim_r4"],
            vec!["itim_r4", "itim_u5"],
        ]
    );
    assert_eq!(
        pages.iter().map(data_ids).collect::<Vec<_>>(),
        vec![vec!["itim_u2"], vec!["itim_a3"], vec!["itim_u5"],]
    );
    assert_eq!(
        seen(&pages, "itim_r1"),
        1,
        "asc：排在最后一个可映射条目之前（更旧），下一页不再返回"
    );
    assert_eq!(
        seen(&pages, "itim_r4"),
        2,
        "asc：排在它之后（更新），下一页重读"
    );
}
