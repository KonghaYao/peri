//! 首屏与初始游标契约（REQ-H01-1/§5.3、验收 A15/A20/A22 服务端半边）。
//!
//! 从 `history_page_wire_test.rs` 按职责拆出（STD-SIZE-001：单文件 ≤1000 行），语义未改动：
//! 用例体、断言与 helper 逐字节搬移。夹具与跨文件共享 helper 在父模块里，经 `use super::*` 取用。

use super::*;

/// A15 / REQ-H01-1：初始游标锚在自有有效区最旧一条，绝不退回首屏第一条。
#[tokio::test]
#[serial]
async fn initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;

    // 普通新会话：没有更早范围 → nextCursor = null（不编造游标）。
    let fresh = fixture.initial_cursor().await;
    assert_eq!(fresh["nextCursor"], Value::Null);
    assert!(fresh["snapshotToken"].is_string(), "浏览身份必须随响应给出");

    // 归档后：锚点 = 最旧的自有**有效**条目（摘要），不是首屏第一条（更不是被排除的原文）。
    let archived = fixture.archive(2).await;
    let response = fixture.initial_cursor().await;
    let cursor = response["nextCursor"]
        .as_str()
        .expect("归档历史存在时必须给出游标")
        .to_owned();
    let cursor = peri_acp_types::history_page::HistoryCursorEnvelope::decode(&cursor, &fixture.sid)
        .expect("游标必须可解码");
    assert_eq!(
        response["snapshotToken"].as_str(),
        Some(cursor.snapshot_token.as_str()),
        "游标与响应携带同一份浏览身份"
    );
    assert!(
        cursor.snapshot_token.ends_with(&cursor.cursor.range_token),
        "浏览身份含范围身份（另一半是存储看不见的投影口径）：{}",
        cursor.snapshot_token
    );

    let page = crate::dispatch::load_history_page(
        fixture.cfg.controller.as_ref(),
        &fixture.sid,
        &cursor.cursor,
        50,
        history_page::PAGE_BYTE_BUDGET,
    )
    .await
    .expect("首游标必须可用");
    let ids: Vec<MessageId> = page.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, archived, "首屏之后的页按历史正序补齐归档区");
    assert!(
        !ids.contains(&cursor.cursor.anchor),
        "页与首屏不得重叠：锚点条目已在首屏显示"
    );
    assert!(
        page.next_cursor.is_none(),
        "读到范围起点即收尾（更早方向没有内容）"
    );
}

/// 只让**通知出口**失败的 transport：注入一次回放投递失败（验收 A22 的服务端半边）。
///
/// 请求 / 响应通道原样委托给真实 MPSC 服务端：失败点必须精确落在回放的那条通知上，
/// 否则「load 失败」可能来自别的通道故障，用例就证明不了回放与 load 成败的绑定关系。
struct NotificationFailingTransport {
    inner: crate::transport::mpsc::MpscServerTransport,
}

#[async_trait::async_trait]
impl AcpTransport for NotificationFailingTransport {
    async fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, crate::transport::types::AcpError> {
        self.inner.send_request(method, params).await
    }

    async fn send_notification(
        &self,
        _method: &str,
        _params: Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        Err(crate::transport::types::AcpError::new(
            -32603,
            "replay delivery failed (injected)",
        ))
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        self.inner.recv().await
    }

    async fn send_response(
        &self,
        id: crate::transport::types::RequestId,
        result: Result<Value, crate::transport::types::AcpError>,
    ) -> Result<(), crate::transport::types::AcpError> {
        self.inner.send_response(id, result).await
    }
}

