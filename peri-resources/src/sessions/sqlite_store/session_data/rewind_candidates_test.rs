//! 有界回退候选读取（SQLite）的行为与计划证据。
//!
//! 断言口径与 `session_history_perf_test.rs` 一致：计划取自**生产语句原文**
//! （`EXPLAIN QUERY PLAN` 前缀，不复制 SQL），行为断言只看调用方可见结果；
//! 不用内存曲线冒充证据。
//!
//! 关键证据：Ai/Tool 行写成**不可解码**的正文。任何「先全量读回再过滤」的实现都会在
//! 解码处失败；本接口必须只返回 user 候选且成功。

use super::*;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, RewindCandidate,
};
use peri_acp_types::store::{
    serialize_persisted_payload, InheritedContext, PersistedPayload, ThreadStore,
};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use sqlx::SqlitePool;
use std::collections::HashMap;
use tempfile::TempDir;

const SESSION: &str = "rewind-candidates-bounded";
const LIMIT: usize = 64;

async fn database() -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    (store, data, directory)
}

fn frozen(marker: &str) -> FrozenSnapshotBytes {
    FrozenSnapshotBytes::new(format!(r#"{{"version":1,"marker":"{marker}"}}"#))
}

async fn workspace(store: &SqliteThreadStore, cwd: &std::path::Path) -> ResolvedWorkspace {
    store.resolve_workspace(cwd).await.unwrap()
}

fn session(
    id: &str,
    cwd: &str,
    workspace: &ResolvedWorkspace,
    frozen: FrozenSnapshotBytes,
) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-10-24T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: cwd.to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen,
    }
}

/// 建一个空会话（历史由各用例按自己的行形状直接写）。
async fn empty_session() -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(SESSION),
    ))
    .await
    .unwrap();
    (store, data, directory)
}

/// 写入一条可解码的 canonical 行。
async fn insert_payload(pool: &SqlitePool, payload: &PersistedPayload, excluded: bool) {
    let role = match payload {
        PersistedPayload::Message(BaseMessage::Human { .. }) => "user",
        PersistedPayload::Message(BaseMessage::Ai { .. }) => "assistant",
        PersistedPayload::Message(BaseMessage::System { .. }) => "system",
        PersistedPayload::Message(BaseMessage::Tool { .. }) => "tool",
        PersistedPayload::SystemReminder { .. } => "system_reminder",
    };
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content, excluded)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(payload.id().as_uuid().to_string())
    .bind(SESSION)
    .bind(role)
    .bind(serialize_persisted_payload(payload).unwrap())
    .bind(if excluded { 1_i64 } else { 0_i64 })
    .execute(pool)
    .await
    .unwrap();
}

/// 写入一条原始正文行（用于「正文永远不该被解码」的工具行）。
async fn insert_raw(pool: &SqlitePool, id: MessageId, role: &str, content: &str) {
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(id.as_uuid().to_string())
    .bind(SESSION)
    .bind(role)
    .bind(content)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_inherited_context(
    store: &SqliteThreadStore,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: HashMap::new(),
    }
    .to_json()
    .unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(SESSION)
        .execute(&store.database.pool)
        .await
        .unwrap();
}

fn texts(candidates: &[RewindCandidate]) -> Vec<String> {
    candidates.iter().map(|c| c.text.clone()).collect()
}

#[tokio::test]
async fn returns_only_user_rows_newest_first_and_never_decodes_tool_bodies() {
    let (store, data, _directory) = empty_session().await;
    // 每条 user 行之后跟一条大正文、不可解码的 tool 行：全量读取会在解码处失败。
    let user_count = 100;
    let users: Vec<PersistedPayload> = (0..user_count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("message-{index}"))))
        .collect();
    for user in &users {
        insert_payload(&store.database.pool, user, false).await;
        insert_raw(
            &store.database.pool,
            MessageId::new(),
            "tool",
            &format!(
                "{{\"not\":\"a payload\",\"blob\":\"{}\"}}",
                "T".repeat(128 * 1024)
            ),
        )
        .await;
    }

    let candidates = data
        .load_rewind_candidates(&SESSION.to_owned(), LIMIT)
        .await
        .unwrap();

    assert_eq!(candidates.len(), LIMIT, "只返回有界 user 候选");
    assert_eq!(candidates[0].text, "message-99", "最新在前");
    assert_eq!(candidates[63].text, "message-36", "只取最近 64 条");
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.text.starts_with("message-")),
        "工具正文不得进入候选: {:?}",
        texts(&candidates)
    );
}

