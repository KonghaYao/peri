use super::*;

#[derive(Clone, Copy, PartialEq)]
enum ReadFault {
    FollowupPage,
    TurnHistory,
    StalledCursor,
    /// 每次读取存储页前追加一条独立 receipt，模拟请求期间的并发写入。
    AppendReceiptBeforeRead,
    /// 首次页读取后向无正文 thought 组追加一帧，并追加独立 answer。
    GrowFirstGroupAfterFirstRead {
        thought_seq: u64,
        answer_seq: u64,
    },
}

/// 记录回扫与探页调用，并能在指定读取处注入故障。
struct CountingStore {
    inner: MemorySwarmStore,
    items_before: std::sync::atomic::AtomicUsize,
    item_pages: std::sync::atomic::AtomicUsize,
    fault: Option<ReadFault>,
}

impl CountingStore {
    fn new() -> Self {
        Self {
            inner: MemorySwarmStore::new(),
            items_before: std::sync::atomic::AtomicUsize::new(0),
            item_pages: std::sync::atomic::AtomicUsize::new(0),
            fault: None,
        }
    }

    fn items_before_calls(&self) -> usize {
        self.items_before.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn item_page_calls(&self) -> usize {
        self.item_pages.load(std::sync::atomic::Ordering::SeqCst)
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
        if self.fault == Some(ReadFault::TurnHistory) {
            return Err(crate::store::StoreError::Unavailable(
                "test history read failure".into(),
            ));
        }
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
        let index = self
            .item_pages
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if index == 1 && self.fault == Some(ReadFault::FollowupPage) {
            return Err(crate::store::StoreError::Unavailable(
                "test second page failure".into(),
            ));
        }
        if self.fault == Some(ReadFault::AppendReceiptBeforeRead) {
            let seq = 3 + index as u64;
            append(
                &self.inner,
                seq,
                &format!("itim_concurrent_{seq}"),
                ItemKind::Receipt,
                json!({}),
                false,
            )
            .await;
        }
        let mut page = self
            .inner
            .swarm_item_page(logical_id, order, after, limit)
            .await?;
        if index == 0 {
            if let Some(ReadFault::GrowFirstGroupAfterFirstRead {
                thought_seq,
                answer_seq,
            }) = self.fault
            {
                append(
                    &self.inner,
                    thought_seq,
                    "itim_thought_empty",
                    ItemKind::Thought,
                    json!({"complete": true}),
                    false,
                )
                .await;
                append(
                    &self.inner,
                    answer_seq,
                    "itim_answer",
                    ItemKind::Message,
                    json!({"role": "assistant", "complete": true, "text": "answer"}),
                    false,
                )
                .await;
            }
        }
        if self.fault == Some(ReadFault::StalledCursor) && page.has_more {
            page.next_cursor = after;
        }
        Ok(page)
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

async fn list_fault_fixture(
    store: &CountingStore,
    order: ItemOrder,
    after: Option<u64>,
) -> Result<ItemPage, SwarmError> {
    item_page(
        store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order,
            after,
            after_id: None,
            limit: 1,
        },
        4,
    )
    .await
}

async fn fault_fixture(fault: ReadFault) -> CountingStore {
    let store = CountingStore {
        fault: Some(fault),
        ..CountingStore::new()
    };
    user(&store.inner, 1, "itim_opener").await;
    for seq in 2..=4 {
        append(
            &store.inner,
            seq,
            &format!("itim_receipt_{seq}"),
            ItemKind::Receipt,
            json!({}),
            false,
        )
        .await;
    }
    store
}

#[tokio::test]
async fn an_already_projectable_first_page_does_not_probe() {
    let store = fault_fixture(ReadFault::FollowupPage).await;
    let page = list_fault_fixture(&store, ItemOrder::Asc, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.next_cursor, Some(1));
    assert!(page.has_more);
    assert_eq!(store.item_page_calls(), 1);
}

#[tokio::test]
async fn a_second_page_read_failure_returns_only_a_502_error() {
    let store = fault_fixture(ReadFault::FollowupPage).await;
    let error = list_fault_fixture(&store, ItemOrder::Asc, Some(1))
        .await
        .unwrap_err();
    assert_eq!(store.item_page_calls(), 2);
    assert_store_read_error(error, "item_page").await;
}

#[tokio::test]
async fn a_turn_history_read_failure_returns_only_a_502_error() {
    let store = fault_fixture(ReadFault::TurnHistory).await;
    let error = list_fault_fixture(&store, ItemOrder::Asc, Some(1))
        .await
        .unwrap_err();
    assert_eq!(store.items_before_calls(), 1);
    assert_store_read_error(error, "items_before").await;
}

async fn assert_store_read_error(error: SwarmError, operation: &str) {
    use axum::response::IntoResponse;

    let response = error.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "upstream_unavailable");
    assert_eq!(body["error"]["details"]["operation"], operation);
    assert!(body.get("nextCursor").is_none());
    assert!(body.get("items").is_none());
    assert!(body.get("data").is_none());
}

#[tokio::test]
async fn an_empty_page_with_a_stalled_cursor_fails_in_both_orders() {
    for (order, after) in [(ItemOrder::Asc, 1), (ItemOrder::Desc, 4)] {
        let store = fault_fixture(ReadFault::StalledCursor).await;
        let error = list_fault_fixture(&store, order, Some(after))
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ItemsScanLimit);
        assert_eq!(store.item_page_calls(), 1);
    }
}

