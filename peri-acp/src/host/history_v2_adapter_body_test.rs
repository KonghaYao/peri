//! History V2 body 的展示投影与有界读取（R1）。
//!
//! 从 `history_v2_adapter_test.rs` 拆出以维持单文件 ≤1000 行；`use super::*` 复用父模块
//! 的真实 SQLite 夹具、descriptor/分块 helper 与适配器构造。

use super::*;
use peri_acp_types::messages::ToolCallRequest;

#[tokio::test]
#[serial]
async fn oversized_tool_call_and_result_bodies_render_semantic_text() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let call_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            peri_acp_types::messages::MessageContent::Text("invoking tool".to_owned()),
            vec![ToolCallRequest::new(
                "call-big",
                "big_tool",
                serde_json::json!({"blob": "i".repeat(70 * 1024)}),
            )],
        )),
    )
    .await;
    let result_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::tool_result("call-big", "o".repeat(70 * 1024))),
    )
    .await;
    let backend = adapter(&fixture);
    let call_opened = open_at(&backend, &fixture.sid, call_id).await;
    let call_snapshot = snapshot_of(&call_opened, &fixture.sid);
    let call = descriptor_of(&call_opened, call_id);
    let (call_text, call_total, _) =
        read_whole_body(&backend, &call_snapshot, &call.body_ref).await;
    assert!(call_text.contains("工具调用：big_tool"));
    assert!(call_text.contains("状态：in_progress"));
    assert!(call_text.contains("输入："));
    assert!(call_text.contains("\"blob\""));
    assert!(call_text.contains("（输入已截断）"));
    assert!(!call_text.contains("\"role\":\"assistant\""));
    assert_eq!(call_total, call_text.len() as u64);

    let result_opened = open_at(&backend, &fixture.sid, result_id).await;
    let result_snapshot = snapshot_of(&result_opened, &fixture.sid);
    let result = descriptor_of(&result_opened, result_id);
    let (result_text, result_total, _) =
        read_whole_body(&backend, &result_snapshot, &result.body_ref).await;
    assert!(result_text.contains("工具结果：completed"));
    assert!(result_text.contains("输出："));
    assert!(result_text.contains(&"o".repeat(1024)));
    assert!(!result_text.contains("\"role\":\"tool\""));
    assert_eq!(result_total, result_text.len() as u64);
}

#[tokio::test]
#[serial]
async fn content_version_is_stable_per_content_not_a_constant() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let first_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai("a".repeat(70 * 1024))),
    )
    .await;
    let second_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai("b".repeat(70 * 1024))),
    )
    .await;
    let backend = adapter(&fixture);
    let first_opened = open_at(&backend, &fixture.sid, first_id).await;
    let first_snapshot = snapshot_of(&first_opened, &fixture.sid);
    let first = descriptor_of(&first_opened, first_id);
    let (_, _, first_version) = read_whole_body(&backend, &first_snapshot, &first.body_ref).await;
    let (_, _, first_again) = read_whole_body(&backend, &first_snapshot, &first.body_ref).await;
    assert_eq!(first_version, first_again, "同一内容跨请求稳定");
    assert_eq!(first_version.len(), 32);
    assert_ne!(first_version, "1");

    let second_opened = open_at(&backend, &fixture.sid, second_id).await;
    let second_snapshot = snapshot_of(&second_opened, &fixture.sid);
    let second = descriptor_of(&second_opened, second_id);
    let (_, _, second_version) =
        read_whole_body(&backend, &second_snapshot, &second.body_ref).await;
    assert_ne!(
        first_version, second_version,
        "内容不同不能复用同一个 contentVersion"
    );
}

