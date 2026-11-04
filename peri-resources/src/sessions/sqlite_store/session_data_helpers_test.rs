use super::*;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::messages::{BaseMessage, MessageContent};
use peri_acp_types::session_resources::NewSessionMeta;
use peri_acp_types::store::ThreadStore;

fn payload(index: u128, body: String) -> PersistedPayload {
    PersistedPayload::Message(BaseMessage::Human {
        id: uuid::Uuid::from_u128(index + 1).into(),
        content: MessageContent::Text(body),
    })
}

fn encoded_payload(index: u128, bytes: usize) -> PersistedPayload {
    let empty = payload(index, String::new());
    let overhead = serialize_persisted_payload(&empty).unwrap().len();
    payload(index, "x".repeat(bytes.checked_sub(overhead).unwrap()))
}

/// [回归测试] 构造窗口不序列化整批，跨字节边界只保留一条 lookahead。
#[test]
fn test_serialized_history_chunks_are_lazy_and_bounded_by_encoded_bytes() {
    let payloads = [
        encoded_payload(0, MESSAGE_INSERT_MAX_BYTES - 100),
        encoded_payload(1, 200),
        encoded_payload(2, 300),
    ];
    let mut chunks = SerializedHistoryChunks::new(&payloads);
    assert_eq!(chunks.payloads.len(), 3, "构造时不得预消费正文");
    assert!(chunks.pending.is_none());
    let first = chunks.next().unwrap().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].content.len(), MESSAGE_INSERT_MAX_BYTES - 100);
    assert_eq!(chunks.payloads.len(), 1, "不得序列化 lookahead 之后的正文");
    assert_eq!(chunks.pending.as_ref().unwrap().content.len(), 200);
    let second = chunks.next().unwrap().unwrap();
    assert_eq!(second.len(), 2);
    let actual = first
        .into_iter()
        .chain(second)
        .map(|row| row.content)
        .collect::<Vec<_>>();
    let expected = payloads
        .iter()
        .map(|entry| serialize_persisted_payload(entry).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "边界前后正文与顺序须逐字节保真");
    assert!(chunks.next().is_none());
}

/// [回归测试] 恰好触窗立即写出，超窗单条独占一批且完整保留。
#[test]
fn test_serialized_history_chunks_preserve_exact_limit_and_oversized_entry() {
    let payloads = [
        encoded_payload(0, MESSAGE_INSERT_MAX_BYTES / 2),
        encoded_payload(1, MESSAGE_INSERT_MAX_BYTES / 2),
        encoded_payload(2, MESSAGE_INSERT_MAX_BYTES + 1),
        encoded_payload(3, 200),
    ];
    let chunks = SerializedHistoryChunks::new(&payloads)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1, 1]);
    assert_eq!(
        chunks[0].iter().map(|row| row.content.len()).sum::<usize>(),
        MESSAGE_INSERT_MAX_BYTES
    );
    assert_eq!(chunks[1][0].content.len(), MESSAGE_INSERT_MAX_BYTES + 1);
    let actual = chunks
        .into_iter()
        .flatten()
        .map(|row| row.content)
        .collect::<Vec<_>>();
    let expected = payloads
        .iter()
        .map(|entry| serialize_persisted_payload(entry).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "超窗不截断、不跳过，也不与其它条目合批");
}

#[test]
fn test_serialized_history_chunks_also_respect_sql_variable_limit() {
    let count = message_insert_chunk_rows();
    let payloads = (0..count + 1)
        .map(|index| payload(index as u128, "small".into()))
        .collect::<Vec<_>>();
    let chunks = SerializedHistoryChunks::new(&payloads)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [count, 1]);
    assert!(SerializedHistoryChunks::new(&[]).next().is_none());
}

