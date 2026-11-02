use super::*;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::tasks::BgTaskKind;

use crate::session::test_resources::TestSession;

#[tokio::test(start_paused = true)]
async fn test_child_window_starts_once_and_expires_without_activity() {
    let window = ChildSettlementWindow::default();
    assert_eq!(window.deadline(), None, "正常执行尚未进入收尾");
    let deadline = window.start();
    for _ in 0..119 {
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(window.start(), deadline, "唤醒不得续期");
        assert!(!window.expired());
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(window.expired(), "无外部活动也必须到期");
    assert_eq!(window.start(), deadline, "到期后再次启动也不能重置");
    let resumed = ChildSettlementWindow::default();
    assert!(resumed.start() > deadline, "显式新 run 有独立窗口");
}

fn task(id: &str, owner: Option<&str>) -> PendingHandoffTask {
    PendingHandoffTask {
        task_id: id.into(),
        kind: BgTaskKind::Shell,
        owner_session_id: owner.map(str::to_owned),
        owner_identity: Some("mcp:workspace:Builtin".into()),
    }
}

fn task_set() -> Vec<PendingHandoffTask> {
    vec![
        task("mcp-aaa", Some("root-a")),
        task("mcp-bbb", Some("root-a")),
    ]
}

fn handoff(tasks: Vec<PendingHandoffTask>, waited_ms: u64) -> PendingHandoff {
    PendingHandoff {
        tasks,
        waited: Duration::from_millis(waited_ms),
    }
}

fn canonical_payload(tasks: &[PendingHandoffTask]) -> Vec<u8> {
    let normalized = normalized_pending_tasks(tasks);
    canonical_handoff_payload(&handoff_reminder(&normalized).expect("canonical reminder"))
        .expect("canonical payload")
}

fn delivery_id_for(tasks: &[PendingHandoffTask], thread: &str) -> MessageId {
    handoff_delivery_id(&thread.to_owned(), &canonical_payload(tasks))
}

/// 用真实 SQLite 门面构造一个只含 transcript 的会话句柄（交接只读 transcript 端口）。
fn session_handle(store: Arc<dyn SessionResources>, thread: &str) -> SessionHandle {
    let session = crate::session::Session::new(
        Arc::from("/tmp/handoff-contract"),
        crate::session::FrozenContext::builder().build(),
        Some(thread.to_owned()),
    );
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(store, thread.to_owned());
    crate::agent::stages::StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .session
}

fn reminder_payloads(payloads: &[PersistedPayload]) -> Vec<(MessageId, &TrustedSystemReminder)> {
    payloads
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::SystemReminder { id, reminder } => Some((*id, reminder)),
            PersistedPayload::Message(_) => None,
        })
        .collect()
}

/// 事故前的 v1 身份：只有 domain + task_id + owner_session_id，与载荷中的等待时长脱节。
fn legacy_v1_delivery_id(tasks: &[PendingHandoffTask]) -> MessageId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"bounded-wait-handoff");
    for task in tasks {
        hasher.update((task.task_id.len() as u64).to_be_bytes());
        hasher.update(task.task_id.as_bytes());
        let owner = task.owner_session_id.as_deref().unwrap_or_default();
        hasher.update((owner.len() as u64).to_be_bytes());
        hasher.update(owner.as_bytes());
    }
    let digest = hasher.finalize();
    digest_to_message_id(&digest)
}

/// 事故前的 v1 载荷：等待时长写在稳定 ID 的消息里。
fn legacy_v1_reminder(tasks: &[PendingHandoffTask], waited_ms: u64) -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalReminderSource("handoff".into()),
            kind: "pending_handoff".into(),
            severity: ReminderSeverity::Error,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: "legacy v1 bounded-wait handoff".into(),
            summary: None,
            metadata: serde_json::json!({
                "pending": tasks.len(),
                "waited_ms": waited_ms,
            }),
        })
        .unwrap()
}