#[tokio::test]
async fn trailing_pure_reminders_do_not_consume_the_candidate_budget() {
    let (store, data, _directory) = empty_session().await;
    let valid: Vec<PersistedPayload> = (0..LIMIT)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("valid-{index}"))))
        .collect();
    for payload in &valid {
        insert_payload(&store.database.pool, payload, false).await;
    }
    // 130 条纯注入：超过一个 128 行的扫描批，必须继续下一批才能凑满 64 条有效候选。
    for index in 0..130 {
        let reminder = PersistedPayload::Message(BaseMessage::human(format!(
            "<system-reminder>notice-{index}</system-reminder>"
        )));
        insert_payload(&store.database.pool, &reminder, false).await;
    }

    let candidates = data
        .load_rewind_candidates(&SESSION.to_owned(), LIMIT)
        .await
        .unwrap();
    assert_eq!(candidates.len(), LIMIT, "尾部注入不挤掉有效候选");
    assert_eq!(candidates[0].text, "valid-63", "最新有效候选在前");
    assert_eq!(candidates[63].text, "valid-0");
}

#[tokio::test]
async fn archived_users_stay_candidates_and_inherited_humans_follow_own_rows() {
    let (store, data, _directory) = empty_session().await;
    let archived = PersistedPayload::Message(BaseMessage::human("归档问题"));
    let latest = PersistedPayload::Message(BaseMessage::human("自有最新问题"));
    insert_payload(&store.database.pool, &archived, true).await;
    insert_payload(
        &store.database.pool,
        &PersistedPayload::Message(BaseMessage::ai("自有回答")),
        false,
    )
    .await;
    insert_payload(&store.database.pool, &latest, false).await;

    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("继承区问题")),
        PersistedPayload::Message(BaseMessage::ai("继承区回答")),
        PersistedPayload::Message(BaseMessage::human("继承区新问题")),
    ];
    set_inherited_context(&store, &inherited, "parent-session").await;

    let candidates = data
        .load_rewind_candidates(&SESSION.to_owned(), LIMIT)
        .await
        .unwrap();
    assert_eq!(
        texts(&candidates),
        vec!["自有最新问题", "归档问题", "继承区新问题", "继承区问题"],
        "归档 user 仍可见；继承区 Human 排在自有条目之后（最新在前）"
    );
}

#[tokio::test]
async fn limit_zero_returns_no_candidates() {
    let (_store, data, _directory) = empty_session().await;
    let candidates = data
        .load_rewind_candidates(&SESSION.to_owned(), 0)
        .await
        .unwrap();
    assert!(candidates.is_empty());
}

/// 计划证据：生产语句原文 + `EXPLAIN QUERY PLAN`。语句本身含 role 过滤与 LIMIT；
/// 计划必须按索引定位、不得出现 `messages` 全表扫描（不新增索引，结论只报告）。
#[tokio::test]
async fn candidate_rows_sql_is_user_filtered_bounded_and_index_located() {
    use super::rewind_candidates::{REWIND_CANDIDATE_BODY_SQL, REWIND_CANDIDATE_ROWS_SQL};

    assert!(
        REWIND_CANDIDATE_ROWS_SQL.contains("role = 'user'"),
        "阶段一必须只取 user 行: {REWIND_CANDIDATE_ROWS_SQL}"
    );
    assert!(
        REWIND_CANDIDATE_ROWS_SQL.contains("LIMIT ?3"),
        "阶段一必须有界: {REWIND_CANDIDATE_ROWS_SQL}"
    );
    assert!(
        !REWIND_CANDIDATE_ROWS_SQL.contains("excluded"),
        "归档 user 仍是合法目标，不得按 excluded 过滤: {REWIND_CANDIDATE_ROWS_SQL}"
    );
    assert!(
        REWIND_CANDIDATE_BODY_SQL.contains("message_id = ?2"),
        "阶段二按命中 id 取正文: {REWIND_CANDIDATE_BODY_SQL}"
    );

    let (store, _data, _directory) = empty_session().await;
    let pool = &store.database.pool;
    let plan_sql = format!("EXPLAIN QUERY PLAN {REWIND_CANDIDATE_ROWS_SQL}");
    let rows = sqlx::query(sqlx::AssertSqlSafe(plan_sql))
        .bind(SESSION)
        .bind(None::<i64>)
        .bind(128_i64)
        .fetch_all(pool)
        .await
        .unwrap();
    let plan: Vec<String> = rows
        .iter()
        .map(|row| {
            let detail: String = sqlx::Row::try_get(row, 3).unwrap_or_default();
            detail
        })
        .collect();
    println!("REWIND_CANDIDATE_ROWS_SQL: {REWIND_CANDIDATE_ROWS_SQL}");
    println!("EXPLAIN QUERY PLAN: {plan:?}");

    assert!(
        plan.iter().any(|line| line.contains("SEARCH messages")),
        "阶段一必须按索引定位: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("SCAN messages")),
        "阶段一不得全表扫描 messages: {plan:?}"
    );
}