async fn database() -> (
    SqliteThreadStore,
    SqliteSessionData,
    tempfile::TempDir,
    NewSession,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = store.resolve_workspace(directory.path()).await.unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let session = NewSession {
        thread_id: "source".into(),
        created_at: "2026-11-04T00:00:00Z".into(),
        meta: NewSessionMeta {
            title: None,
            cwd: workspace.cwd.to_str().unwrap().into(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(&workspace),
        frozen: FrozenSnapshotBytes::new(r#"{"version":1,"marker":"bounded-write"}"#),
    };
    data.save_new_session(&session).await.unwrap();
    (store, data, directory, session)
}

/// [回归测试] append 与 fork 同用字节窗口，包含超窗条目时正文、顺序及 flags 保真。
#[tokio::test]
async fn test_append_and_fork_preserve_large_entries_across_byte_windows() {
    let (_store, data, _directory, source) = database().await;
    let batch = [
        payload(0, "你好".repeat(100_000)),
        encoded_payload(1, MESSAGE_INSERT_MAX_BYTES + 1),
        encoded_payload(2, MESSAGE_INSERT_MAX_BYTES - 100),
    ];
    data.append_history(&source.thread_id, &batch)
        .await
        .unwrap();
    let appended = data.load_snapshot(&source.thread_id).await.unwrap();
    assert_eq!(appended.meta.message_count, batch.len());
    let bytes = |entries: &[PersistedPayload]| {
        entries
            .iter()
            .map(|entry| serialize_persisted_payload(entry).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(bytes(&appended.payloads), bytes(&batch));
    assert_eq!(appended.meta.title, Some("你好".repeat(25)));
    let fork_payloads = batch
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let BaseMessage::Human { content, .. } = entry.as_message().unwrap() else {
                unreachable!()
            };
            PersistedPayload::Message(BaseMessage::Human {
                id: uuid::Uuid::from_u128(100 + index as u128).into(),
                content: content.clone(),
            })
        })
        .collect::<Vec<_>>();
    let mut target = source.clone();
    target.thread_id = "target".into();
    let flags = HashMap::from([(
        fork_payloads[1].id(),
        MessageFlags {
            excluded: true,
            ..Default::default()
        },
    )]);
    data.save_fork(&ForkSnapshot {
        target: target.clone(),
        source_id: source.thread_id.clone(),
        payloads: fork_payloads.clone(),
        flags: flags.clone(),
    })
    .await
    .unwrap();
    let forked = data.load_snapshot(&target.thread_id).await.unwrap();
    assert_eq!(forked.meta.message_count, fork_payloads.len());
    assert_eq!(bytes(&forked.payloads), bytes(&fork_payloads));
    assert!(forked.flags[&fork_payloads[1].id()].excluded);
    assert_eq!(
        bytes(
            &data
                .load_snapshot(&source.thread_id)
                .await
                .unwrap()
                .payloads
        ),
        bytes(&batch),
        "fork不得更改来源"
    );
}

/// [回归测试] 字节分块后段主键冲突，append 前段已写行与派生计数一起回滚。
#[tokio::test]
async fn test_append_rolls_back_byte_windows_on_late_conflict() {
    let (_store, data, _directory, source) = database().await;
    let existing = payload(100, "original".into());
    data.append_history(&source.thread_id, std::slice::from_ref(&existing))
        .await
        .unwrap();
    let batch = [
        encoded_payload(0, MESSAGE_INSERT_MAX_BYTES),
        encoded_payload(1, MESSAGE_INSERT_MAX_BYTES),
        existing.clone(),
    ];
    let error = data
        .append_history(&source.thread_id, &batch)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { detail } if detail == "session identity already exists"),
        "{error}"
    );
    let after = data.load_snapshot(&source.thread_id).await.unwrap();
    assert_eq!(after.meta.message_count, 1);
    assert_eq!(after.payloads.len(), 1);
    assert_eq!(after.payloads[0].id(), existing.id());
    assert_eq!(after.meta.title.as_deref(), Some("original"));
}

/// [回归测试] fork 后段失败须连同目标归属、绑定及前段正文一起回滚。
#[tokio::test]
async fn test_fork_rolls_back_target_and_byte_windows_on_late_conflict() {
    let (store, data, _directory, source) = database().await;
    let existing = payload(100, "source remains".into());
    data.append_history(&source.thread_id, std::slice::from_ref(&existing))
        .await
        .unwrap();
    let mut target = source.clone();
    target.thread_id = "failed-target".into();
    let fork = ForkSnapshot {
        target: target.clone(),
        source_id: source.thread_id.clone(),
        payloads: vec![
            encoded_payload(0, MESSAGE_INSERT_MAX_BYTES),
            encoded_payload(1, MESSAGE_INSERT_MAX_BYTES),
            existing.clone(),
        ],
        flags: HashMap::new(),
    };
    let error = data.save_fork(&fork).await.unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { detail } if detail == "session identity already exists"),
        "{error}"
    );
    let remnants: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM threads WHERE id = ?1), (SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1), (SELECT COUNT(*) FROM messages WHERE thread_id = ?1)").bind(&target.thread_id).fetch_one(&store.database.pool).await.unwrap();
    assert_eq!(remnants, (0, 0, 0), "失败fork不得留下目标任何一部分");
    let after = data.load_snapshot(&source.thread_id).await.unwrap();
    assert_eq!(after.meta.message_count, 1);
    assert_eq!(after.payloads.len(), 1);
    assert_eq!(after.payloads[0].id(), existing.id());
}