#[tokio::test]
#[serial]
async fn oversized_payload_above_limit_returns_budget_exceeded_with_bounded_reads() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai("a".repeat(8 * 1024 * 1024 + 1))),
    )
    .await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let descriptor = descriptor_of(&opened, id);

    let error = backend
        .body(&snapshot, &descriptor.body_ref, 0, 64 * 1024)
        .await
        .expect_err("over-limit body must fail");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::BudgetExceeded
    );
    assert_eq!(
        error.recovery(),
        peri_acp_types::history_v2::HistoryRecovery::ReduceScope
    );
    assert!(
        backend.payload_read_max_bytes().is_empty(),
        "advertised over-limit payload must not be read at all"
    );
    let read_bytes: usize = backend
        .payload_read_max_bytes()
        .iter()
        .map(|bytes| *bytes as usize)
        .sum();
    assert!(
        read_bytes <= 8 * 1024 * 1024,
        "must stop at the 8 MiB cap, read {read_bytes} bytes"
    );
}

#[tokio::test]
#[serial]
async fn undecodable_body_maps_to_source_unavailable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai("x".repeat(70 * 1024))),
    )
    .await;
    // 存储层损坏：正文仍是超大行，但不是合法 persisted envelope。
    overwrite_message_content(&tmp, id, &format!("{{\"broken\":{}", "x".repeat(70 * 1024))).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let descriptor = descriptor_of(&opened, id);
    let error = backend
        .body(&snapshot, &descriptor.body_ref, 0, 1024)
        .await
        .expect_err("corrupt body must not be returned");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::SourceUnavailable
    );
}

#[tokio::test]
#[serial]
async fn payload_without_visible_projection_maps_to_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai(
            peri_acp_types::messages::MessageContent::raw(vec![serde_json::json!({
                "type": "vendor_native",
                "blob": "x".repeat(70 * 1024),
            })]),
        )),
    )
    .await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let descriptor = descriptor_of(&opened, id);
    let error = backend
        .body(&snapshot, &descriptor.body_ref, 0, 1024)
        .await
        .expect_err("no visible updates must not return an empty body");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
}

async fn open_at(
    backend: &HistoryV2BackendAdapter,
    session_id: &str,
    message_id: MessageId,
) -> peri_acp_types::history_v2::HistoryOpenedSnapshot {
    backend
        .open(
            session_id,
            &HistoryAnchor::Entry(HistoryEntryAnchor {
                source: V2Source::Own,
                entry_id: message_id.as_uuid().to_string(),
            }),
            0,
            0,
            64 * 1024,
        )
        .await
        .expect("open opaque entry")
}

fn descriptor_of(
    opened: &peri_acp_types::history_v2::HistoryOpenedSnapshot,
    message_id: MessageId,
) -> HistoryBodyDescriptor {
    let suffix = message_id.as_uuid().to_string();
    let entry = opened
        .page
        .entries
        .iter()
        .find(|entry| entry.entry_id.ends_with(&suffix))
        .unwrap_or_else(|| {
            panic!(
                "opaque entry {suffix}: entries={:?}",
                opened
                    .page
                    .entries
                    .iter()
                    .map(|entry| (entry.entry_id.as_str(), entry.sub_items.len()))
                    .collect::<Vec<_>>()
            )
        });
    match &entry.sub_items[0] {
        HistorySubItem::Opaque(opaque) => opaque.body.clone(),
        other => panic!("expected opaque subitem, got {other:?}"),
    }
}

/// 顺序读取完整 body，返回（UTF-8 文本、权威总长、contentVersion）。
async fn read_whole_body(
    backend: &HistoryV2BackendAdapter,
    snapshot: &peri_acp_types::history_v2::HistorySnapshotRef,
    body_ref: &str,
) -> (String, u64, String) {
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut total = 0u64;
    let mut version = String::new();
    loop {
        let slice = backend
            .body(snapshot, body_ref, offset, 64 * 1024)
            .await
            .expect("body slice");
        if offset == 0 {
            total = slice.total_bytes.get();
            version = slice.content_version.clone();
        }
        assert_eq!(slice.total_bytes.get(), total, "total must stay stable");
        let chunk = decode_base64(&slice.data);
        offset += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        if slice.complete {
            assert_eq!(offset, total, "complete body must reach totalBytes");
            break;
        }
        assert_eq!(slice.next_offset, Some(DecimalU64::new(offset)));
    }
    (
        String::from_utf8(bytes).expect("display text is UTF-8"),
        total,
        version,
    )
}

/// 存储层损坏夹具：直接改写 canonical 正文，但保持行仍为超大条目。
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
