//! 提交 tail 单调守卫、票据级 oversize 记忆与二次校验失败不写入的单元回归。

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde_json::Value;

use peri_acp_types::history_window::{
    HistoryCheckpoint, HistoryCommittedTail, HistorySource, HistoryWindowMetadata,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;

use crate::host::{ContextOwnership, SessionState};
use crate::session::agent_pool::AgentPool;
use crate::transport::types::AcpError;

use super::oversize::{
    OversizeMemory, OVERSIZE_DESCRIPTOR_BYTE_BUDGET, OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES,
};
use super::{finalize, HistoryWindowCoordinator, WindowConnection, WindowNotificationSink};

fn tail(position: u64) -> HistoryCommittedTail {
    HistoryCommittedTail::new(
        &ThreadId::from("session-1".to_owned()),
        MessageId::new(),
        position,
    )
}

fn checkpoint(id: &str) -> HistoryCheckpoint {
    HistoryCheckpoint {
        checkpoint_id: id.to_owned(),
        summary_entry_id: None,
        position: 0,
    }
}

fn coordinator_with(checkpoint_id: &str, position: u64) -> HistoryWindowCoordinator {
    let mut coordinator = HistoryWindowCoordinator::default();
    coordinator.publish_commit(Some(&checkpoint(checkpoint_id)), Some(&tail(position)));
    coordinator
}

#[test]
fn regressed_tail_is_rejected_for_same_and_different_checkpoints() {
    let mut coordinator = coordinator_with("cp-a", 10);

    // 同一 checkpoint 回退：整体拒绝，checkpoint/tail 都不动。
    coordinator.publish_commit(Some(&checkpoint("cp-a")), Some(&tail(9)));
    assert_eq!(coordinator.checkpoint_id.as_deref(), Some("cp-a"));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 10);

    // 不同 checkpoint 回退：同样整体拒绝，不切换提交身份。
    coordinator.publish_commit(Some(&checkpoint("cp-b")), Some(&tail(9)));
    assert_eq!(coordinator.checkpoint_id.as_deref(), Some("cp-a"));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 10);

    // checkpoint=None 也不能绕过单调整体守卫：回退不得清空已发布事实。
    // 已知清空路径先走 finish_rewind/invalidate。
    coordinator.publish_commit(None, Some(&tail(9)));
    assert_eq!(coordinator.checkpoint_id.as_deref(), Some("cp-a"));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 10);
}

#[test]
fn advancing_or_equal_tail_is_accepted() {
    let mut coordinator = coordinator_with("cp-a", 10);

    // 同 checkpoint 前进（后续 append）仍更新 tail。
    coordinator.publish_commit(Some(&checkpoint("cp-a")), Some(&tail(12)));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 12);

    // 相同位置幂等。
    coordinator.publish_commit(Some(&checkpoint("cp-a")), Some(&tail(12)));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 12);

    // 不同 checkpoint 前进接受并切换身份。
    coordinator.publish_commit(Some(&checkpoint("cp-b")), Some(&tail(13)));
    assert_eq!(coordinator.checkpoint_id.as_deref(), Some("cp-b"));
    assert_eq!(coordinator.summary_entry_id.as_deref(), Some("cp-b"));
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 13);
}

#[test]
fn none_checkpoint_clears_when_the_tail_does_not_regress() {
    let mut coordinator = coordinator_with("cp-a", 10);

    coordinator.publish_commit(None, Some(&tail(11)));
    assert!(coordinator.checkpoint_id.is_none());
    assert!(coordinator.summary_entry_id.is_none());
    assert_eq!(coordinator.committed_tail.as_ref().unwrap().position, 11);

    coordinator.publish_commit(None, None);
    assert!(coordinator.committed_tail.is_none());
}

