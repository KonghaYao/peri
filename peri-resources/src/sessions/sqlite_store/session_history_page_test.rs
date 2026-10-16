//! 历史分页读取（archive 视图）的端到端行为：首屏、向后翻页、不可展示条目、字节预算与失效游标。
//!
//! 断言以调用方能观察到的东西为准：页内顺序与内容、游标是否存在、错误分类。
//! 真实云端往返见 `remote/cloud_history_test.rs`（默认 `#[ignore]`）。

use super::*;
use peri_acp_types::history_page::{
    HistoryCursor, HistoryCursorError, HistoryPage, HistoryPageCandidate, HistoryPagePlan,
    HistoryPageRow,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::store::MessageFlags;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

/// 分页读取的字节预算在测试里的默认值：足够大，只验证顺序规则。
const NO_BUDGET_LIMIT: usize = usize::MAX;

const PAGE_SESSION: &str = "history-page";

fn ids(page: &HistoryPage) -> Vec<MessageId> {
    page.items.iter().map(|item| item.id).collect()
}

fn messages(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("message {index}"))))
        .collect()
}

fn model_only_reminder() -> PersistedPayload {
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("history_page_test".into()),
        kind: "notice".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Configurable,
        audiences: ReminderAudiences(vec![ReminderAudience::Model]),
        body: "model only".into(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(reminder)
            .unwrap(),
    }
}

/// 建一个会话并追加给定历史；返回数据面与落库后的载荷。
async fn session_with_history(
    history: &[PersistedPayload],
) -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        PAGE_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("history-page"),
    ))
    .await
    .unwrap();
    data.append_history(&PAGE_SESSION.to_owned(), history)
        .await
        .unwrap();
    (store, data, directory)
}

#[tokio::test]
async fn first_page_starts_from_the_newest_row_and_pages_backwards_without_gap() {
    let history = messages(7);
    let (_store, data, _directory) = session_with_history(&history).await;

    let first = data
        .load_history_page(&PAGE_SESSION.to_owned(), None, 3, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&first),
        vec![history[4].id(), history[5].id(), history[6].id()],
        "首屏从最新开始，页内仍是历史正序"
    );
    let cursor = first.next_cursor.clone().expect("更早方向还有内容");

    let second = data
        .load_history_page(&PAGE_SESSION.to_owned(), Some(&cursor), 3, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&second),
        vec![history[1].id(), history[2].id(), history[3].id()],
        "续读无缺口、无重复"
    );

    let third = data
        .load_history_page(
            &PAGE_SESSION.to_owned(),
            Some(&second.next_cursor.clone().unwrap()),
            3,
            NO_BUDGET_LIMIT,
        )
        .await
        .unwrap();
    assert_eq!(ids(&third), vec![history[0].id()]);
    assert!(third.next_cursor.is_none(), "读到范围起点即收尾");
}

#[tokio::test]
async fn excluded_archive_rows_are_returned_because_the_archive_is_what_pages_read() {
    let history = messages(4);
    let (_store, data, _directory) = session_with_history(&history).await;
    data.apply_message_projections(
        &PAGE_SESSION.to_owned(),
        &[(
            history[1].id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();
    // 标记确实生效：有效视图不再包含它。
    let active = data
        .load_active_snapshot(&PAGE_SESSION.to_owned())
        .await
        .unwrap();
    assert!(!active.payloads.iter().any(|p| p.id() == history[1].id()));

    let page = data
        .load_history_page(&PAGE_SESSION.to_owned(), None, 10, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&page),
        history
            .iter()
            .map(|payload| payload.id())
            .collect::<Vec<_>>(),
        "归档区间正是分页要读的内容"
    );
}

#[tokio::test]
async fn non_displayable_rows_advance_the_position_without_faking_the_end() {
    // 混合历史：可见条目之间夹着 System 条目与 Model-only reminder。
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("m0")),
        PersistedPayload::Message(BaseMessage::system("tool notice")),
        model_only_reminder(),
        PersistedPayload::Message(BaseMessage::ai("a1")),
        PersistedPayload::Message(BaseMessage::human("m1")),
    ];
    let (_store, data, _directory) = session_with_history(&history).await;

    // 最新 3 行里只有 a1/m1 可展示；不可展示的 reminder 仍占推进位置（游标停在它上面）。
    let first = data
        .load_history_page(&PAGE_SESSION.to_owned(), None, 3, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&first), vec![history[3].id(), history[4].id()]);
    let cursor = first
        .next_cursor
        .clone()
        .expect("一整段不可展示条目不是终点");
    assert_eq!(cursor.anchor, history[2].id());

    let second = data
        .load_history_page(&PAGE_SESSION.to_owned(), Some(&cursor), 3, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![history[0].id()]);
    assert!(second.next_cursor.is_none());
}

