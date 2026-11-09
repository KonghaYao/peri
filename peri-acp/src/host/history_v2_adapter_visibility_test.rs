//! FIX-03：前缀未知或读取失败不得当成隐藏（真实 SQLite + 注入读取失败）。
//!
//! 覆盖：1024 字节前缀在 UTF-8 字符中间截断、字段在前缀之外、字段串出现在正文里、
//! 注入前缀读取失败后整页失败且恢复后可重试，以及明确隐藏仍不输出。

use super::*;
use peri_acp_types::history_v2::HistorySnapshotRef;
use peri_acp_types::store::serialize_persisted_payload;

/// 构造一个 canonical 前缀恰好在 UTF-8 字符中间被切断的 TUI reminder。
fn reminder_with_cut_prefix() -> PersistedPayload {
    for filler in 0..8 {
        let body = format!("{}{}", "a".repeat(filler), "🙂".repeat(600));
        let payload = PersistedPayload::SystemReminder {
            id: MessageId::new(),
            reminder: reminder_with_body(&body, vec![ReminderAudience::Tui]),
        };
        let bytes = serialize_persisted_payload(&payload).unwrap().into_bytes();
        if bytes.len() > 1024 && std::str::from_utf8(&bytes[..1024]).is_err() {
            return payload;
        }
    }
    panic!("no filler produced a multibyte cut at the 1024-byte prefix");
}

async fn set_message_role(tmp: &tempfile::TempDir, id: MessageId, role: &str) {
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE messages SET role = ?1 WHERE message_id = ?2")
        .bind(role)
        .bind(id.as_uuid().to_string())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn overwrite_message_content(tmp: &tempfile::TempDir, id: MessageId, content: &str) {
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE messages SET content = ?1 WHERE message_id = ?2")
        .bind(content)
        .bind(id.as_uuid().to_string())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn set_inherited(
    tmp: &tempfile::TempDir,
    sid: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = peri_acp_types::store::InheritedContext {
        payloads: payloads.to_vec(),
        flags: Default::default(),
    }
    .to_json()
    .unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

fn opaque_body(
    opened: &peri_acp_types::history_v2::HistoryOpenedSnapshot,
    id: MessageId,
) -> HistoryBodyDescriptor {
    let suffix = id.as_uuid().to_string();
    let entry = opened
        .page
        .entries
        .iter()
        .find(|entry| entry.entry_id.ends_with(&suffix))
        .unwrap_or_else(|| {
            panic!(
                "entry {suffix} missing; entries={:?}",
                opened
                    .page
                    .entries
                    .iter()
                    .map(|entry| entry.entry_id.as_str())
                    .collect::<Vec<_>>()
            )
        });
    match &entry.sub_items[0] {
        HistorySubItem::Opaque(opaque) => opaque.body.clone(),
        other => panic!("expected opaque subitem, got {other:?}"),
    }
}

async fn read_all(
    backend: &HistoryV2BackendAdapter,
    snapshot: &HistorySnapshotRef,
    body: &HistoryBodyDescriptor,
) -> String {
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let slice = backend
            .body(snapshot, &body.body_ref, offset, 64 * 1024)
            .await
            .expect("body slice");
        let chunk = decode_base64(&slice.data);
        offset += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        if slice.complete {
            break;
        }
    }
    String::from_utf8(bytes).expect("display text")
}

#[tokio::test]
#[serial]
async fn truncated_multibyte_prefix_keeps_reminder_visible() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let payload = reminder_with_cut_prefix();
    let id = payload.id();
    let bytes = serialize_persisted_payload(&payload).unwrap().into_bytes();
    assert!(
        std::str::from_utf8(&bytes[..1024]).is_err(),
        "fixture must cut a UTF-8 sequence at the prefix boundary"
    );
    append_payload(&fixture, payload).await;

    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("a truncated multibyte prefix must not hide the entry");
    assert_eq!(backend.visibility_read_max_bytes(), vec![1024]);
    let body = opaque_body(&opened, id);
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let text = read_all(&backend, &snapshot, &body).await;
    assert!(text.starts_with("提醒："));
    assert!(text.contains('🙂'), "full body must remain readable");
}

#[tokio::test]
#[serial]
async fn unknown_role_and_inherited_use_the_same_prefix_rules() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;

    // own：role 列未知时按内容结构判定。
    let own_payload = reminder_with_cut_prefix();
    let own_id = own_payload.id();
    append_payload(&fixture, own_payload).await;
    set_message_role(&tmp, own_id, "unknown").await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("unknown role open");
    assert_eq!(backend.visibility_read_max_bytes(), vec![1024]);
    opaque_body(&opened, own_id);

    // inherited：没有 role 列，同一套结构化判定。
    let sid = "fix03-inherited-unknown";
    create_empty_session(&fixture.cfg, tmp.path(), sid).await;
    let inherited_payload = reminder_with_cut_prefix();
    let inherited_id = inherited_payload.id();
    set_inherited(&tmp, sid, &[inherited_payload], "parent-session").await;
    backend.reset_visibility_reads();
    let opened = backend
        .open(sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("inherited open");
    assert_eq!(backend.visibility_read_max_bytes(), vec![1024]);
    opaque_body(&opened, inherited_id);
}

#[tokio::test]
#[serial]
async fn undetermined_prefix_fails_the_page_and_retry_reads_the_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let body_text = "可见正文".repeat(4 * 1024);
    let payload = PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: reminder_with_body(&body_text, vec![ReminderAudience::Tui]),
    };
    let id = payload.id();
    let canonical = serialize_persisted_payload(&payload).unwrap();
    append_payload(&fixture, payload).await;

    // audiences 被填充字段推到 1024 字节之外：无法判定，不能成功空页。
    let padding = "p".repeat(1100);
    let undetermined = format!(
        r#"{{"version":1,"type":"system_reminder","id":"{}","padding":"{padding}","reminder":{{"audiences":["tui"],"body":"..."}}}}"#,
        id.as_uuid()
    );
    overwrite_message_content(&tmp, id, &undetermined).await;
    let backend = adapter(&fixture);
    let error = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect_err("undetermined visibility must fail the page");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::SourceUnavailable
    );
    assert_eq!(
        error.recovery(),
        peri_acp_types::history_v2::HistoryRecovery::Retry,
        "undetermined visibility is a retryable source error, not success and not hidden"
    );

    // 恢复 canonical 内容后重试可读。
    overwrite_message_content(&tmp, id, &canonical).await;
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("retry after restoring content");
    let descriptor = opaque_body(&opened, id);
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let text = read_all(&backend, &snapshot, &descriptor).await;
    assert!(text.contains(&body_text));
}

