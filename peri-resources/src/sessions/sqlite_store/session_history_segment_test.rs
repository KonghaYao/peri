//! canonical 完整回放（向前段读取）的端到端行为：首段继承区、固定上界、段边界与失效锚点。
//!
//! 断言以调用方能观察到的东西为准：段的载荷顺序与内容、游标是否存在与它给出的区间、错误
//! 分类。同一份语义在远端 adapter 上由 `remote/session_history_page_test.rs` 断言（两个
//! 后端必须同形）。

use super::*;
use peri_acp_types::history_page::{HistoryCursorError, HistoryForwardCursor};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::store::{InheritedContext, MessageFlags};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

/// 段读取的字节预算在测试里的默认值：足够大，只验证顺序规则。
const NO_BUDGET_LIMIT: usize = usize::MAX;

const SEGMENT_SESSION: &str = "history-segment";

fn ids(payloads: &[PersistedPayload]) -> Vec<MessageId> {
    payloads.iter().map(PersistedPayload::id).collect()
}

/// 段序列摊平后的载荷身份（段内顺序照原样）。
fn segment_ids(segments: &[Vec<PersistedPayload>]) -> Vec<MessageId> {
    ids(&segments.iter().flatten().cloned().collect::<Vec<_>>())
}

fn messages(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("message {index}"))))
        .collect()
}

/// 客户端不可渲染的条目（Model-only reminder）：完整回放必须照原样交付它。
fn model_only_reminder() -> PersistedPayload {
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("history_segment_test".into()),
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

/// 建一个会话并追加给定历史；返回存储句柄（用于写继承区快照）、数据面与临时目录。
async fn session_with_history(
    history: &[PersistedPayload],
) -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        SEGMENT_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("history-segment"),
    ))
    .await
    .unwrap();
    if !history.is_empty() {
        data.append_history(&SEGMENT_SESSION.to_owned(), history)
            .await
            .unwrap();
    }
    (store, data, directory)
}

/// 冻结继承区快照（单行 `inherited_context`）：先祖条目在本线程**没有行**。
async fn freeze_inherited(store: &SqliteThreadStore, payloads: Vec<PersistedPayload>) {
    store
        .store_inherited_context(
            &SEGMENT_SESSION.to_owned(),
            &InheritedContext {
                payloads,
                flags: HashMap::new(),
            },
        )
        .await
        .unwrap();
}

/// 走完一次完整回放：一直取段到 `next_cursor = None`，返回每段的载荷。
async fn replay_segments(
    data: &SqliteSessionData,
    limit: usize,
    byte_budget: usize,
) -> Vec<Vec<PersistedPayload>> {
    let id = SEGMENT_SESSION.to_owned();
    let mut cursor: Option<HistoryForwardCursor> = None;
    let mut segments = Vec::new();
    loop {
        let segment = data
            .load_history_segment(&id, cursor.as_ref(), limit, byte_budget)
            .await
            .unwrap();
        segments.push(segment.payloads);
        match segment.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(segments.len() < 100, "段循环没有收敛");
    }
    segments
}

#[tokio::test]
async fn history_segment_first_read_returns_the_inherited_context_and_fixes_the_tail() {
    let history = messages(3);
    let (store, data, _directory) = session_with_history(&history).await;
    let inherited = messages(2);
    freeze_inherited(&store, inherited.clone()).await;
    let id = SEGMENT_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&first.payloads),
        vec![
            inherited[0].id(),
            inherited[1].id(),
            history[0].id(),
            history[1].id()
        ],
        "继承区整份在最前面（本切片不分页），其后是 rowid 最小的两条自有行"
    );
    let positions = data.load_entry_positions(&id).await.unwrap();
    let cursor = first.next_cursor.clone().expect("更晚方向还有内容");
    assert_eq!(
        cursor.after,
        positions[&history[1].id()].to_string(),
        "游标停在本段推进的最后一条自有行上"
    );
    assert_eq!(
        cursor.tail,
        positions[&history[2].id()].to_string(),
        "tail = 首段那一刻该线程自有行的最大 rowid"
    );

    let second = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(ids(&second.payloads), vec![history[2].id()]);
    assert!(
        second.next_cursor.is_none(),
        "到固定上界即收尾，继承区不重复"
    );
}

