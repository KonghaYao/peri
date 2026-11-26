use serde_json::{json, Value};

use super::*;
use crate::domain::{ItemKind, ItemSource};
use crate::store::{MemorySwarmStore, SwarmItemRow, SwarmTurnRow};

async fn append(
    store: &MemorySwarmStore,
    seq: u64,
    id: &str,
    kind: ItemKind,
    payload: Value,
    truncated: bool,
) {
    store
        .swarm_append_item(SwarmItemRow {
            logical_id: "agent-01".into(),
            seq,
            item_id: id.into(),
            kind,
            source: ItemSource::root(),
            payload,
            created_at: "2026-01-01T00:00:00Z".into(),
            truncated,
        })
        .await
        .unwrap();
}

async fn user(store: &MemorySwarmStore, seq: u64, id: &str) {
    append(
        store,
        seq,
        id,
        ItemKind::Message,
        json!({"role":"user","text":id}),
        false,
    )
    .await;
}

async fn turn(store: &MemorySwarmStore, opener: &str, status: TurnStatus) {
    store
        .swarm_upsert_turn(SwarmTurnRow {
            logical_id: "agent-01".into(),
            turn_id: turn_id_for_item(opener),
            status,
            created_at: 1,
            started_at: Some(1),
            completed_at: (!status.is_open()).then_some(2),
            error: None,
            usage: None,
            subagent_id: None,
        })
        .await
        .unwrap();
}

async fn page(store: &MemorySwarmStore, order: ItemOrder, after: Option<u64>, limit: u32) -> Value {
    let page = item_page(
        store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order,
            after,
            after_id: None,
            limit,
        },
        900,
    )
    .await
    .unwrap();
    serde_json::to_value(page).unwrap()
}

#[tokio::test]
async fn mid_turn_pages_keep_steer_and_tool_calls_on_the_original_turn() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_open").await;
    user(&store, 2, "itim_steer").await;
    append(
        &store,
        3,
        "itim_delta",
        ItemKind::Message,
        json!({"role":"assistant","text":"half"}),
        false,
    )
    .await;
    append(
        &store,
        4,
        "itim_call",
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1","title":"tool","status":"pending"}),
        false,
    )
    .await;
    append(
        &store,
        5,
        "itim_result",
        ItemKind::ToolResult,
        json!({"toolCallId":"call-1","status":"failed","output":"boom"}),
        false,
    )
    .await;
    turn(&store, "itim_open", TurnStatus::Cancelled).await;
    for (order, after, expected) in [
        (
            ItemOrder::Asc,
            Some(1),
            ["itim_steer", "itim_delta", "itim_call", "itim_result"],
        ),
        (
            ItemOrder::Desc,
            None,
            ["itim_result", "itim_call", "itim_delta", "itim_steer"],
        ),
    ] {
        let page = page(&store, order, after, 4).await;
        let data = page["data"].as_array().unwrap();
        let ids: Vec<_> = data
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, expected);
        assert!(data
            .iter()
            .all(|item| item["turn_id"] == turn_id_for_item("itim_open")));
        assert_eq!(
            data.iter().find(|i| i["id"] == "itim_delta").unwrap()["status"],
            "incomplete"
        );
        let call = data.iter().find(|i| i["type"] == "function_call").unwrap();
        assert_eq!(call["call_id"], "call-1");
        assert_eq!(call["status"], "failed", "同页输出覆盖调用状态");
        assert_eq!(page["has_more"], order == ItemOrder::Desc);
        assert_eq!(
            page["nextCursor"],
            if order == ItemOrder::Desc {
                json!(2)
            } else {
                Value::Null
            }
        );
    }
    let call_page = page(&store, ItemOrder::Asc, Some(3), 1).await;
    let output_page = page(&store, ItemOrder::Asc, Some(4), 1).await;
    assert_eq!(
        call_page["data"][0]["call_id"],
        output_page["data"][0]["call_id"]
    );
    assert_eq!(
        call_page["data"][0]["status"], "in_progress",
        "跨页时不回填"
    );
}

