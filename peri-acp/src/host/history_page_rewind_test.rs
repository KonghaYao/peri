//! rewind 与过期游标（REQ-T04 末段、验收 A19）。
//!
//! 从 `history_page_wire_test.rs` 按职责拆出（STD-SIZE-001：单文件 ≤1000 行），语义未改动：
//! 用例体、断言与 helper 逐字节搬移。夹具与跨文件共享 helper 在父模块里，经 `use super::*` 取用。

use super::*;

/// A19 / REQ-T04 末段：**已发出的游标在结构版本前进后失效**——wire 面同样是「拒绝，不是
/// 初始 load」。
///
/// 先证明该游标在 rewind 前确实能出页（否则用例是空转），再走一次真实的 `session/rewind`
/// （唯一推进结构版本的提交），然后用**同一个**游标重取：拒绝必须来自版本判据，而不是
/// 「游标本来就坏」。
#[tokio::test]
#[serial]
async fn an_expired_cursor_is_rejected_without_an_initial_load() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let sid = fixture.sid.clone();
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();
    // 回退目标 = 自有区最后一条（回退到它即把它自己及以后移出历史）。
    let target = fixture
        .own_ids()
        .await
        .last()
        .expect("夹具会话必须有自有条目")
        .as_uuid()
        .to_string();
    // rewind 的门是**会话级**注册表（`SessionManager::get_caps`），与分页的连接级门不同源。
    fixture.cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..pagination_caps()
        },
    );

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;

    // 先决条件：该游标此刻真的可用（拒绝不能来自「游标本就无效」）。
    let page = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect("rewind 之前该游标必须可用");
    assert!(
        !page["items"]
            .as_array()
            .expect("历史页必须带 items")
            .is_empty(),
        "先决条件：该游标确实能读到归档条目：{page}"
    );

    let preview = client
        .send_request(
            "session/rewind-preview",
            json!({ "sessionId": sid, "target_message_id": target }),
        )
        .await
        .expect("rewind 预览必须成功");
    let fingerprint = preview["preview_fingerprint"]
        .as_str()
        .expect("预览必须给出指纹")
        .to_owned();
    client
        .send_request(
            "session/rewind",
            json!({
                "sessionId": sid,
                "target_message_id": target,
                "preview_fingerprint": fingerprint,
            }),
        )
        .await
        .expect("rewind 必须成功");
    // rewind 自己会发通知（RewindCompleted 等），不属于本用例判据：先排空。
    s9c_drain_notifications(&client).await;

    let before = session_facts(&sessions, &sid).await;
    assert_eq!(
        before.2, 1,
        "一次成功的 rewind 恰好推进一次结构版本（否则下面的拒绝判据不成立）"
    );
    let stored_before = cfg
        .session_resources
        .load_session_history(&sid)
        .await
        .unwrap()
        .len();

    let error = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("结构版本前进后旧游标必须被拒绝，实际返回了成功响应"));
    assert_page_rejection(&error, -32012, "expired_cursor", "rewind 之后的旧游标");
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
    );
    assert_eq!(
        session_facts(&sessions, &sid).await,
        before,
        "拒绝不得触发一次初始 load，也不得改动热态"
    );
    assert_eq!(
        cfg.session_resources
            .load_session_history(&sid)
            .await
            .unwrap()
            .len(),
        stored_before,
        "拒绝不得改动存储里的 canonical 历史"
    );
    task.abort();
}