/// A22（服务端半边）：某一条初始回放投递失败 → 本次 load 以错误结束，且**不给出**任何
/// 分页身份（失败代次没有成功 cursor）。
///
/// 控制组先证明「同一份归档状态下，生产的初始游标函数本来会给出游标」：因此这次失败确实
/// 是回放投递造成的，而不是这条会话本来就没有更早历史。
#[tokio::test]
#[serial]
async fn a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    fixture.archive(4).await;
    assert!(
        fixture.initial_cursor().await["nextCursor"].is_string(),
        "控制组：这条会话本来会给出游标"
    );

    let cfg = Arc::clone(&fixture.cfg);
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let failing: Arc<dyn AcpTransport> = Arc::new(NotificationFailingTransport { inner: server });
    let (client, sessions, task) = spawn_loop_with_transport(
        cfg,
        fixture.sessions.clone(),
        Some(pagination_caps()),
        client,
        failing,
    )
    .await;
    let sid = fixture.sid.clone();

    let error = client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect_err("回放投递失败必须让初始 load 失败");
    assert_eq!(error.code, -32603, "回放失败按内部错误上报：{error:?}");
    assert!(
        error.message.contains("history replay failed"),
        "错误必须指明是回放失败：{error:?}"
    );
    // 失败响应里不可能有分页身份，会话也不留半加载态（下一次 load 仍可从同一份历史重试）。
    let states = sessions.lock().await;
    let state = states
        .get(&sid)
        .expect("失败的 load 不得删除已发布的会话状态");
    assert!(!state.closing, "回放失败不是会话关闭");
    drop(states);
    task.abort();
}

/// 合并契约（wire 级，REQ-H01-3 / A20）：协商分页的连接做初始 load 时，首屏每一条有效条目
/// 都带上与历史页**同一把尺子**的身份与位置，页覆盖的只是更早的那一段——客户端才能把稀疏
/// 首屏与后续页拼成一段不间断的历史。
///
/// 刻意走冷恢复（loop 手里的会话集合是空的）：只有这条路径的首屏与位置表同出于存储的有效
/// 视图。`AlreadyLive` 会沿用 fixture 内存里那份尚未被 compaction 改写的 `history_payloads`，
/// 那样测到的只是 fixture 自己。
#[tokio::test]
#[serial]
async fn a_negotiated_initial_load_marks_the_first_screen_with_page_comparable_identities() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    let archived = fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();
    let own_ids = fixture.own_ids().await;
    let expected_first_screen = own_ids.len() - archived.len();
    assert_eq!(
        (expected_first_screen, archived.len()),
        (3, 4),
        "fixture 前提：有效视图 3 条 + 归档区 4 条"
    );

    let cfg = Arc::clone(&fixture.cfg);
    let cold: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (client, _sessions, task) = spawn_loop(cfg, cold, Some(pagination_caps())).await;
    let sid = fixture.sid.clone();

    let load = {
        let client = Arc::clone(&client);
        let sid = sid.clone();
        tokio::spawn(async move {
            client
                .send_request("session/load", json!({ "sessionId": sid }))
                .await
        })
    };

    // 回放必须在响应**之前**发完：先收满首屏的条目身份，再取响应。少一条就说明首屏里有条目
    // 没被标记，客户端也就无从把它并进页序列。
    let mut marks: Vec<(String, u64)> = Vec::new();
    let mut generations: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while marks.len() < expected_first_screen {
        let incoming = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("首屏条目必须在时限内全部带上身份与位置")
            .expect("回放结束之前客户端通道不得关闭");
        let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        else {
            continue;
        };
        if method != "session/update" {
            continue;
        }
        let meta = &params["update"]["_meta"];
        let Some(entry) = meta["periEntryId"].as_str() else {
            continue;
        };
        if marks.iter().any(|(seen, _)| seen == entry) {
            continue;
        }
        marks.push((
            entry.to_owned(),
            meta["periPosition"]
                .as_u64()
                .unwrap_or_else(|| panic!("首屏自有条目必须带存储序位置：{meta}")),
        ));
        generations.push(
            meta["periLoadGeneration"]
                .as_str()
                .unwrap_or_else(|| panic!("位置戳与本次 load 的代次同进同出：{meta}"))
                .to_owned(),
        );
    }
    let reply = load
        .await
        .expect("load 任务不得 panic")
        .expect("初始 load 必须成功");

    // 首屏覆盖自有条目里**没被归档**的那一段，且逐条带身份。
    let mut first_screen: Vec<String> = marks.iter().map(|(entry, _)| entry.clone()).collect();
    first_screen.sort();
    let mut expected: Vec<String> = own_ids
        .iter()
        .filter(|id| !archived.contains(id))
        .map(|id| crate::dispatch::entry_id(&sid, *id))
        .collect();
    expected.sort();
    assert_eq!(first_screen, expected, "首屏条目必须逐条带身份");
    assert!(
        generations.iter().all(|seen| seen == &generations[0]),
        "一次 load 只有一个代次：{generations:?}"
    );
    assert_eq!(
        Some(generations[0].as_str()),
        reply["_meta"]["periLoadGeneration"].as_str(),
        "回放通知与成功响应必须带同一个代次（REQ-H04）"
    );

    // 更早的那一段：页给出的身份与归档段逐条对应，且位置严格小于首屏的最低位置——两段不重叠，
    // 合起来正好是全部自有条目。
    let pages = walk_pages(&client, &sid, &Value::String(cursor), 2).await;
    assert_page_chain(&pages);
    let mut page_entries: Vec<String> = pages
        .iter()
        .flatten()
        .map(|item| {
            item["entryId"]
                .as_str()
                .expect("页条目必须带身份")
                .to_owned()
        })
        .collect();
    page_entries.sort();
    let mut archived_entries: Vec<String> = archived
        .iter()
        .map(|id| crate::dispatch::entry_id(&sid, *id))
        .collect();
    archived_entries.sort();
    assert_eq!(page_entries, archived_entries, "页覆盖的就是归档的那一段");
    let page_positions = logical_sequence(&pages);
    let lowest_first_screen = marks
        .iter()
        .map(|(_, position)| *position)
        .min()
        .expect("首屏必须至少有一条带位置的条目");
    assert!(
        page_positions
            .iter()
            .all(|position| *position < lowest_first_screen),
        "页必须严格早于首屏：首屏最低位置 {lowest_first_screen}，页 {page_positions:?}"
    );
    assert_eq!(
        page_positions.len() + marks.len(),
        own_ids.len(),
        "首屏 + 页必须覆盖全部自有条目"
    );
    task.abort();
}