/// 同 ID 异内容：模拟另一个写入者占用了本次将使用的交付 ID。
fn conflicting_reminder() -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalReminderSource("handoff".into()),
            kind: "pending_handoff".into(),
            severity: ReminderSeverity::Error,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "another writer's pending handoff".into(),
            summary: None,
            metadata: serde_json::json!({"pending": 1}),
        })
        .unwrap()
}

fn digest_to_message_id(digest: &[u8]) -> MessageId {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    MessageId::from(uuid::Uuid::from_bytes(bytes))
}

// 到点路径：忙碌时先等待，超过上限后不再等待并恰好报告一次交接到期。
#[test]
fn test_bounded_wait_expires_once_after_the_bound() {
    let wait = BoundedWait::new(Duration::from_millis(40));
    assert!(wait.should_wait(true));
    assert!(!wait.take_due());
    std::thread::sleep(Duration::from_millis(60));
    assert!(!wait.should_wait(true), "超过上限后不得继续无限等待");
    assert!(wait.take_due(), "到点必须报告一次交接到期");
    assert!(!wait.take_due(), "交接到期只报告一次");
}

// 关键路径：任务结算后等待窗口复位，不会被上一轮的历史时长提前触发交接。
#[test]
fn test_bounded_wait_resets_when_work_settles() {
    let wait = BoundedWait::new(Duration::from_secs(30));
    assert!(wait.should_wait(true));
    assert!(!wait.should_wait(false), "无未结算任务时不得等待");
    assert!(!wait.take_due());
    assert!(wait.should_wait(true), "新的未结算任务重新起算等待窗口");
    assert!(!wait.take_due());
}

// 交接内容契约：pending 计数、任务身份与 scope owner 都必须可读；等待时长只进日志。
#[test]
fn test_handoff_reminder_records_pending_tasks_and_scope_owner() {
    let tasks = vec![task("mcp-aaa", Some("root-a")), task("bg-bbb", None)];
    let reminder = handoff_reminder(&tasks).unwrap();
    let body = &reminder.as_reminder().body;
    assert!(body.starts_with("pending: 2"), "{body}");
    assert!(body.contains("mcp-aaa"), "{body}");
    assert!(body.contains("bg-bbb"), "{body}");
    assert!(body.contains("root-a"), "{body}");
    assert!(
        body.contains("reconciled by the scope owner"),
        "交接必须说明结果由 scope owner 对账补投: {body}"
    );
    assert_eq!(reminder.as_reminder().kind, HANDOFF_REMINDER_KIND);
    let metadata = &reminder.as_reminder().metadata;
    assert_eq!(metadata["pending"], 2);
    assert_eq!(
        metadata.get("waited_ms"),
        None,
        "等待时长是本次观测值，必须留在 tracing，不能进入稳定身份的 canonical 载荷"
    );
    assert_eq!(metadata["tasks"][0]["task_id"], "mcp-aaa");
}

// 身份闭合：同集合换序收敛；kind / owner_identity / 目标 thread 的真实变化必须改变身份。
#[test]
fn test_handoff_delivery_id_is_stable_for_the_same_pending_set() {
    let first = vec![
        task("mcp-aaa", Some("root-a")),
        task("mcp-bbb", Some("root-a")),
    ];
    let reordered = vec![
        task("mcp-bbb", Some("root-a")),
        task("mcp-aaa", Some("root-a")),
    ];
    assert_eq!(
        canonical_payload(&first),
        canonical_payload(&reordered),
        "换序必须产生完全相同的 canonical 载荷"
    );
    assert_eq!(
        delivery_id_for(&first, "root-a"),
        delivery_id_for(&reordered, "root-a"),
        "换序必须收敛到同一投递 ID"
    );
    assert_ne!(
        delivery_id_for(&first, "root-a"),
        delivery_id_for(&first, "root-b"),
        "不同目标 thread 的消息身份必须隔离"
    );
    assert_ne!(
        delivery_id_for(&first, "root-a"),
        delivery_id_for(&[task("mcp-aaa", Some("root-b"))], "root-a"),
        "scope owner 变化必须改变身份"
    );
    let mut other_kind = first.clone();
    other_kind[0].kind = BgTaskKind::Mcp;
    assert_ne!(
        delivery_id_for(&first, "root-a"),
        delivery_id_for(&other_kind, "root-a"),
        "任务种类变化必须改变身份"
    );
    let mut other_owner_identity = first.clone();
    other_owner_identity[0].owner_identity = Some("mcp:other:instance".into());
    assert_ne!(
        delivery_id_for(&first, "root-a"),
        delivery_id_for(&other_owner_identity, "root-a"),
        "owner identity 变化必须改变身份"
    );
}