#[tokio::test]
#[serial]
async fn injected_prefix_read_failure_fails_page_then_retry_reads() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let body_text = "r".repeat(4 * 1024);
    let payload = PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: reminder_with_body(&body_text, vec![ReminderAudience::Tui]),
    };
    let id = payload.id();
    append_payload(&fixture, payload).await;

    let backend = adapter(&fixture);
    backend.inject_visibility_read_failures(1);
    let error = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect_err("an I/O failure must not be treated as hidden");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::BackendUnavailable,
        "store read errors keep their original retryable semantics"
    );

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("retry after the injected failure");
    let descriptor = opaque_body(&opened, id);
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let text = read_all(&backend, &snapshot, &descriptor).await;
    assert!(text.contains(&body_text));
}

#[tokio::test]
#[serial]
async fn explicitly_hidden_reminder_is_still_skipped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let payload = PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: reminder_with_body(&"m".repeat(4 * 1024), vec![ReminderAudience::Model]),
    };
    append_payload(&fixture, payload).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("model-only reminder is explicitly hidden");
    assert!(
        opened.page.entries.is_empty(),
        "explicitly hidden reminders stay out of the page"
    );
    assert_eq!(backend.visibility_read_max_bytes(), vec![1024]);
}

fn diagnostic_only_reminder(body: &str) -> PersistedPayload {
    let reminder = SystemReminder {
        version: 1,
        category: ReminderCategory::Guidance,
        source: ReminderSource("history_v2_test".to_owned()),
        kind: "diagnostic_notice".to_owned(),
        severity: ReminderSeverity::Info,
        delivery: ReminderDelivery::DiagnosticOnly,
        audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
        body: body.to_owned(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(reminder)
            .expect("diagnostic-only reminder is a valid DTO"),
    }
}

#[tokio::test]
#[serial]
async fn diagnostic_only_reminder_never_produces_an_opaque_descriptor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let payload = diagnostic_only_reminder(&"d".repeat(4 * 1024));
    let id = payload.id();
    append_payload(&fixture, payload).await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect("diagnostic-only reminder is explicitly hidden");
    assert!(
        opened.page.entries.is_empty(),
        "diagnostic-only reminders must not emit an opaque descriptor"
    );

    // 即使猜到条目身份，body 也必须按同一条出口规则拒绝。
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let error = backend
        .body(&snapshot, &id.as_uuid().to_string(), 0, 1024)
        .await
        .expect_err("diagnostic-only body must stay hidden");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
}

#[tokio::test]
#[serial]
async fn reminder_that_fails_decode_fails_the_page_instead_of_leaking_a_descriptor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let payload = PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: reminder_with_body(&"v".repeat(4 * 1024), vec![ReminderAudience::Tui]),
    };
    let id = payload.id();
    append_payload(&fixture, payload).await;

    // 前缀字段全部合法（configurable + tui），但 metadata 不是对象：完整 payload
    // 无法通过 canonical 解码/validate。页面必须失败关闭，不能猜可见或猜隐藏。
    let invalid_metadata = format!(
        r#"{{"version":1,"type":"system_reminder","id":"{}","reminder":{{"version":1,"category":"guidance","source":"test","kind":"notice","severity":"info","delivery":"configurable","audiences":["tui"],"body":"...","metadata":"not-an-object"}}}}"#,
        id.as_uuid()
    );
    overwrite_message_content(&tmp, id, &invalid_metadata).await;

    let backend = adapter(&fixture);
    let error = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64)
        .await
        .expect_err("undecodable reminder must fail the page");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::SourceUnavailable,
        "body would also return source_unavailable for the same content"
    );
}