#[tokio::test]
async fn history_segment_empty_thread_ends_without_a_cursor() {
    let (_store, data, _directory) = session_with_history(&[]).await;
    let id = SEGMENT_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 5, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert!(first.payloads.is_empty());
    assert!(first.next_cursor.is_none(), "没有自有行就没有下一段");

    // 会话不存在由边界语句自己回答（不另发一次探询）：NotFound，不是空段。
    let error = data
        .load_history_segment(&"missing-session".to_owned(), None, 5, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
}

#[tokio::test]
async fn history_segment_only_inherited_context_has_no_own_rows() {
    let (store, data, _directory) = session_with_history(&[]).await;
    let inherited = messages(3);
    freeze_inherited(&store, inherited.clone()).await;

    let first = data
        .load_history_segment(&SEGMENT_SESSION.to_owned(), None, 1, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&first.payloads),
        ids(&inherited),
        "继承区不分页：段长只约束自有行"
    );
    assert!(first.next_cursor.is_none());
}

#[tokio::test]
async fn history_segment_returns_archived_and_system_rows_in_canonical_order() {
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("m0")),
        PersistedPayload::Message(BaseMessage::system("tool notice")),
        model_only_reminder(),
        PersistedPayload::Message(BaseMessage::human("m1")),
    ];
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = SEGMENT_SESSION.to_owned();

    data.apply_message_projections(
        &id,
        &[(
            history[0].id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();
    // 标记确实生效：有效视图不再包含它（完整回放却必须包含）。
    let active = data.load_active_snapshot(&id).await.unwrap();
    assert!(!active.payloads.iter().any(|p| p.id() == history[0].id()));

    let segments = replay_segments(&data, 2, NO_BUDGET_LIMIT).await;
    assert_eq!(
        segment_ids(&segments),
        ids(&history),
        "完整回放不过滤 excluded / System / model-only reminder，顺序 = canonical（rowid 升序）"
    );
    assert_eq!(segments.len(), 2, "limit=2 时四行分成两段");
}

#[tokio::test]
async fn history_segment_tail_keeps_later_rows_out_of_this_replay() {
    let history = messages(4);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = SEGMENT_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = first.next_cursor.clone().unwrap();

    // 回放进行中追加两行：它们的 rowid 大于 tail，不进入本次遍历。
    let late = messages(2);
    data.append_history(&id, &late).await.unwrap();

    let second = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        ids(&second.payloads),
        vec![history[2].id(), history[3].id()]
    );
    assert!(
        second.next_cursor.is_none(),
        "到 tail 即收尾：追加的行不进入本次回放"
    );

    // 下一次回放从新的上界开始：两行都在（canonical 历史本身没丢）。
    let mut expected = ids(&history);
    expected.extend(ids(&late));
    let segments = replay_segments(&data, 4, NO_BUDGET_LIMIT).await;
    assert_eq!(segment_ids(&segments), expected);
}

#[tokio::test]
async fn history_segment_fails_closed_when_the_after_row_is_gone() {
    let history = messages(4);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = SEGMENT_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = first.next_cursor.clone().unwrap();

    // 锚点行被精确删除：失败关闭，不回退成从头开始，也不用空段冒充回放结束。
    data.remove_history_entries(&id, &[history[1].id()])
        .await
        .unwrap();
    let error = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::Cursor(HistoryCursorError::Expired { .. })
        ),
        "锚点消失必须是失效游标，不是「没有更多历史」"
    );
}

#[tokio::test]
async fn history_segment_gives_an_oversized_entry_its_own_segment() {
    let oversized = PersistedPayload::Message(BaseMessage::ai("x".repeat(1024 * 1024)));
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("before")),
        oversized.clone(),
        PersistedPayload::Message(BaseMessage::human("after")),
    ];
    let (_store, data, _directory) = session_with_history(&history).await;
    let budget = 64 * 1024;

    let segments = replay_segments(&data, 5, budget).await;
    assert_eq!(segment_ids(&segments), ids(&history), "回放不丢内容");
    assert_eq!(segments.len(), 3, "超预算的单条独占一段，前后各成一段");
    assert_eq!(segments[1].len(), 1);
    assert_eq!(
        segments[1][0].as_message().unwrap().content().len(),
        1024 * 1024,
        "超预算的单条正文完整交付"
    );
}

#[tokio::test]
async fn history_segment_length_is_decided_by_bytes_not_limit() {
    let history = messages(4);
    let (_store, data, _directory) = session_with_history(&history).await;
    let one_row = serialize_persisted_payload(&history[3]).unwrap().len();

    // 预算只够一行、limit 很大：段长由字节决定（每段一行）。
    let segments = replay_segments(&data, 10, one_row).await;
    assert_eq!(
        segments.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![1, 1, 1, 1]
    );
    assert_eq!(segment_ids(&segments), ids(&history));
}
