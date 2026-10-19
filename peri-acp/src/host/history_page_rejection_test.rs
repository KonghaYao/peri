//! 分页准入与拒绝路径（REQ §5.1~§5.3、验收 A21/A25）。
//!
//! 从 `history_page_wire_test.rs` 按职责拆出（STD-SIZE-001：单文件 ≤1000 行），语义未改动：
//! 用例体、断言与 helper 逐字节搬移。夹具与跨文件共享 helper 在父模块里，经 `use super::*` 取用。

use super::*;

/// A21：翻页与正在执行的 turn 并行——分页不取执行生命周期锁，也不回答 -32010。
#[tokio::test]
#[serial]
async fn a_page_request_is_answered_while_the_session_holds_the_execution_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(2).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    // 执行中的 turn：该会话的 prompt 锁被持有。
    let prompt_lock = Arc::new(tokio::sync::Mutex::new(()));
    let held = Arc::clone(&prompt_lock).lock_owned().await;
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().cancel_token = Some(CancellationToken::new());
    }

    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 50 }),
        ),
    )
    .await
    .expect("分页不得排队等待执行生命周期锁")
    .expect("分页请求必须成功");

    let items = reply["items"].as_array().expect("历史页必须带 items");
    assert_eq!(
        items.len(),
        archived.len(),
        "归档正文正是分页要读的内容：{reply}"
    );
    assert!(reply.get("nextCursor").is_some(), "页响应必须携带游标字段");
    // 不装配、不改绑定：会话状态里没有 environment，也没有被广播的配置更新。
    let states = sessions.lock().await;
    let state = states.get(&sid).expect("分页不改动会话集合");
    assert!(
        state.environment.is_none(),
        "分页不得执行任务绑定 / MCP attach"
    );
    assert!(!state.closing);
    drop(states);
    drop(held);
    task.abort();
}

/// A25 / REQ §5.3：未协商分页能力的连接带分页字段 → 明确拒绝，且不降级为初始 load。
///
/// 拒绝面四条断言：错误码 + `data.reason`、**零回放通知**、热态四元事实不变、
/// 存储里的 canonical 历史不变。
#[tokio::test]
#[serial]
async fn pagination_fields_from_an_unnegotiated_connection_are_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(1).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("游标由能力协商前的读取给出")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        fixture.sessions,
        Some(PeriCaps::default()),
    )
    .await;
    let sid = fixture.sid.clone();
    let before = session_facts(&sessions, &sid).await;
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
        .expect_err("未协商能力时携带 cursor 必须拒绝");
    assert_page_rejection(
        &error,
        -32601,
        "pagination_not_negotiated",
        "未协商分页的连接带 cursor",
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
    );
    assert_eq!(
        session_facts(&sessions, &sid).await,
        before,
        "拒绝不得触发一次初始 load：热态四元事实不变"
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
    assert_eq!(archived.len(), 1);
    task.abort();
}