// 旧 v1 身份与 v2 必须隔离：已受影响的会话不会被旧行继续拖住。
#[test]
fn test_handoff_delivery_id_is_isolated_from_the_legacy_v1_identity() {
    let tasks = task_set();
    let legacy = legacy_v1_delivery_id(&tasks);
    let current = delivery_id_for(&tasks, "root-a");
    assert_ne!(legacy, current, "v2 必须使用新 domain，不得复用旧 key");
    assert_eq!(
        current,
        delivery_id_for(&tasks, "root-a"),
        "同一输入必须复现同一身份"
    );
}

// 核心路径：有界交接快照携带公开任务身份与 scope owner/owner identity。
#[tokio::test]
async fn test_pending_handoff_snapshot_carries_owner_identity() {
    use peri_acp_types::tasks::ExternalTaskRegistration;
    let manager = crate::agent::async_tasks::TaskManager::new();
    let task_id = manager
        .register_external(ExternalTaskRegistration {
            session_id: "root-session".into(),
            initiator_session_id: Some("child-session".into()),
            owner_identity: "mcp:workspace:Builtin".into(),
            owner_task_id: "raw-1".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300".into(),
            started_at: None,
            cancel: std::sync::Arc::new(|| Box::pin(async { Ok(()) })),
            on_terminal: std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) })),
        })
        .unwrap();
    let pending = manager.pending_handoff_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].task_id, task_id.task_id());
    assert_eq!(pending[0].owner_session_id.as_deref(), Some("root-session"));
    assert_eq!(
        pending[0].owner_identity.as_deref(),
        Some("mcp:workspace:Builtin")
    );
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session"),
        "聚合条目必须携带投递归属"
    );
}

// T1：同一 store、同一会话、相同任务，连续两次交接（等待耗时差 1ms）必须成功，
// 且只留下一条 ID 与完整载荷都相同的 canonical 记录。真实本地 SQLite adapter。
#[tokio::test]
async fn test_repeated_handoff_converges_in_the_local_store() {
    let fixture = TestSession::open().await;
    let thread_id = fixture.thread_id();
    let handle = session_handle(fixture.resources(), &thread_id);
    let tasks = task_set();

    let first = write_pending_handoff(&handle, &handoff(tasks.clone(), 120_001))
        .await
        .expect("第一次交接必须成功");
    let second = write_pending_handoff(&handle, &handoff(tasks, 120_002))
        .await
        .expect("等待耗时变化不得让同一集合的第二次交接失败");
    assert_eq!(first, second, "同一 pending 集合必须收敛到同一投递 ID");
    let HandoffCommit::Committed(delivery_id) = first else {
        panic!("存在未结任务时必须写成交接");
    };

    let snapshot = fixture
        .resources()
        .load_session_snapshot(&fixture.thread_id())
        .await
        .unwrap();
    let reminders = reminder_payloads(&snapshot.payloads);
    assert_eq!(reminders.len(), 1, "重复交接不得新增第二条记录");
    assert_eq!(reminders[0].0, delivery_id);
    assert_eq!(
        reminders[0].1.as_reminder().metadata.get("waited_ms"),
        None,
        "canonical 载荷不得携带本次等待时长"
    );
}