/// PARTIAL A 的存储级证据：多读的那一条候选行的正文**永不出库**。
///
/// 「更早方向还有内容」的那一行被直接写成不可解码的正文（绕过门面，只有原始 SQL 能造出
/// 这种行）：任何「先把 `limit + 1` 条正文都取回来再裁页」的实现都会在阶段二读到它并
/// 以损坏失败。本页必须原样读出，游标照常前进。
#[tokio::test]
async fn the_candidate_row_beyond_the_page_never_leaves_storage() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        PAGE_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("history-page"),
    ))
    .await
    .unwrap();
    // 更早一行：合法 uuid（阶段一按 uuid 解 id）+ 不可解码的正文。
    let unreadable = MessageId::new();
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(unreadable.as_uuid().to_string())
    .bind(PAGE_SESSION)
    .bind("user")
    .bind("{\"this is not\": a persisted payload envelope")
    .execute(&store.database.pool)
    .await
    .unwrap();
    let visible = PersistedPayload::Message(BaseMessage::human("only the page is read"));
    data.append_history(&PAGE_SESSION.to_owned(), std::slice::from_ref(&visible))
        .await
        .unwrap();

    // 页目标 1 条：候选行是「本页 1 条 + 更早的垃圾行」，阶段二只该问本页那条。
    let page = data
        .load_history_page(&PAGE_SESSION.to_owned(), None, 1, NO_BUDGET_LIMIT)
        .await
        .expect("多读的候选行正文不得被读出：读出即在这里以损坏失败");
    assert_eq!(ids(&page), vec![visible.id()]);
    assert!(
        page.next_cursor.is_some(),
        "多出的候选行说明更早方向还有内容"
    );
    assert!(
        data.load_history_page(
            &PAGE_SESSION.to_owned(),
            page.next_cursor.as_ref(),
            1,
            NO_BUDGET_LIMIT,
        )
        .await
        .is_err(),
        "垃圾行真被读到就必须失败关闭（这条断言锁住上面那句证据的前提）"
    );
}

#[tokio::test]
async fn a_payload_over_the_budget_occupies_its_own_page_and_the_cursor_advances() {
    let oversized = PersistedPayload::Message(BaseMessage::ai("x".repeat(1024 * 1024)));
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("before")),
        oversized.clone(),
        PersistedPayload::Message(BaseMessage::human("after")),
    ];
    let (_store, data, _directory) = session_with_history(&history).await;
    let budget = 64 * 1024;

    let first = data
        .load_history_page(&PAGE_SESSION.to_owned(), None, 5, budget)
        .await
        .unwrap();
    assert_eq!(ids(&first), vec![history[2].id()]);
    assert!(first.total_bytes() <= budget);
    assert!(!first.items[0].oversized);

    let second = data
        .load_history_page(
            &PAGE_SESSION.to_owned(),
            Some(&first.next_cursor.clone().unwrap()),
            5,
            budget,
        )
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![oversized.id()]);
    assert!(second.items[0].oversized, "超预算的单条独占一页");
    assert!(second.items[0].encoded_bytes > budget);
    let cursor = second
        .next_cursor
        .clone()
        .expect("oversize 之后游标仍要前进");

    let third = data
        .load_history_page(&PAGE_SESSION.to_owned(), Some(&cursor), 5, budget)
        .await
        .unwrap();
    assert_eq!(ids(&third), vec![history[0].id()]);
    assert!(third.next_cursor.is_none());
}

#[tokio::test]
async fn a_deleted_anchor_or_a_changed_range_head_expires_the_cursor() {
    let history = messages(6);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = PAGE_SESSION.to_owned();

    let first = data
        .load_history_page(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = first.next_cursor.clone().unwrap();
    assert_eq!(cursor.anchor, history[4].id());

    // 之后追加消息：起点与锚点都没变，游标仍然有效（旧页等价）。
    data.append_history(
        &id,
        &[PersistedPayload::Message(BaseMessage::human("late"))],
    )
    .await
    .unwrap();
    let second = data
        .load_history_page(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![history[2].id(), history[3].id()]);

    // 锚点被删除：失败关闭，不降级成「从最新一页开始」。
    let cursor = second.next_cursor.clone().unwrap();
    data.remove_history_entries(&id, &[history[2].id()])
        .await
        .unwrap();
    let error = data
        .load_history_page(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Cursor(HistoryCursorError::Expired { .. })
    ));

    // 浏览范围起点被删除（锚点还在）：同样失效。
    let fresh = data
        .load_history_page(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = fresh.next_cursor.clone().unwrap();
    data.remove_history_entries(&id, &[history[0].id()])
        .await
        .unwrap();
    let error = data
        .load_history_page(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Cursor(HistoryCursorError::Expired { .. })
    ));
}