/// A25 / REQ §5.2~§5.3：**分类阶段被拒的每一类坏请求**都不得降级成初始 load。
///
/// 表驱动：每条坏请求断言拒绝面（错误码 + `data.reason`，且 `data` 只含 reason）、
/// **零回放通知**、热态四元事实不变、canonical 历史不变。连接**已协商**分页能力——
/// 分类阶段的拒绝必须来自参数本身，而不是能力门。
#[tokio::test]
#[serial]
async fn a_rejected_page_request_never_runs_an_initial_load() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let sid = fixture.sid.clone();
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();
    // 同一个 cursor 换一个会话身份重新封装：解出的内层游标属于别的 thread（R7 的跨会话形态）。
    // 目标会话不必存在——跨会话判定只看游标自身的会话身份。
    let envelope = peri_acp_types::history_page::HistoryCursorEnvelope::decode(&cursor, &sid)
        .expect("首游标必须可解码");
    let cross_session = peri_acp_types::history_page::HistoryCursorEnvelope::new(
        envelope.structure_version,
        envelope.snapshot_token.clone(),
        peri_acp_types::history_page::HistoryCursor::new(
            "another-session".to_owned(),
            envelope.cursor.anchor,
            envelope.cursor.range_token.clone(),
        ),
    )
    .encode();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;
    let before = session_facts(&sessions, &sid).await;
    let stored_before = cfg
        .session_resources
        .load_session_history(&sid)
        .await
        .unwrap()
        .len();

    // (用例标签, 请求参数, 期望错误码, 期望 data.reason)
    let cases: [(&str, Value, i64, &str); 8] = [
        (
            "只有 limit、没有 cursor 字段（R1）",
            json!({ "sessionId": sid, "limit": 10 }),
            -32602,
            "limit_without_cursor",
        ),
        (
            "cursor 为空串（R3）",
            json!({ "sessionId": sid, "cursor": "" }),
            -32602,
            "invalid_cursor",
        ),
        (
            "cursor 不是字符串（R3）",
            json!({ "sessionId": sid, "cursor": 42 }),
            -32602,
            "invalid_cursor",
        ),
        (
            "cursor 属于别的会话（R7）",
            json!({ "sessionId": sid, "cursor": cross_session }),
            -32011,
            "cross_session_cursor",
        ),
        (
            "历史页带 cwd（R4）",
            json!({ "sessionId": sid, "cursor": cursor, "cwd": "/tmp" }),
            -32602,
            "setup_param_on_history_page",
        ),
        (
            "历史页带 mcpServers（R4）",
            json!({ "sessionId": sid, "cursor": cursor, "mcpServers": [] }),
            -32602,
            "setup_param_on_history_page",
        ),
        (
            "历史页带 loadGeneration（R5）",
            json!({ "sessionId": sid, "cursor": cursor, "loadGeneration": "gen" }),
            -32602,
            "load_generation_on_history_page",
        ),
        (
            "limit 越界（R8）",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 0 }),
            -32602,
            "invalid_limit",
        ),
    ];

    for (label, params, code, reason) in cases {
        let error = client
            .send_request("session/load", params)
            .await
            .err()
            .unwrap_or_else(|| panic!("{label}：必须被拒绝，实际返回了成功响应"));
        assert_page_rejection(&error, code, reason, label);
        assert!(
            s9c_drain_notifications(&client).await.is_empty(),
            "{label}：拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
        );
        assert_eq!(
            session_facts(&sessions, &sid).await,
            before,
            "{label}：拒绝不得触发一次初始 load，也不得改动热态"
        );
        assert_eq!(
            cfg.session_resources
                .load_session_history(&sid)
                .await
                .unwrap()
                .len(),
            stored_before,
            "{label}：拒绝不得改动存储里的 canonical 历史"
        );
    }

    // 对照：同一连接上，合法历史页仍然成功（否则上面的「全被拒绝」可能只是连接坏了）。
    let page = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect("同一连接上的合法游标必须照常出页");
    assert!(
        !page["items"]
            .as_array()
            .expect("历史页必须带 items")
            .is_empty(),
        "归档条目正是分页要读的内容：{page}"
    );
    task.abort();
}

/// 复核 R6 / REQ §5.2：**合法 cursor 但缺 `sessionId`** 的历史页请求必须被拒绝，且不得降级成
/// 初始 load。
///
/// 这一类的拒绝**没有 `data.reason`**：会话身份在参数解析阶段就失败（`-32602`），请求本身
/// 没有指名会话，也就没有可分类的对象——它是本文件里唯一不带分类的拒绝面，
/// 不能套用 [`assert_page_rejection`] 的「data 只含 reason」规则。
#[tokio::test]
#[serial]
async fn a_page_request_without_a_session_id_is_rejected_without_an_initial_load() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let sid = fixture.sid.clone();
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;
    let before = session_facts(&sessions, &sid).await;
    let stored_before = cfg
        .session_resources
        .load_session_history(&sid)
        .await
        .unwrap()
        .len();

    let error = client
        .send_request("session/load", json!({ "cursor": cursor }))
        .await
        .err()
        .unwrap_or_else(|| panic!("缺 sessionId 的历史页请求必须被拒绝，实际返回了成功响应"));
    assert_eq!(error.code, -32602, "缺 sessionId：错误码");
    assert_eq!(
        error.data, None,
        "缺 sessionId 的拒绝不带 data（没有 data.reason 可分类）：{error}"
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
    );
    assert_eq!(
        session_facts(&sessions, &sid).await,
        before,
        "拒绝不得触发一次初始 load：热态四元事实不变"
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

    // 对照：补上 sessionId 后，同一连接上同一个 cursor 照常出页（否则「被拒」可能只是连接坏了）。
    let page = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect("同一连接上补上 sessionId 后该游标必须照常出页");
    assert!(
        !page["items"]
            .as_array()
            .expect("历史页必须带 items")
            .is_empty(),
        "归档条目正是分页要读的内容：{page}"
    );
    task.abort();
}

