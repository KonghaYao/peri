use super::*;
use peri_acp_types::tasks::BgTaskKind;

fn task(id: &str, owner: Option<&str>) -> PendingHandoffTask {
    PendingHandoffTask {
        task_id: id.into(),
        kind: BgTaskKind::Shell,
        owner_session_id: owner.map(str::to_owned),
        owner_identity: Some("mcp:workspace:Builtin".into()),
    }
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

// 交接内容契约：pending 计数、任务身份与 scope owner 都必须可读。
#[test]
fn test_handoff_reminder_records_pending_tasks_and_scope_owner() {
    let handoff = PendingHandoff {
        tasks: vec![task("mcp-aaa", Some("root-a")), task("bg-bbb", None)],
        waited: Duration::from_secs(120),
    };
    let reminder = handoff_reminder(&handoff).unwrap();
    let body = &reminder.as_reminder().body;
    assert!(body.starts_with("pending: 2"), "{body}");
    assert!(body.contains("mcp-aaa"), "{body}");
    assert!(body.contains("bg-bbb"), "{body}");
    assert!(body.contains("root-a"), "{body}");
    assert!(
        body.contains("reconciled by the scope owner"),
        "交接必须说明结果由 scope owner 对账补投: {body}"
    );
    let metadata = &reminder.as_reminder().metadata;
    assert_eq!(metadata["pending"], 2);
    assert_eq!(metadata["waited_ms"], 120_000);
    assert_eq!(metadata["tasks"][0]["task_id"], "mcp-aaa");
}

// 同一未结算集合重复交接必须收敛到同一投递 ID（幂等，不产生第二条记录）。
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
    let changed = vec![task("mcp-aaa", Some("root-b"))];
    let id = handoff_delivery_id(&first);
    assert_ne!(id, handoff_delivery_id(&changed));
    // 顺序无关（调用方按 task_id 排序后传入，这里锁同一集合的稳定性）
    let mut sorted = reordered;
    sorted.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    assert_eq!(id, handoff_delivery_id(&sorted));
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
    assert_eq!(pending[0].task_id, task_id);
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
