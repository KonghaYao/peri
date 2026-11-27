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
        call_page["data"][0]["status"], "incomplete",
        "跨页不回填输出状态：turn 已终止时按 incomplete 兜底"
    );
    assert_eq!(output_page["data"][0]["status"], "failed");
}

#[tokio::test]
async fn a_truly_truncated_completion_only_moves_the_boundary_through_its_persisted_turn() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_first").await;
    // 真实截断：`ItemView::fit_payload` 把 payload 换成标记，role/complete 不可见，
    // 所以这个完成帧既进不了 data，也不再是可识别的边界。
    let (marker, truncated) = ItemView::fit_payload(json!({
        "role": "assistant",
        "complete": true,
        "text": "x".repeat(crate::domain::item::MAX_PAYLOAD_BYTES),
    }));
    assert!(truncated);
    assert_eq!(marker["_truncated"], json!(true));
    assert!(marker["originalBytes"].as_u64().is_some());
    append(
        &store,
        2,
        "itim_hidden_complete",
        ItemKind::Message,
        marker,
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
    // 截断行失去边界：第 3 行只是同一个 turn 里的 steer；只有持久 turn 行
    // （第 4 行）能把下一个用户输入判成新 turn。
    assert_eq!(
        ids,
        [
            "itim_first",
            "itim_first",
            "itim_after_restart",
            "itim_after_restart"
        ]
        .map(turn_id_for_item)
    );
    // 截断行仍逐帧出现在 items，只是不进 data。
    assert!(page["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["itemId"] == "itim_hidden_complete"));
    assert!(!page["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == "itim_hidden_complete"));
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

#[tokio::test]
async fn recovered_output_stays_in_items_and_does_not_move_the_turn_boundary() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_open").await;
    turn(&store, "itim_open", TurnStatus::InProgress).await;
    // 恢复期根输出：真实持久行，但不属于任何 gateway turn，也没有 phase。
    append(
        &store,
        2,
        "itim_recovered",
        ItemKind::Message,
        json!({
            "role": "assistant",
            "text": "recovered output",
            "complete": true,
            "recovered": true,
        }),
        false,
    )
    .await;
    user(&store, 3, "itim_steer").await;
    append(
        &store,
        4,
        "itim_final",
        ItemKind::Message,
        json!({"role":"assistant","text":"answer","complete":true}),
        false,
    )
    .await;

    let page = page(&store, ItemOrder::Asc, None, 10).await;
    let data = page["data"].as_array().unwrap();
    let ids: Vec<_> = data
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    // 恢复行不进 data；它也不置 has_output，所以第 3 行仍是同一个 turn 的 steer。
    assert_eq!(ids, ["itim_open", "itim_steer", "itim_final"]);
    assert!(data
        .iter()
        .all(|item| item["turn_id"] == turn_id_for_item("itim_open")));
    assert!(page["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["itemId"] == "itim_recovered"));
}

#[tokio::test]
async fn a_dangling_call_in_a_terminal_turn_is_incomplete() {
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_open").await;
    append(
        &store,
        2,
        "itim_call",
        ItemKind::ToolCall,
        json!({"toolCallId":"call-1","title":"tool","status":"in_progress"}),
        false,
    )
    .await;
    let call_status = |page: &Value| {
        page["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // 宿主没送终态、turn 已终止：仍报 in_progress 就是永远不变的错误信号。
    for terminal in [
        TurnStatus::Completed,
        TurnStatus::Cancelled,
        TurnStatus::Failed,
    ] {
        turn(&store, "itim_open", terminal).await;
        let page = page(&store, ItemOrder::Asc, None, 10).await;
        assert_eq!(call_status(&page), "incomplete", "{terminal:?}");
    }
    turn(&store, "itim_open", TurnStatus::InProgress).await;
    let running = page(&store, ItemOrder::Asc, None, 10).await;
    assert_eq!(call_status(&running), "in_progress");
}

/// 记录 `swarm_items_before` 调用次数的 store 包装（M-5 的回扫成本上限）。
struct CountingStore {
    inner: MemorySwarmStore,
    items_before: std::sync::atomic::AtomicUsize,
}

impl CountingStore {
    fn new() -> Self {
        Self {
            inner: MemorySwarmStore::new(),
            items_before: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn items_before_calls(&self) -> usize {
        self.items_before.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::store::SwarmStore for CountingStore {
    async fn swarm_upsert_session(
        &self,
        row: crate::store::SwarmSessionRow,
    ) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_upsert_session(row).await
    }

    async fn swarm_create_hosted_session(
        &self,
        row: crate::store::SwarmSessionRow,
        binding: crate::store::SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), crate::store::StoreError> {
        self.inner
            .swarm_create_hosted_session(row, binding, require_existing)
            .await
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<crate::store::SwarmSessionRow>, crate::store::StoreError> {
        self.inner.swarm_get_session(logical_id).await
    }

    async fn swarm_update_session(
        &self,
        update: crate::store::SwarmSessionUpdate,
    ) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_update_session(update).await
    }

    async fn swarm_patch_session(
        &self,
        patch: crate::store::SwarmSessionPatch,
    ) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_patch_session(patch).await
    }

    async fn swarm_list_sessions(
        &self,
        window: &crate::domain::SessionsWindow,
    ) -> Result<Vec<crate::store::SwarmSessionRow>, crate::store::StoreError> {
        self.inner.swarm_list_sessions(window).await
    }

    async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_delete_session(logical_id).await
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_append_item(row).await
    }

    async fn swarm_append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<crate::store::SwarmSessionUpdate>,
    ) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_append_items(rows, progress).await
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, crate::store::StoreError> {
        self.inner.swarm_items_after(logical_id, after, limit).await
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, crate::store::StoreError> {
        self.items_before
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .swarm_items_before(logical_id, before, limit)
            .await
    }

    async fn swarm_item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::store::SwarmItemPage, crate::store::StoreError> {
        self.inner
            .swarm_item_page(logical_id, order, after, limit)
            .await
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, crate::store::StoreError> {
        self.inner.swarm_last_seq(logical_id).await
    }

    async fn swarm_get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, crate::store::StoreError> {
        self.inner.swarm_get_item(logical_id, item_id).await
    }

    async fn swarm_put_input(
        &self,
        row: crate::store::SwarmInputRow,
    ) -> Result<crate::store::InputPut, crate::store::StoreError> {
        self.inner.swarm_put_input(row).await
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<crate::store::SwarmInputRow>, crate::store::StoreError> {
        self.inner.swarm_get_input(logical_id, input_id).await
    }

    async fn swarm_put_binding(
        &self,
        row: crate::store::SwarmBindingRow,
    ) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_put_binding(row).await
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<crate::store::SwarmBindingRow>, crate::store::StoreError> {
        self.inner.swarm_get_binding(logical_env_id).await
    }

    async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), crate::store::StoreError> {
        self.inner.swarm_upsert_turn(row).await
    }

    async fn swarm_get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, crate::store::StoreError> {
        self.inner.swarm_get_turn(logical_id, turn_id).await
    }

    async fn swarm_turn_page(
        &self,
        logical_id: &str,
        window: &crate::domain::TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, crate::store::StoreError> {
        self.inner.swarm_turn_page(logical_id, window).await
    }

    async fn swarm_close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<Value>,
    ) -> Result<u64, crate::store::StoreError> {
        self.inner
            .swarm_close_open_turns(logical_id, status, completed_at, error)
            .await
    }

    async fn swarm_has_open_turn(
        &self,
        logical_id: &str,
    ) -> Result<bool, crate::store::StoreError> {
        self.inner.swarm_has_open_turn(logical_id).await
    }
}

#[tokio::test]
async fn many_gaps_stop_at_the_lookback_cap_and_degrade_to_items_only() {
    let store = CountingStore::new();
    // 每个条目都带持久 turn 行：每次回扫都在第一页命中 opener，一个缺口一次回扫。
    for seq in (1..=19).step_by(2) {
        let opener = format!("itim_open_{seq}");
        append(
            &store.inner,
            seq,
            &opener,
            ItemKind::Message,
            json!({"role":"user","text":opener}),
            false,
        )
        .await;
        store
            .swarm_upsert_turn(SwarmTurnRow {
                logical_id: "agent-01".into(),
                turn_id: turn_id_for_item(&opener),
                status: TurnStatus::InProgress,
                created_at: 1,
                started_at: Some(1),
                completed_at: None,
                error: None,
                usage: None,
                subagent_id: None,
            })
            .await
            .unwrap();
    }
    let page = item_page(
        &store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order: ItemOrder::Asc,
            after: None,
            after_id: None,
            limit: 20,
        },
        900,
    )
    .await
    .unwrap();
    let page = serde_json::to_value(page).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 10);
    assert_eq!(
        store.items_before_calls(),
        8,
        "回扫次数必须受硬上限约束（9 个缺口只回扫 8 次）"
    );
    // 超上限后的缺口降级为 items-only：不把未验证的 turn 归属写进 data。
    // 第一个条目不需要缺口回扫，所以前 9 个条目仍可映射。
    assert_eq!(page["data"].as_array().unwrap().len(), 9);
    let mapped: Vec<_> = page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(!mapped.contains(&"itim_open_19".to_owned()));
    assert!(page["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == "itim_open_17"));
}

/// [回归测试] 历史多行可能让一个缺口的回扫本身跨过很多页。
#[tokio::test]
async fn a_deep_lookback_stops_at_the_page_cap_and_keeps_raw_rows() {
    let store = CountingStore::new();
    user(&store.inner, 1, "itim_opener").await;
    let last_history_seq = u64::from(ItemsWindow::MAX_LIMIT) * MAX_TURN_LOOKBACK_PAGES as u64 + 1;
    for seq in 2..=last_history_seq {
        append(
            &store.inner,
            seq,
            "itim_history",
            ItemKind::Message,
            json!({"role":"assistant","text":"recovered","complete":true,"recovered":true}),
            false,
        )
        .await;
    }
    user(&store.inner, last_history_seq + 1, "itim_steer").await;
    let page = item_page(
        &store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order: ItemOrder::Asc,
            after: Some(last_history_seq),
            after_id: None,
            limit: 1,
        },
        last_history_seq + 1,
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item_id, "itim_steer");
    assert!(page.data.is_empty());
    assert_eq!(store.items_before_calls(), MAX_TURN_LOOKBACK_PAGES);
}

#[tokio::test]
async fn multi_generation_streams_project_the_first_completion_plus_late_deltas() {
    // 历史写入器形状（同一 itemId 的多代 flush）：
    //   delta "hel" → complete "hel"（第一次 flush）→
    //   delta "lo"（迟到 chunk）→ complete "lo"（第二次 flush）
    // 第二次 complete 是该代 delta 的累计文本，不是新增文本；再追加会拼出 "hellolo"。
    //
    // 当前 writer 在 turn 终态 flush 后丢弃迟到 chunk，不再铸造第二代
    // （`actor/reader.rs` 的 `closed`，由 `reader_test` 覆盖），所以这个形状只能来自
    // 旧数据。这里直接用 store 构造历史行，投影规则仍需兼容它们。
    let store = MemorySwarmStore::new();
    user(&store, 1, "itim_opener").await;
    turn(&store, "itim_opener", TurnStatus::Completed).await;
    for (seq, kind, payload) in [
        (
            2,
            ItemKind::Message,
            json!({"role":"assistant","delta":true,"text":"hel"}),
        ),
        (3, ItemKind::Thought, json!({"delta":true,"text":"rea"})),
        (
            4,
            ItemKind::Message,
            json!({"role":"assistant","complete":true,"text":"hel"}),
        ),
        (5, ItemKind::Thought, json!({"complete":true,"text":"rea"})),
        (
            6,
            ItemKind::Message,
            json!({"role":"assistant","delta":true,"text":"lo"}),
        ),
        (7, ItemKind::Thought, json!({"delta":true,"text":"son"})),
        (
            8,
            ItemKind::Message,
            json!({"role":"assistant","complete":true,"text":"lo"}),
        ),
        (9, ItemKind::Thought, json!({"complete":true,"text":"son"})),
    ] {
        append(
            &store,
            seq,
            match kind {
                ItemKind::Message => "itim_message",
                _ => "itim_thought",
            },
            kind,
            payload,
            false,
        )
        .await;
    }

    let page = page(&store, ItemOrder::Asc, None, 20).await;
    let data = page["data"].as_array().expect("data");
    let message = data
        .iter()
        .find(|item| item["type"] == "message" && item["role"] == "assistant")
        .expect("assistant projection");
    assert_eq!(
        message["content"],
        json!([{ "type": "output_text", "text": "hello" }])
    );
    let reasoning = data
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("reasoning projection");
    assert_eq!(
        reasoning["summary"],
        json!([{ "type": "summary_text", "text": "reason" }])
    );
    // items 仍是逐帧原始行：两代各一条 delta 与一条 complete，共享同一 itemId。
    let frames = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|row| row["itemId"] == message["id"])
        .count();
    assert_eq!(frames, 4);
}
