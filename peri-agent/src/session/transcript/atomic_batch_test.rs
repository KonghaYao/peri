use std::sync::{atomic::AtomicBool, Arc};

use futures::poll;
use parking_lot::RwLock;

use super::*;
use crate::messages::{ContentBlock, MessageContent, ToolCallRequest};
use crate::session::test_resources::mock::{MockSessionResources, ResumeLoadGate};
use crate::session::test_resources::TestSession;

fn exchange(image_count: usize, data_chars: usize) -> Vec<BaseMessage> {
    let calls: Vec<_> = (0..image_count)
        .map(|index| ToolCallRequest::new(format!("image-{index}"), "Read", serde_json::json!({})))
        .collect();
    let mut messages = vec![BaseMessage::ai_with_tool_calls("", calls)];
    for index in 0..image_count {
        messages.push(BaseMessage::tool_result(
            format!("image-{index}"),
            MessageContent::blocks(vec![ContentBlock::image_base64(
                "image/png",
                "A".repeat(data_chars),
            )]),
        ));
    }
    messages
}

fn gated_transcript() -> (
    Arc<RwLock<MessageTranscript>>,
    Arc<MockSessionResources>,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let store = MockSessionResources::new();
    let (entered, enter) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    *store.append_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered,
        release: wait,
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let transcript = Arc::new(RwLock::new(
        MessageTranscript::new().with_persistence(store.clone(), "atomic-exchange".to_owned()),
    ));
    (transcript, store, enter, release)
}

async fn flush(transcript: &Arc<RwLock<MessageTranscript>>) {
    let writer = transcript.read().persist_tx_handle().unwrap();
    MessageTranscript::flush_via_tx(&writer).await.unwrap();
}

