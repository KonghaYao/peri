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