// T3：数据库已有 v1 交接时，修复后的交接首次写入新 key 成功；重放不再新增，
// 且原 v1 行保持完整。
#[tokio::test]
async fn test_legacy_v1_handoff_row_does_not_block_the_canonical_contract() {
    let fixture = TestSession::open().await;
    let thread_id = fixture.thread_id();
    let tasks = task_set();
    let legacy_id = legacy_v1_delivery_id(&tasks);
    let legacy = legacy_v1_reminder(&tasks, 120_001);
    assert!(fixture
        .resources()
        .append_reminder_if_absent(&thread_id, legacy_id, &legacy)
        .await
        .unwrap());

    let handle = session_handle(fixture.resources(), &thread_id);
    let first = write_pending_handoff(&handle, &handoff(tasks.clone(), 120_002))
        .await
        .expect("v1 旧行不得阻塞 v2 首次交接");
    let HandoffCommit::Committed(delivery_id) = first else {
        panic!("存在未结任务时必须写成交接");
    };
    assert_ne!(delivery_id, legacy_id, "v2 必须与旧身份隔离");
    let replay = write_pending_handoff(&handle, &handoff(tasks, 120_003))
        .await
        .expect("v2 重放必须成功");
    assert_eq!(replay, HandoffCommit::Committed(delivery_id));

    let snapshot = fixture
        .resources()
        .load_session_snapshot(&fixture.thread_id())
        .await
        .unwrap();
    let reminders = reminder_payloads(&snapshot.payloads);
    assert_eq!(reminders.len(), 2, "v1 原行保留，v2 只新增一条");
    let legacy_row = reminders
        .iter()
        .find(|(id, _)| *id == legacy_id)
        .expect("v1 原记录必须保留");
    assert_eq!(
        legacy_row.1.as_reminder().metadata["waited_ms"],
        120_001,
        "v1 原记录不得被覆盖"
    );
}

// T4：规范消息已 excluded 时重复同集合交接不报冲突，也不把归档正文回灌热态。
#[tokio::test]
async fn test_excluded_handoff_replays_without_refilling_the_hot_view() {
    let fixture = TestSession::open().await;
    let thread_id = fixture.thread_id();
    let tasks = task_set();
    let first_handle = session_handle(fixture.resources(), &thread_id);
    let first = write_pending_handoff(&first_handle, &handoff(tasks.clone(), 120_001))
        .await
        .unwrap();
    let HandoffCommit::Committed(delivery_id) = first else {
        panic!("存在未结任务时必须写成交接");
    };
    fixture
        .resources()
        .apply_message_projections(
            &thread_id,
            &[(
                delivery_id,
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )],
        )
        .await
        .unwrap();

    // Full Compact 后热态已释放该条目：用同一 store 的新热态重放。
    let handle = session_handle(fixture.resources(), &thread_id);
    let replayed = write_pending_handoff(&handle, &handoff(tasks, 120_002))
        .await
        .expect("已排除的规范消息不得让重复交接失败");
    assert_eq!(replayed, HandoffCommit::Committed(delivery_id));
    assert!(
        handle.transcript.read().get(delivery_id).is_none(),
        "已排除的归档正文不得回灌热态"
    );
}

// T8：真正的同 ID 异内容仍然显式失败，不能借本次修复吞掉身份冲突。
#[tokio::test]
async fn test_conflicting_handoff_payload_still_fails_explicitly() {
    let fixture = TestSession::open().await;
    let thread_id = fixture.thread_id();
    let tasks = task_set();
    let delivery_id = delivery_id_for(&tasks, &thread_id);
    assert!(fixture
        .resources()
        .append_reminder_if_absent(&thread_id, delivery_id, &conflicting_reminder())
        .await
        .unwrap());

    let handle = session_handle(fixture.resources(), &thread_id);
    let error = write_pending_handoff(&handle, &handoff(tasks, 120_001))
        .await
        .expect_err("同 ID 异内容必须失败");
    assert!(
        matches!(error, HandoffCommitError::Persistence(_)),
        "必须保持显式持久化失败语义，实际 {error:?}"
    );

    let snapshot = fixture
        .resources()
        .load_session_snapshot(&fixture.thread_id())
        .await
        .unwrap();
    let reminders = reminder_payloads(&snapshot.payloads);
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].0, delivery_id);
    assert_eq!(
        reminders[0].1.as_reminder().body,
        "another writer's pending handoff",
        "冲突时不得覆盖既有记录"
    );
}