/// [回归测试] 同批两个或九个图片结果超过普通积压预算，仍须整批落库且可冷加载。
#[tokio::test]
async fn test_atomic_exchange_large_images_are_durable_and_complete() {
    for count in [2, 9] {
        let session = TestSession::open().await;
        let transcript = Arc::new(RwLock::new(
            MessageTranscript::new()
                .with_persistence(session.resources(), session.thread_id.clone()),
        ));
        let messages = exchange(count, 8 * 1024 * 1024);
        let ids: Vec<_> = messages.iter().map(BaseMessage::id).collect();
        MessageTranscript::commit_tool_exchange(&transcript, messages)
            .await
            .unwrap();
        let loaded = session
            .resources()
            .load_session_snapshot(&session.thread_id)
            .await
            .unwrap();
        assert_eq!(
            loaded
                .payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(transcript.read().len(), count + 1);
        for payload in loaded.payloads.iter().skip(1) {
            let MessageContent::Blocks(blocks) = payload.as_message().unwrap().message_content()
            else {
                panic!("图片结果必须保留多模态内容");
            };
            let ContentBlock::Image {
                source: crate::messages::ImageSource::Base64 { data, .. },
            } = &blocks[0]
            else {
                panic!("图片不得静默降级或丢弃");
            };
            assert_eq!(data.len(), 8 * 1024 * 1024);
        }
        assert_eq!(
            transcript
                .read()
                .persistence_budget()
                .unwrap()
                .outstanding(),
            (0, 0)
        );
        assert!(!transcript.read().compaction_commit_state().is_uncertain());
    }
}

#[tokio::test]
async fn test_atomic_exchange_drains_existing_byte_backlog_first() {
    let store = MockSessionResources::new();
    let prefix = BaseMessage::human("older pending message");
    let prefix_id = prefix.id();
    let bytes = super::super::persistence::Reservation::for_payload(&PersistedPayload::Message(
        prefix.clone(),
    ))
    .bytes;
    let budget = super::super::persistence::PersistenceBudget::new(1, bytes);
    let mut current = MessageTranscript::new().with_persistence_budget(
        store.clone(),
        "ordered-exchange".to_owned(),
        budget.clone(),
    );
    current.append(prefix);
    assert_eq!(budget.outstanding(), (1, bytes));
    let transcript = Arc::new(RwLock::new(current));
    MessageTranscript::commit_tool_exchange(&transcript, exchange(2, bytes))
        .await
        .unwrap();
    assert_eq!(store.append_calls(), 2, "旧积压与不可分交换各提交一次");
    assert_eq!(store.payloads()[0].id(), prefix_id);
    assert_eq!(store.payloads().len(), 4);
    assert_eq!(budget.outstanding(), (0, 0));
    assert!(!transcript.read().has_persistence_failure());
}

#[tokio::test]
async fn test_atomic_exchange_waits_for_store_and_limits_concurrent_batches() {
    let (transcript, store, entered, release) = gated_transcript();
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    assert!(transcript.read().is_empty(), "确认前不能发布部分 canonical");
    let error = MessageTranscript::commit_tool_exchange(&transcript, exchange(1, 8))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already pending"));
    release.send(()).unwrap();
    commit.await.unwrap();
    assert_eq!(store.append_calls(), 1);
    assert_eq!(store.payloads().len(), 3);
    assert_eq!(transcript.read().len(), 3);
    assert!(!transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_caller_drop_keeps_owned_write_and_requires_reload() {
    let (transcript, store, entered, release) = gated_transcript();
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    drop(commit);
    release.send(()).unwrap();
    flush(&transcript).await;
    assert_eq!(
        store.payloads().len(),
        3,
        "writer须完成整交换，不能只留下AI和部分结果"
    );
    assert!(transcript.read().is_empty());
    assert!(transcript.read().compaction_commit_state().is_uncertain());
    assert!(
        !transcript.read().has_persistence_failure(),
        "不确定态不能阻止owned writer收尾"
    );
}

#[tokio::test]
async fn test_atomic_exchange_drop_during_prior_flush_preserves_fifo_completion() {
    let (transcript, store, entered, release) = gated_transcript();
    let prefix = BaseMessage::human("older");
    let prefix_id = prefix.id();
    transcript.write().append(prefix);
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    drop(commit);
    release.send(()).unwrap();
    flush(&transcript).await;
    assert_eq!(store.payloads().len(), 4);
    assert_eq!(store.payloads()[0].id(), prefix_id);
    assert_eq!(store.append_calls(), 2);
    assert_eq!(transcript.read().len(), 1);
    assert!(transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_drop_after_ack_keeps_unconfirmed_view_invalid() {
    let (transcript, store, entered, release) = gated_transcript();
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    release.send(()).unwrap();
    flush(&transcript).await;
    drop(commit);
    assert_eq!(store.payloads().len(), 3);
    assert!(transcript.read().is_empty());
    assert!(transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_shutdown_waits_behind_the_owned_transaction() {
    let (transcript, store, entered, release) = gated_transcript();
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    let writer = transcript.read().persist_tx_handle().unwrap();
    transcript.read().shutdown_persistence();
    drop(commit);
    release.send(()).unwrap();
    writer.closed().await;
    assert_eq!(store.payloads().len(), 3);
    assert!(transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_revision_change_rejects_hot_publication() {
    let (transcript, store, entered, release) = gated_transcript();
    let mut commit = Box::pin(MessageTranscript::commit_tool_exchange(
        &transcript,
        exchange(2, 8),
    ));
    assert!(poll!(commit.as_mut()).is_pending());
    entered.await.unwrap();
    let later = BaseMessage::human("concurrent append");
    let later_id = later.id();
    transcript.write().append(later);
    release.send(()).unwrap();
    let error = commit.await.unwrap_err();
    assert!(error.to_string().contains("changed during exchange commit"));
    flush(&transcript).await;
    let payloads = store.payloads();
    assert_eq!(payloads.len(), 4);
    assert_eq!(
        payloads[3].id(),
        later_id,
        "后续 writer 操作不能越过整批提交"
    );
    assert_eq!(transcript.read().len(), 1);
    assert!(transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_store_failure_preserves_canonical_and_surfaces_error() {
    let session = TestSession::open().await;
    let transcript = Arc::new(RwLock::new(MessageTranscript::new().with_persistence(
        session.read_only_resources().await,
        session.thread_id.clone(),
    )));
    let error = MessageTranscript::commit_tool_exchange(&transcript, exchange(2, 8))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("append_history failed"));
    assert!(transcript.read().is_empty());
    assert!(transcript.read().has_persistence_failure());
    assert!(transcript.read().compaction_commit_state().is_uncertain());
    let loaded = session
        .resources()
        .load_session_snapshot(&session.thread_id)
        .await
        .unwrap();
    assert!(loaded.payloads.is_empty());
}

#[tokio::test]
async fn test_atomic_exchange_rejects_duplicate_ids_before_any_writer_effect() {
    let store = MockSessionResources::new();
    let transcript = Arc::new(RwLock::new(
        MessageTranscript::new().with_persistence(store.clone(), "duplicates".to_owned()),
    ));
    let message = BaseMessage::human("duplicate");
    let error =
        MessageTranscript::commit_tool_exchange(&transcript, vec![message.clone(), message])
            .await
            .unwrap_err();
    assert!(error.to_string().contains("repeats within the batch"));
    assert_eq!(store.append_calls(), 0);
    assert!(transcript.read().is_empty());
    assert!(!transcript.read().compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_atomic_exchange_preserves_a_previous_full_compact_receipt() {
    let store = MockSessionResources::new();
    let current = MessageTranscript::new().with_persistence(store.clone(), "after-full".to_owned());
    current.compaction_commit_state.mark_committed();
    let transcript = Arc::new(RwLock::new(current));
    MessageTranscript::commit_tool_exchange(&transcript, exchange(1, 8))
        .await
        .unwrap();
    assert_eq!(store.payloads().len(), 2);
    assert!(transcript.read().compaction_commit_state().has_committed());
    assert!(!transcript.read().compaction_commit_state().is_uncertain());
}