// ─── A15 回归：own 区首条是不可展示条目（subagent / fork / bg 子会话的真实形状）──────
//
// 曾经：`dispatch::initial_history_cursor` 取「有效视图最早一条 own payload」当锚点。
// 该条目若不可展示、又不被 Full 排除（真实来源：子会话身份 System 条目，
// `session/subagent/factory/spawn.rs:351`），比它晚的被排除原文永远不会被任何一页交付；
// 它同时又是线程最旧一行时，`nextCursor` 直接是 `null`（谎称没有更早历史）。
// 锚点是**首屏已交付集合**的下边界，可展示性判据与分页出口同源。

/// own = [System 身份, h1, h2] + 一次 Full（排除 h1/h2、追加摘要）：
/// 锚点落在摘要（首屏唯一可展示条目），压缩原文随页链全部交付。
#[tokio::test]
#[serial]
async fn the_initial_cursor_anchors_past_non_displayable_own_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-system-identity-head"),
    )
    .await;

    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    let h1 = PersistedPayload::Message(BaseMessage::human("older turn"));
    let h2 = PersistedPayload::Message(BaseMessage::ai("just-compacted answer"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system.clone(), h1.clone(), h2.clone()])
        .await
        .unwrap();
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: vec![
                    (
                        h1.id(),
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    ),
                    (
                        h2.id(),
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    ),
                ],
                appended_messages: vec![BaseMessage::ai("compacted summary")],
            },
        )
        .await
        .unwrap();

    let effective = fixture
        .cfg
        .session_resources
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        effective.first().map(PersistedPayload::id),
        Some(system.id()),
        "夹具：身份 System 条是 own 区首条（子会话形状）"
    );
    let summary = effective.last().map(PersistedPayload::id);
    assert_eq!(effective.len(), 2, "有效视图 = [身份 System, 摘要]");

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    let cursor = initial
        .next_cursor
        .expect("存储里有压缩原文，不得谎称没有更早历史");
    assert_eq!(
        Some(cursor.anchor),
        summary,
        "锚点是首屏唯一可展示条目（摘要），不是身份 System 条"
    );

    // 走完页链：压缩原文（比锚点早的行）必须全部交付，且与首屏零重叠。
    let mut next = Some(cursor);
    let mut delivered: Vec<MessageId> = Vec::new();
    for _ in 0..4 {
        let Some(current) = next else { break };
        let page = crate::dispatch::load_history_page(
            fixture.cfg.controller.as_ref(),
            &sid,
            &current,
            50,
            history_page::PAGE_BYTE_BUDGET,
        )
        .await
        .expect("初始游标必须可用");
        delivered.extend(page.items.iter().map(|item| item.id));
        next = page.next_cursor;
    }
    assert!(next.is_none(), "页链读到范围起点收尾，不假造终点");
    assert_eq!(delivered, vec![h1.id(), h2.id()], "压缩原文按历史正序交付");
    assert!(
        !delivered.contains(&summary.expect("有效视图非空")),
        "首屏已展示的摘要不得重复出现在页里"
    );
}