#[tokio::test]
async fn an_ascending_probe_stops_at_the_request_observation_bound() {
    let store = CountingStore {
        fault: Some(ReadFault::AppendReceiptBeforeRead),
        ..CountingStore::new()
    };
    user(&store.inner, 1, "itim_opener").await;
    append(
        &store.inner,
        2,
        "itim_receipt_2",
        ItemKind::Receipt,
        json!({}),
        false,
    )
    .await;

    // 请求开始时最后 seq 是 2；之后每次读页前都会追加一条新 receipt。
    let page = item_page(
        &store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order: ItemOrder::Asc,
            after: Some(1),
            after_id: None,
            limit: 1,
        },
        2,
    )
    .await
    .unwrap();

    assert_eq!(store.item_page_calls(), 1, "升序不追并发追加的条目");
    assert!(page.data.is_empty());
    assert_eq!(page.next_cursor, Some(2));
    assert!(page.has_more);
    let seqs: Vec<_> = page.items.iter().map(|row| row.seq).collect();
    assert_eq!(seqs, [2]);
}

#[tokio::test]
async fn a_group_grown_between_pages_keeps_each_seq_once() {
    let store = CountingStore {
        fault: Some(ReadFault::GrowFirstGroupAfterFirstRead {
            thought_seq: 4,
            answer_seq: 5,
        }),
        ..CountingStore::new()
    };
    user(&store.inner, 1, "itim_opener").await;
    append(
        &store.inner,
        2,
        "itim_thought_empty",
        ItemKind::Thought,
        json!({"complete": true}),
        false,
    )
    .await;
    append(
        &store.inner,
        3,
        "itim_receipt_3",
        ItemKind::Receipt,
        json!({}),
        false,
    )
    .await;

    let page = item_page(
        &store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order: ItemOrder::Asc,
            after: Some(1),
            after_id: None,
            limit: 1,
        },
        900,
    )
    .await
    .unwrap();

    // 首读后 thought 的最大 seq 从 2 变为 4，下一轮会把 [2, 4] 整组再选出一次。
    let seqs: Vec<_> = page.items.iter().map(|row| row.seq).collect();
    assert_eq!(seqs, [2, 3, 4, 5]);
    let unique: std::collections::BTreeSet<_> = seqs.iter().copied().collect();
    assert_eq!(unique.len(), seqs.len(), "同一响应内 seq 必须唯一");
    assert_eq!(page.data.len(), 1);
    assert_eq!(
        serde_json::to_value(&page).unwrap()["data"][0]["id"],
        "itim_answer"
    );
    assert_eq!(page.next_cursor, None);
}

#[tokio::test]
async fn repeated_frames_do_not_consume_the_inclusion_budget() {
    let store = CountingStore {
        fault: Some(ReadFault::GrowFirstGroupAfterFirstRead {
            thought_seq: 1003,
            answer_seq: 1004,
        }),
        ..CountingStore::new()
    };
    user(&store.inner, 1, "itim_opener").await;
    for seq in 2..=1001 {
        append(
            &store.inner,
            seq,
            "itim_thought_empty",
            ItemKind::Thought,
            json!({"complete": true}),
            false,
        )
        .await;
    }
    // 首读必须还有下一个组，否则末尾页本来就不会探页。
    append(
        &store.inner,
        1002,
        "itim_receipt_1002",
        ItemKind::Receipt,
        json!({}),
        false,
    )
    .await;

    let page = item_page(
        &store,
        &LogicalId::parse("agent-01").unwrap(),
        ItemsWindow {
            order: ItemOrder::Asc,
            after: Some(1),
            after_id: None,
            limit: 1,
        },
        5000,
    )
    .await
    .unwrap();

    // 1000 行组增长后重选，只多出 1 条新帧；重复行不能把纳入量推到 2000 以上。
    let seqs: Vec<_> = page.items.iter().map(|row| row.seq).collect();
    assert_eq!(seqs, (2..=1004).collect::<Vec<_>>());
    let unique: std::collections::BTreeSet<_> = seqs.iter().copied().collect();
    assert_eq!(unique.len(), seqs.len());
    assert_eq!(page.data.len(), 1);
    assert_eq!(store.item_page_calls(), 4);
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