#[tokio::test]
async fn both_output_boundary_and_persisted_opener_start_a_new_turn() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_first").await;
    append(
        &store,
        2,
        "itim_hidden_complete",
        ItemKind::Message,
        json!({"role":"assistant","complete":true}),
        true,
    )
    .await;
    user(&store, 3, "itim_after_output").await;
    user(&store, 4, "itim_after_restart").await;
    turn(&store, "itim_after_restart", TurnStatus::InProgress).await;
    append(
        &store,
        5,
        "itim_next",
        ItemKind::Message,
        json!({"role":"assistant","complete":true,"text":"done"}),
        false,
    )
    .await;
    let page = page(&store, ItemOrder::Asc, None, 5).await;
    let ids: Vec<_> = page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["turn_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "itim_first",
            "itim_after_output",
            "itim_after_restart",
            "itim_after_restart"
        ]
        .map(turn_id_for_item)
    );
    assert_eq!(page["items"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn orphan_and_internal_rows_do_not_change_the_raw_page_boundary() {
    let store = MemorySwarmStore::new();
    append(
        &store,
        1,
        "itim_orphan",
        ItemKind::Message,
        json!({"role":"assistant","complete":true}),
        false,
    )
    .await;
    append(
        &store,
        2,
        "itim_tool",
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1","title":"tool"}),
        false,
    )
    .await;
    append(
        &store,
        3,
        "itim_receipt",
        ItemKind::Receipt,
        json!({"_raw":true}),
        false,
    )
    .await;
    user(&store, 4, "itim_open").await;
    let first = page(&store, ItemOrder::Asc, None, 3).await;
    assert_eq!(first["data"], json!([]));
    assert_eq!(first["items"].as_array().unwrap().len(), 3);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["nextCursor"], 3);
    assert_eq!(first["lastItemSeq"], 900);
    let second = page(&store, ItemOrder::Asc, first["nextCursor"].as_u64(), 3).await;
    assert_eq!(second["data"][0]["turn_id"], turn_id_for_item("itim_open"));
    assert_eq!(second["has_more"], false);
    assert_eq!(second["nextCursor"], Value::Null);
}

#[tokio::test]
async fn an_opener_between_legacy_frames_still_counts_when_omitted_from_the_page() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_old").await;
    append(
        &store,
        2,
        "itim_answer",
        ItemKind::Message,
        json!({"role":"assistant","delta":true,"text":"early"}),
        false,
    )
    .await;
    user(&store, 3, "itim_resumed").await;
    turn(&store, "itim_resumed", TurnStatus::InProgress).await;
    append(
        &store,
        4,
        "itim_answer",
        ItemKind::Message,
        json!({"role":"assistant","complete":true,"text":"final"}),
        false,
    )
    .await;
    let page = page(&store, ItemOrder::Desc, None, 1).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["seq"], 4);
    assert_eq!(page["items"][1]["seq"], 2);
    assert_eq!(page["nextCursor"], 4);
    assert_eq!(page["data"][0]["turn_id"], turn_id_for_item("itim_resumed"));
    assert_eq!(page["data"][0]["content"][0]["text"], "final");
}

#[tokio::test]
async fn multi_page_lookback_matches_replay_classification() {
    use crate::service::{classify_row, ReplayTurn, RowFrame};

    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_open").await;
    let last_steer = u64::from(ItemsWindow::MAX_LIMIT) + 2;
    for seq in 2..=last_steer {
        user(&store, seq, &format!("itim_steer_{seq}")).await;
    }
    append(
        &store,
        last_steer + 1,
        "itim_answer",
        ItemKind::Message,
        json!({"role":"assistant","complete":true}),
        false,
    )
    .await;

    let mut turns: Vec<ReplayTurn> = Vec::new();
    let mut current = None;
    let persisted = BTreeMap::new();
    let rows = store
        .swarm_items_after("agent-01", 0, (last_steer + 1) as u32)
        .await
        .unwrap();
    let mut replay_turn = None;
    for row in rows {
        if let RowFrame::Assistant { turn, .. } =
            classify_row(&row.to_view(), &mut turns, &mut current, &persisted)
        {
            replay_turn = Some(turn);
        }
    }
    let replay_id = &turns[replay_turn.expect("replayed assistant")].turn_id;
    let desc = page(&store, ItemOrder::Desc, None, 2).await;
    assert_eq!(desc["data"][0]["id"], "itim_answer");
    assert_eq!(desc["data"][0]["turn_id"], *replay_id);
    assert_eq!(desc["data"][1]["turn_id"], *replay_id);
}