/// 有效视图里一条可展示条目都没有（只有身份 System，且没有任何归档）：`nextCursor = null`。
/// 这一支走「退回最新一条有效自有 payload」的兜底锚点，它同样不能编造游标。
#[tokio::test]
#[serial]
async fn an_all_non_displayable_own_region_reports_no_earlier_page() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-system-only"),
    )
    .await;
    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system])
        .await
        .unwrap();

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    assert!(
        initial.next_cursor.is_none(),
        "唯一自有行既是锚点又是范围起点：没有更早历史"
    );
}

/// 兜底分支的证据（不是「为了有个游标」而编造）：有效视图里一条可展示条目都没有、而存储里
/// 还有更老的可展示归档行时，锚点落在**最新一条有效自有条目**上，那行归档正文仍可交付
/// （旧实现的 `payloads.first()` 在这里直接 `nextCursor = null`，归档行不可达）。
#[tokio::test]
#[serial]
async fn the_fallback_anchor_keeps_an_archived_row_reachable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-fallback-anchor"),
    )
    .await;

    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    let archived = PersistedPayload::Message(BaseMessage::human("archived body"));
    let reminder = model_only_reminder("model-only note");
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system.clone(), archived.clone(), reminder.clone()])
        .await
        .unwrap();
    // 只排除归档行、且**不**追加摘要：「摘要被移除后正文仍被排除」这一形状的极端版。
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: vec![(
                    archived.id(),
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                )],
                appended_messages: vec![],
            },
        )
        .await
        .unwrap();

    let effective = fixture
        .cfg
        .session_resources
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        effective
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![system.id(), reminder.id()],
        "形状前提：有效视图 = [身份 System, 仅模型受众 reminder]"
    );
    assert!(
        effective
            .iter()
            .all(|payload| !payload.displayable_to_client()),
        "形状前提：有效视图里没有任何可展示条目"
    );

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    let cursor = initial
        .next_cursor
        .expect("存储里还有一条更老的可展示行，不得谎称没有更早历史");
    assert_eq!(
        cursor.anchor,
        reminder.id(),
        "兜底锚点是最新一条有效自有条目，不是有效视图第一条"
    );

    let page = crate::dispatch::load_history_page(
        fixture.cfg.controller.as_ref(),
        &sid,
        &cursor,
        50,
        history_page::PAGE_BYTE_BUDGET,
    )
    .await
    .expect("兜底游标必须可用");
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![archived.id()],
        "比兜底锚点更早的归档正文仍被交付"
    );
    assert!(page.next_cursor.is_none(), "读到范围起点收尾，不假造终点");
}

/// 仅模型受众的 canonical reminder：`displayable_to_client()` = false（不进入客户端渲染）。
fn model_only_reminder(body: &str) -> PersistedPayload {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("history_page_wire_test".into()),
            kind: "model_only".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.to_owned(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder,
    }
}