#[test]
fn memory_cap_clears_entries_and_never_grows_past_the_limit() {
    let mut memory = OversizeMemory::new();
    let entries: Vec<(MessageId, u64)> = (0..OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES)
        .map(|_| (MessageId::new(), 1))
        .collect();
    memory.remember(&entries);
    assert_eq!(memory.len(), OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES);
    assert!(!memory.overflowed());

    memory.remember(&[(MessageId::new(), 1)]);
    assert!(memory.overflowed());
    assert_eq!(memory.len(), 0, "超过上限必须整体清空");

    memory.remember(&entries);
    assert_eq!(memory.len(), 0, "overflow 后不再随登记增长");
}

#[test]
fn descriptor_budget_stops_registration_and_overflow_forces_revalidation() {
    let mut memory = OversizeMemory::new();
    let first = MessageId::new();
    memory.remember(&[(first, OVERSIZE_DESCRIPTOR_BYTE_BUDGET)]);
    assert!(memory.remembers(first));
    assert!(!memory.overflowed());

    let second = MessageId::new();
    memory.remember(&[(second, 1)]);
    assert!(memory.overflowed());
    // 溢出后不能凭「不在集合里」拒绝，必须返回 true 让调用方回存储重取校验。
    assert!(memory.remembers(second));

    // 重复登记同一身份不重复扣预算（seek 可能再次返回同一 descriptor）。
    let mut memory = OversizeMemory::new();
    memory.remember(&[(first, OVERSIZE_DESCRIPTOR_BYTE_BUDGET)]);
    memory.remember(&[(first, OVERSIZE_DESCRIPTOR_BYTE_BUDGET)]);
    assert!(!memory.overflowed());
    assert!(memory.remembers(first));
}

struct NoopSink;

#[async_trait::async_trait]
impl WindowNotificationSink for NoopSink {
    async fn send(&self, _session_id: &str, _payload: Value) -> Result<(), AcpError> {
        Ok(())
    }
}

fn state_with(
    coordinator: HistoryWindowCoordinator,
    generation: u64,
    structure_version: u64,
) -> SessionState {
    SessionState {
        session_id: "session-1".to_owned(),
        thread_id: "thread-1".to_owned(),
        cwd: "/tmp".to_owned(),
        environment: None,
        closing: false,
        context_ownership: ContextOwnership::Resident,
        history_payloads: vec![],
        structure_version,
        session_generation: generation,
        history_window: coordinator,
        event_health: Default::default(),
        cancel_token: None,
        interaction_gate: Arc::new(tokio::sync::Mutex::new(())),
        host_request_seq: Arc::new(AtomicU64::new(0)),
        continuation_armed: false,
        continuation_epoch: 0,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
        frozen: None,
        recall_items: vec![],
        agent_pool: AgentPool::new(),
        title: None,
        tags: vec![],
    }
}

#[tokio::test]
async fn failed_finalize_does_not_pollute_oversize_memory() {
    let connection = WindowConnection::new("connection-1", Arc::new(NoopSink));
    let metadata = HistoryWindowMetadata {
        session_id: ThreadId::from("session-1".to_owned()),
        session_generation: "7".to_owned(),
        structure_version: 3,
        snapshot_token: "snapshot-1".to_owned(),
        committed_tail: None,
        checkpoint_id: None,
    };
    let prepared =
        HistoryWindowCoordinator::prepare(metadata, HistorySource::Own, &connection, false, None);
    let mut coordinator = HistoryWindowCoordinator::default();
    let issued = coordinator.publish(prepared);
    let sessions = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        "session-1".to_owned(),
        state_with(coordinator, 7, 3),
    )])));
    let entry = MessageId::new();

    // generation 不匹配：二次校验失败，descriptor 不得进入票据记忆。
    let error = finalize(&sessions, "session-1", 8, 3, &issued, &[(entry, 1)])
        .await
        .unwrap_err();
    assert_eq!(error.code, -32012);
    assert!(
        !issued.remembers_oversize(entry),
        "failed finalize must not pollute oversize memory"
    );

    // 校验通过后才登记。
    finalize(&sessions, "session-1", 7, 3, &issued, &[(entry, 1)])
        .await
        .expect("matching generation/structure must pass");
    assert!(issued.remembers_oversize(entry));
}