/// 复核 R9 / REQ §5.2：会话**不在热态集合**里（存储仍在，内存态没有 = 进程重启后的冷恢复
/// 视角）时，用原 cursor 取页必须被拒绝——不得凭 cursor 里的会话身份把会话顺手建回来。
///
/// 拒绝没有 `data.reason`（存在性判据直接给 `-32602`）；断言三条：错误码、**零回放通知**、
/// 热态集合与存储都不得被写。
#[tokio::test]
#[serial]
async fn a_page_request_for_a_session_outside_the_live_set_is_rejected_without_creating_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let sid = fixture.sid.clone();
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;

    // 先决条件：会话还在热态集合里时该游标确实能出页（拒绝不能来自「游标本就无效」）。
    let page = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect("移除会话之前该游标必须可用");
    assert!(
        !page["items"]
            .as_array()
            .expect("历史页必须带 items")
            .is_empty(),
        "先决条件：该游标确实能读到归档条目：{page}"
    );
    assert!(
        sessions.lock().await.remove(&sid).is_some(),
        "夹具必须先有这条热态会话"
    );
    let hot_before = sessions.lock().await.len();
    s9c_drain_notifications(&client).await;
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
        .unwrap_or_else(|| panic!("会话不在热态集合时必须被拒绝，实际返回了成功响应"));
    assert_eq!(error.code, -32602, "会话不在热态集合：错误码");
    assert_eq!(
        error.data, None,
        "会话不在热态集合的拒绝不带 data（没有 data.reason 可分类）：{error}"
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
    );
    let states = sessions.lock().await;
    assert!(
        !states.contains_key(&sid),
        "拒绝不得凭 cursor 把会话建回热态集合：{:?}",
        states.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        states.len(),
        hot_before,
        "拒绝不得创建任何会话（热态集合大小不变）"
    );
    drop(states);
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

/// 复核 R10 / REQ §5.2：会话处于 `closing`（`session/close` 已置位）时，历史页请求按
/// **忙 / 关闭**回答 `-32010`，不得发出任何回放通知，也不得改动热态与存储。
///
/// 与 A21 相对：执行中的 turn（持执行锁）**不是**拒绝理由，只有关闭状态才是。这一类的拒绝
/// 同样没有 `data.reason`（关闭是会话状态，不是请求分类）。
#[tokio::test]
#[serial]
async fn a_page_request_for_a_closing_session_is_rejected_without_a_replay() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let sid = fixture.sid.clone();
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;

    // 先决条件：关闭之前该游标确实能出页（拒绝不能来自「游标本就无效」）。
    let page = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect("关闭之前该游标必须可用");
    assert!(
        !page["items"]
            .as_array()
            .expect("历史页必须带 items")
            .is_empty(),
        "先决条件：该游标确实能读到归档条目：{page}"
    );
    {
        let mut states = sessions.lock().await;
        states
            .get_mut(&sid)
            .expect("夹具会话必须仍在热态集合里")
            .closing = true;
    }
    s9c_drain_notifications(&client).await;
    let before = session_facts(&sessions, &sid).await;
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
        .unwrap_or_else(|| panic!("会话处于 closing 时必须被拒绝，实际返回了成功响应"));
    assert_eq!(error.code, -32010, "关闭中的会话：错误码");
    assert_eq!(
        error.data, None,
        "关闭中的会话拒绝不带 data（没有 data.reason 可分类）：{error}"
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）"
    );
    assert_eq!(
        session_facts(&sessions, &sid).await,
        before,
        "拒绝不得触发一次初始 load：热态四元事实不变"
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

/// A09：协商分页连接的**初始 load** 遇到 `HandedOff`（正文在在途执行器里）时必须
/// 返回 `-32010`，绝不能把空的 `history_payloads` 当成空会话回放并返回成功。
///
/// `prepare_existing` 对 `HandedOff` 刻意不动热态，因此判定必须发生在回放分支。
/// 真实在途 prompt 由执行锁挡住、走不到这里；用例直接构造该热态（所有权 + 空集合），
/// 断言拒绝面：错误码 / 措辞、**零回放通知**、热态未被回填或改写。
#[tokio::test]
#[serial]
async fn a_paginated_initial_load_for_a_handed_off_session_is_rejected_without_replay() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;
    {
        let mut states = sessions.lock().await;
        let state = states.get_mut(&sid).expect("夹具会话必须仍在热态集合里");
        state.context_ownership = crate::host::ContextOwnership::HandedOff;
        state.history_payloads.clear();
    }

    let error = client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .err()
        .unwrap_or_else(|| panic!("HandedOff 会话的初始 load 必须被拒绝，实际返回了成功响应"));
    assert_eq!(error.code, -32010, "HandedOff：错误码");
    assert_eq!(
        error.message, "Session context is owned by an in-flight prompt",
        "措辞与 ensure_resident_context 对 HandedOff 的拒绝一致，实际：{error}"
    );
    assert_eq!(
        error.data, None,
        "HandedOff 的拒绝不带 data（没有 data.reason 可分类）：{error}"
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "拒绝不得发出任何历史回放通知（空历史回放正是 A09 缺陷）"
    );
    let states = sessions.lock().await;
    let state = states.get(&sid).expect("拒绝不得移除会话");
    assert_eq!(
        state.context_ownership,
        crate::host::ContextOwnership::HandedOff,
        "拒绝不得改写所有权状态"
    );
    assert!(
        state.history_payloads.is_empty(),
        "拒绝不得把存储快照回填进热态（HandedOff 仍是哨兵）"
    );
    drop(states);
    task.abort();
}
