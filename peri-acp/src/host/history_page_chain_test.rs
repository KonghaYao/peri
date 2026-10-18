//! 页链与合并（REQ-H01-3、验收 A08/A09）。
//!
//! 从 `history_page_wire_test.rs` 按职责拆出（STD-SIZE-001：单文件 ≤1000 行），语义未改动：
//! 用例体、断言与 helper 逐字节搬移。夹具与跨文件共享 helper 在父模块里，经 `use super::*` 取用。

use super::*;

/// A08：同一 cursor、同一生效 `limit` 重试 3 次给出等价页；换 `limit` 只改页长与边界，
/// 逻辑遍历顺序不变。
#[tokio::test]
#[serial]
async fn the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    // 归档最旧 4 条（并追加一条摘要）：首屏之后有 4 条更早历史，足以分两页。
    fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions.clone(), Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    let cursor = Value::String(cursor);

    let first = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
        )
        .await
        .expect("首游标必须可用");
    assert_eq!(
        first["items"].as_array().unwrap().len(),
        2,
        "limit=2 决定页长"
    );

    // 相同 cursor + 相同生效 limit 重试两次：条目身份、内容、顺序与后续位置全部等价。
    for attempt in 2..=3 {
        let again = client
            .send_request(
                "session/load",
                json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
            )
            .await
            .unwrap_or_else(|error| panic!("第 {attempt} 次重试必须成功：{error:?}"));
        assert_eq!(
            again["items"], first["items"],
            "第 {attempt} 次重试的页成员必须等价"
        );
        assert_eq!(
            again["nextCursor"], first["nextCursor"],
            "第 {attempt} 次重试的后续位置必须等价"
        );
        assert_eq!(again["snapshotToken"], first["snapshotToken"]);
    }

    // 换 limit：页长与页边界随之改变，但逻辑遍历顺序不变。
    let shorter = walk_pages(&client, &sid, &cursor, 2).await;
    let longer = walk_pages(&client, &sid, &cursor, 3).await;
    assert_page_chain(&shorter);
    assert_page_chain(&longer);
    assert_eq!(shorter[0].len(), 2, "limit=2 的首页两条");
    assert_eq!(longer[0].len(), 3, "limit 改变页长");

    // 同一段历史、同一个逻辑顺序：覆盖到的条目身份集合与 position 序列都一致，
    // 差别只在「哪几条凑成同一页」。
    let entry_ids = |pages: &[Vec<Value>]| {
        let mut ids: Vec<String> = pages
            .iter()
            .flatten()
            .map(|item| item["entryId"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(
        entry_ids(&shorter),
        entry_ids(&longer),
        "limit 不改变覆盖范围"
    );
    assert_eq!(
        logical_sequence(&shorter),
        logical_sequence(&longer),
        "limit 只改页边界：合并后的逻辑顺序不变"
    );
    assert_eq!(
        logical_sequence(&shorter).len(),
        4,
        "归档区的全部更早条目都要被翻出来"
    );
    task.abort();
}

/// A09：追加消息并再次 Full 之后，旧 cursor 与旧 `nextCursor` 仍然可用，页成员不被新的
/// `excluded` 或新的有效视图重新筛选。
#[tokio::test]
#[serial]
async fn a_cursor_survives_appends_and_another_full_without_moving_its_page_members() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions.clone(), Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    let cursor = Value::String(cursor);

    let first = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor.clone(), "limit": 2 }),
        )
        .await
        .expect("首页必须成功");
    let next = first["nextCursor"].clone();
    let second = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": next.clone(), "limit": 2 }),
        )
        .await
        .expect("第二页必须成功");

    // 实时尾部更新 + 再次 Full：追加只动尾部，Full 只改 flags 并追加摘要。
    fixture.append(2).await;
    fixture.archive(1).await;

    let first_again = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
        )
        .await
        .expect("旧 cursor 不得被追加与 Full 弄失效");
    assert_eq!(
        first_again["items"], first["items"],
        "旧页身份、内容与顺序必须等价"
    );
    assert_eq!(
        first_again["nextCursor"], first["nextCursor"],
        "旧页的后续位置必须等价"
    );

    let second_again = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": next, "limit": 2 }),
        )
        .await
        .expect("在途的旧 nextCursor 也必须仍然可用");
    assert_eq!(
        second_again["items"], second["items"],
        "下一段成员不得被重筛"
    );
    assert_eq!(second_again["nextCursor"], second["nextCursor"]);
    task.abort();
}

#[tokio::test]
#[serial]
async fn legacy_pages_keep_complete_cross_page_tool_output_with_window_beta_off() {
    use peri_acp_types::messages::ToolCallRequest;
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    // 同时覆盖普通结果与超过旧页 512 KiB 预算的单条结果。
    for size in [8192, 600 * 1024] {
        let output = format!("{}\nDIFF_ERROR_TAIL", "完整输出".repeat(size / 12));
        let start = BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new("cross-page", "Read", json!({}))],
        );
        let end = BaseMessage::tool_error("cross-page", output.clone());
        fixture
            .cfg
            .session_resources
            .append_history(
                &fixture.sid,
                &[
                    PersistedPayload::Message(start.clone()),
                    PersistedPayload::Message(end.clone()),
                ],
            )
            .await
            .unwrap();
        let count = fixture.own_ids().await.len();
        fixture.archive(count).await;
        let initial = fixture.initial_cursor().await;
        let caps = pagination_caps();
        assert!(!caps.history_window_v1);
        let (client, _, task) = spawn_loop(
            Arc::clone(&fixture.cfg),
            Arc::clone(&fixture.sessions),
            Some(caps),
        )
        .await;
        // 向前翻页先见结果，再见更早的工具起点。
        let result_page = client
            .send_request(
                "session/load",
                json!({"sessionId":fixture.sid,"cursor":initial["nextCursor"],"limit":1}),
            )
            .await
            .unwrap();
        let item = &result_page["items"][0];
        assert_eq!(
            item["entryId"],
            format!("{}:{}", fixture.sid, end.id().as_uuid())
        );
        let update = item["subItems"][0]["update"].clone();
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["content"][0]["content"]["text"], output);
        assert!(
            item.get("body").is_none(),
            "旧分页直接交付完整正文，不依赖 beta body 接口"
        );
        let start_page = client
            .send_request(
                "session/load",
                json!({"sessionId":fixture.sid,"cursor":result_page["nextCursor"],"limit":1}),
            )
            .await
            .unwrap();
        assert_eq!(
            start_page["items"][0]["entryId"],
            format!("{}:{}", fixture.sid, start.id().as_uuid())
        );
        assert!(start_page["items"][0]["subItems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["update"]["sessionUpdate"] == "tool_call"
                    && item["update"]["toolCallId"] == "cross-page"
            }));
        // 起点到达后可按原请求游标补读结果，不需要启用 historyWindowV1。
        let reread = client
            .send_request(
                "session/load",
                json!({
                    "sessionId":fixture.sid,"cursor":initial["nextCursor"],"limit":1
                }),
            )
            .await
            .unwrap();
        assert_eq!(reread["items"][0]["entryId"], item["entryId"]);
        assert_eq!(reread["snapshotToken"], result_page["snapshotToken"]);
        assert_eq!(
            reread["items"][0]["subItems"][0]["update"]["content"][0]["content"]["text"],
            output
        );
        task.abort();
    }
}