#[tokio::test]
async fn a_cursor_from_another_session_is_rejected() {
    let history = messages(3);
    let (_store, data, _directory) = session_with_history(&history).await;
    let cursor = HistoryCursor::new("another-session".to_owned(), history[0].id(), "head");
    let error = data
        .load_history_page(&PAGE_SESSION.to_owned(), Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Cursor(HistoryCursorError::CrossSession { .. })
    ));
}

/// 初始 load 的边界读取（一次有界单行）：空会话 / 单条 / 有内容，且与分页读取同源。
#[tokio::test]
async fn bounds_answer_the_range_head_and_agree_with_the_first_page_cursor() {
    let (_store, data, _directory) = session_with_history(&[]).await;
    let id = PAGE_SESSION.to_owned();

    // 空会话：没有自有历史，因此没有起点，也不编造范围身份。
    let empty = data.history_page_bounds(&id).await.unwrap();
    assert_eq!(empty.range_head, None);
    assert!(empty.range_token.is_empty(), "空范围不该有身份");

    // 单条：起点就是它，token 就是持久化行里的 id 文本。
    let first = messages(1);
    data.append_history(&id, &first).await.unwrap();
    let single = data.history_page_bounds(&id).await.unwrap();
    assert_eq!(single.range_head, Some(first[0].id()));
    assert_eq!(single.range_token, first[0].id().as_uuid().to_string());

    // 有内容：追加只动尾部，起点与身份不变（正在进行的浏览不因新消息失效）。
    let tail = messages(3);
    data.append_history(&id, &tail).await.unwrap();
    let bounds = data.history_page_bounds(&id).await.unwrap();
    assert_eq!(bounds.range_head, Some(first[0].id()));
    assert_eq!(bounds.range_token, single.range_token);

    // 与分页读取同源：首屏游标携带的范围身份就是这里给出的值，拿它续读有效。
    let page = data
        .load_history_page(&id, None, 1, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = page.next_cursor.clone().expect("更早方向还有内容");
    assert_eq!(cursor.anchor, tail[2].id());
    assert_eq!(cursor.range_token, bounds.range_token);
    let second = data
        .load_history_page(&id, Some(&cursor), 1, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![tail[1].id()]);

    // 会话不存在：不回答「空范围」，明确失败。
    let error = data
        .history_page_bounds(&"missing-session".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
}

/// A13：两个后端从同一份行装配出同一页——本机 adapter 的产物必须等于共享装配规则在
/// 同一批原始行上的产物（远端 adapter 走同一函数，只是行的来源不同）。
///
/// 装配分两阶段（阶段一按长度定页，阶段二只取本页那些行的正文），这里把两阶段都按共享规则
/// 走一遍，正是为了让「本机 adapter 有没有多读正文」这类差别暴露出来。
#[tokio::test]
async fn the_local_backend_agrees_with_the_shared_page_assembly_on_the_same_rows() {
    let history = messages(5);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = PAGE_SESSION.to_owned();

    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT message_id, rowid, length(CAST(content AS BLOB)) FROM messages
         WHERE thread_id = ?1 ORDER BY rowid DESC LIMIT ?2",
    )
    .bind(&id)
    .bind(4_i64)
    .fetch_all(&store.database.pool)
    .await
    .unwrap();
    let head: (String,) = sqlx::query_as(
        "SELECT message_id FROM messages WHERE thread_id = ?1 ORDER BY rowid ASC LIMIT 1",
    )
    .bind(&id)
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    let plan = HistoryPagePlan::plan(
        &id,
        &head.0,
        rows.into_iter()
            .map(|(message_id, position, encoded_bytes)| {
                HistoryPageCandidate::new(&message_id, position, encoded_bytes).unwrap()
            })
            .collect(),
        3,
        NO_BUDGET_LIMIT,
    );
    let mut bodies = Vec::new();
    for message_id in plan.body_ids() {
        let expected = message_id.as_uuid().to_string();
        let content: String = sqlx::query_scalar(
            "SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2",
        )
        .bind(&id)
        .bind(&expected)
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
        bodies.push(HistoryPageRow::new(&expected, content).unwrap());
    }
    let expected = plan.materialize(bodies).unwrap();

    let actual = data
        .load_history_page(&id, None, 3, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&actual), ids(&expected));
    assert_eq!(actual.total_bytes(), expected.total_bytes());
    assert_eq!(actual.next_cursor, expected.next_cursor);
    assert_eq!(
        actual
            .items
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>(),
        expected
            .items
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>(),
        "位置（存储序键）也必须与共享规则一致"
    );
}
