//! AsyncResultMailbox 单元测试：接纳幂等、规范提交、请求覆盖、阻塞与挂起。
//!
//! 这些用例只验证处理责任状态机；跨阶段行为（Reason 快照 → Act 提交、Full Compact
//! 保护、激活）由 `peri-agent/src/agent/stages/` 下的 `async_result_*` 集成测试覆盖。

use super::*;
use peri_acp_types::session::MessageSource;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalSource, SystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};

fn reminder(task_id: &str, body: &str) -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalSource("shell".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.into(),
            summary: None,
            metadata: serde_json::json!({
                "task_id": task_id,
                "remote_task_id": format!("remote-{task_id}"),
            }),
        })
        .unwrap()
}

fn mailbox() -> AsyncResultMailbox {
    AsyncResultMailbox::new("session-1", "thread-1")
}

#[test]
fn async_result_accept_is_idempotent_and_rejects_conflicting_content() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let first = reminder("shell-1", "done");

    assert_eq!(
        mailbox.accept(id, &first, MessageSource::ShellComplete),
        Ok(AcceptOutcome::Accepted)
    );
    assert_eq!(
        mailbox.accept(id, &first.clone(), MessageSource::ShellComplete),
        Ok(AcceptOutcome::Duplicate)
    );

    let conflicting = reminder("shell-1", "different body");
    assert!(
        mailbox
            .accept(id, &conflicting, MessageSource::ShellComplete)
            .is_err(),
        "same delivery id with different content must conflict"
    );

    // 接纳只建立 Queued 责任，不进入保护集合。
    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Queued));
    assert!(mailbox.protected_snapshot().is_empty());
    assert!(!mailbox.has_ready());
}

#[test]
fn async_result_canonical_ready_requires_commit_and_protects() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    let before = mailbox.protected_snapshot().version;

    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, Some(7))
        .unwrap();

    let snapshot = mailbox.protected_snapshot();
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].delivery_id, id);
    assert_eq!(snapshot.items[0].state, AsyncResultState::Ready);
    assert_eq!(snapshot.items[0].public_task_id, "shell-1");
    assert_eq!(
        snapshot.items[0].remote_task_id.as_deref(),
        Some("remote-shell-1")
    );
    assert!(
        snapshot.version > before,
        "commit must advance the protected version"
    );
    assert!(mailbox.has_ready());
}

#[test]
fn async_result_protected_snapshot_keeps_admission_order() {
    let mailbox = mailbox();
    let ids = [MessageId::new(), MessageId::new(), MessageId::new()];
    for (index, id) in ids.iter().enumerate() {
        mailbox
            .accept(
                *id,
                &reminder(&format!("shell-{index}"), "done"),
                MessageSource::ShellComplete,
            )
            .unwrap();
    }
    // 乱序提交：保护投影仍按接纳顺序，且每个 ID 只出现一次。
    for index in [2usize, 0, 1] {
        mailbox
            .mark_canonical_ready(
                ids[index],
                &reminder(&format!("shell-{index}"), "done"),
                MessageSource::ShellComplete,
                None,
            )
            .unwrap();
    }

    let snapshot = mailbox.protected_snapshot();
    let order: Vec<_> = snapshot.items.iter().map(|item| item.delivery_id).collect();
    assert_eq!(order, ids);
    assert_eq!(snapshot.ids(), ids);
}

#[test]
fn async_result_capture_request_only_includes_projected_ids() {
    let mailbox = mailbox();
    let projected = MessageId::new();
    let pending = MessageId::new();
    for (id, task) in [(projected, "shell-1"), (pending, "shell-2")] {
        let result = reminder(task, "done");
        mailbox
            .accept(id, &result, MessageSource::ShellComplete)
            .unwrap();
        mailbox
            .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
            .unwrap();
    }

    assert!(
        mailbox
            .capture_request("attempt-1", "request-1", &[])
            .is_none(),
        "a request that projected nothing has no receipt"
    );

    let receipt = mailbox
        .capture_request("attempt-1", "request-1", &[projected])
        .expect("projected result must be captured");
    assert_eq!(receipt.included_ids(), &[projected]);
    assert_eq!(
        mailbox.state_of(&projected),
        Some(AsyncResultState::InFlight)
    );
    assert_eq!(mailbox.state_of(&pending), Some(AsyncResultState::Ready));
    assert!(
        mailbox.has_ready(),
        "unprojected ready result still requires processing"
    );
}

#[test]
fn async_result_cover_requires_a_valid_attempt_bound_commit() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-1", "request-1", &[id])
        .unwrap();

    assert_eq!(mailbox.cover_after_commit(&receipt, Some(id)), Ok(1));
    assert_eq!(
        mailbox.state_of(&id),
        None,
        "covered entries leave the state map"
    );
    assert!(!mailbox.retains_body(&id));
    assert!(mailbox.protected_snapshot().is_empty());
    // 幂等：重复确认不再计数。
    assert_eq!(mailbox.cover_after_commit(&receipt, Some(id)), Ok(0));
}

#[test]
fn async_result_failed_stage_blocks_without_self_activation() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-1", "request-1", &[id])
        .unwrap();

    mailbox.block_attempt(&receipt, "act", "stream interrupted before commit");

    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert!(!mailbox.has_ready(), "Blocked must never self-activate");
    assert!(mailbox.retains_body(&id));
    let snapshot = mailbox.protected_snapshot();
    assert_eq!(snapshot.items.len(), 1);
    let diagnostic = snapshot.items[0].diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.stage, "act");
    assert!(!diagnostic.auto_recovery_allowed);

    // 失败后的迟到提交不得覆盖责任。
    assert!(mailbox.cover_after_commit(&receipt, Some(id)).is_err());

    // 只有显式继续才恢复 Ready，并保留失败诊断。
    mailbox.resume_scope();
    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Ready));
    assert_eq!(
        mailbox.protected_snapshot().items[0]
            .diagnostic
            .as_ref()
            .unwrap()
            .stage,
        "act"
    );
}

#[test]
fn async_result_suspend_preserves_commit_state_for_resume() {
    let mailbox = mailbox();
    let committed = MessageId::new();
    let queued = MessageId::new();
    let committed_result = reminder("shell-1", "done");
    let queued_result = reminder("shell-2", "done");
    mailbox
        .accept(committed, &committed_result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(
            committed,
            &committed_result,
            MessageSource::ShellComplete,
            None,
        )
        .unwrap();
    mailbox
        .accept(queued, &queued_result, MessageSource::ShellComplete)
        .unwrap();

    mailbox.suspend_scope("cancelled");
    assert_eq!(
        mailbox.state_of(&committed),
        Some(AsyncResultState::Suspended)
    );
    assert_eq!(mailbox.state_of(&queued), Some(AsyncResultState::Suspended));
    assert!(!mailbox.has_ready(), "Stop must not auto-wake the model");
    // 挂起不是覆盖。
    assert!(!mailbox.protected_snapshot().is_empty());

    mailbox.resume_scope();
    assert_eq!(mailbox.state_of(&committed), Some(AsyncResultState::Ready));
    assert_eq!(
        mailbox.state_of(&queued),
        Some(AsyncResultState::Queued),
        "never-committed results stay owned by the queue after resume"
    );
}

#[test]
fn async_result_scope_suspension_covers_late_canonical_commit() {
    let mailbox = mailbox();
    mailbox.suspend_scope("cancelled");

    // Stop 之后才完成并规范提交的结果不能变成自动续跑候选。
    let id = MessageId::new();
    let result = reminder("shell-late", "done after stop");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();

    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Suspended));
    assert!(!mailbox.has_ready());
    assert_eq!(mailbox.protected_snapshot().ids(), vec![id]);

    mailbox.resume_scope();
    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Ready));
    assert!(mailbox.has_ready());
}

#[test]
fn async_result_suspension_invalidates_in_flight_receipt() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-1", "request-1", &[id])
        .unwrap();

    mailbox.suspend_scope("cancelled");
    assert!(
        mailbox.cover_after_commit(&receipt, Some(id)).is_err(),
        "a response committed after Stop cannot claim coverage"
    );
    assert_eq!(
        mailbox.state_of(&id),
        Some(AsyncResultState::Suspended),
        "failed coverage must not change the suspended state"
    );
}

#[test]
fn async_result_covered_rejects_conflicting_redelivery() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-1", "request-1", &[id])
        .unwrap();
    assert_eq!(mailbox.cover_after_commit(&receipt, None), Ok(1));

    // 同 ID 同内容的重放（恢复场景）幂等；不同内容仍冲突。
    assert_eq!(
        mailbox.accept(id, &result.clone(), MessageSource::ShellComplete),
        Ok(AcceptOutcome::Duplicate)
    );
    assert!(mailbox
        .accept(
            id,
            &reminder("shell-1", "tampered"),
            MessageSource::ShellComplete
        )
        .is_err());
    assert!(mailbox
        .mark_canonical_ready(
            id,
            &reminder("shell-1", "tampered"),
            MessageSource::ShellComplete,
            None,
        )
        .is_err());
}

#[test]
fn async_result_watch_signal_is_retained_across_missed_changes() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();

    // 订阅发生在变化之后：保留式版本仍可观察，配合 has_ready 不会丢唤醒。
    let receiver = mailbox.subscribe_state();
    assert!(*receiver.borrow() > 0);
    assert!(mailbox.has_ready());
}

#[test]
fn async_result_scope_isolates_sessions_and_threads() {
    let root = AsyncResultMailbox::new("session-1", "thread-1");
    let child = AsyncResultMailbox::new("session-1", "thread-2");
    assert_eq!(root.scope(), &ResultScope::new("session-1", "thread-1"));

    let id = MessageId::new();
    root.accept(
        id,
        &reminder("shell-1", "done"),
        MessageSource::ShellComplete,
    )
    .unwrap();

    assert_eq!(root.pending_count(), 1);
    assert_eq!(child.pending_count(), 0);
    assert!(child.protected_snapshot().is_empty());
}

#[test]
fn async_result_diagnostics_are_bounded() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "done");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();

    mailbox.block_uncovered("compact", &"x".repeat(1024));
    let snapshot = mailbox.protected_snapshot();
    let summary = &snapshot.items[0].diagnostic.as_ref().unwrap().summary;
    assert_eq!(summary.chars().count(), DIAGNOSTIC_MAX_CHARS + 1);
    assert!(summary.ends_with('…'));
}

/// [T08] 请求只覆盖它实际投影的 A；推理期间到达的 B 仍是 Ready，
/// A 的响应提交后只确认 A。
#[test]
fn async_result_commit_covers_only_the_requested_snapshot() {
    let mailbox = mailbox();
    let a = MessageId::new();
    let b = MessageId::new();
    let result_a = reminder("shell-a", "A done");
    let result_b = reminder("shell-b", "B done");
    mailbox
        .accept(a, &result_a, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(a, &result_a, MessageSource::ShellComplete, None)
        .unwrap();

    let receipt = mailbox
        .capture_request("attempt-a", "request-a", &[a])
        .expect("A 必须进入 receipt");

    // 推理期间 B 完成并规范提交。
    mailbox
        .accept(b, &result_b, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(b, &result_b, MessageSource::ShellComplete, None)
        .unwrap();

    assert_eq!(mailbox.cover_after_commit(&receipt, Some(a)), Ok(1));
    assert_eq!(mailbox.state_of(&a), None, "A 已被有效响应覆盖");
    assert_eq!(
        mailbox.state_of(&b),
        Some(AsyncResultState::Ready),
        "B 不在本次请求中，不能被 A 的响应顺带确认"
    );
    assert_eq!(mailbox.protected_snapshot().ids(), vec![b]);
    assert!(mailbox.has_ready());
}

/// [T06/T13] Compact 阶段失败（含受保护事实超预算）必须阻塞全部未覆盖结果，
/// 记录可诊断阶段，且不伪造 Covered；Blocked 不能自我激活。
#[test]
fn async_result_compact_failure_blocks_with_diagnostic() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-1", "terminal fact");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();

    mailbox.block_uncovered(
        "compact",
        "protected result facts exceed the safe context budget",
    );

    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert!(!mailbox.has_ready());
    let snapshot = mailbox.protected_snapshot();
    let diagnostic = snapshot.items[0].diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.stage, "compact");
    assert!(!diagnostic.auto_recovery_allowed);
    assert!(
        diagnostic.summary.len() <= 240,
        "诊断必须有界，不随重试堆叠全文"
    );
    assert!(mailbox.retains_body(&id), "阻塞不得吞掉通知或丢弃任务身份");
}

/// [T01/T08] 关联 trace：同一个 delivery_id 依次出现接纳、规范提交、请求 receipt
/// 与覆盖四个事件。使用 mailbox 的确定性转移记录，避免并行测试的 tracing
/// callsite 兴趣缓存导致断言抖动；生产仍输出同名 tracing 事件。
#[test]
fn async_result_trace_links_delivery_to_coverage() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-trace", "trace coverage");
    mailbox
        .accept(id, &result, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-trace", "request-trace", &[id])
        .unwrap();
    mailbox.cover_after_commit(&receipt, Some(id)).unwrap();

    let events = mailbox.trace_events();
    let text = events.join("\n");
    for event in [
        "result_accepted",
        "result_canonical_ready",
        "primary_result_receipt",
        "result_covered",
    ] {
        assert!(text.contains(event), "关联 trace 缺少 {event}：\n{text}");
    }
    let id_text = id.as_uuid().to_string();
    assert!(
        events.iter().filter(|line| line.contains(&id_text)).count() >= 3,
        "trace 行必须能按 delivery_id 关联：\n{text}"
    );
    println!("{text}");
}

/// [T11] 跨 prompt 重建：canonical 历史里已有提醒、热态 mailbox 记录缺失时，
/// Receive 补建同一 ID 的 Ready 责任，不重复投影、不跨 scope 串扰。
#[test]
fn async_result_canonical_rebuild_keeps_identity_and_scope() {
    let mailbox = mailbox();
    let id = MessageId::new();
    let result = reminder("shell-rebuild", "terminal fact");
    // 不先 accept：模拟新 prompt 重建 transcript 后，历史已有提醒但 mailbox 暂无记录。
    mailbox
        .mark_canonical_ready(id, &result, MessageSource::ShellComplete, None)
        .unwrap();
    assert_eq!(mailbox.state_of(&id), Some(AsyncResultState::Ready));
    assert_eq!(mailbox.protected_snapshot().ids(), vec![id]);

    // 迟到的重复投递沿用原 ID 与内容：只确认一次，不再新增保护条目。
    assert_eq!(
        mailbox.accept(id, &result, MessageSource::ShellComplete),
        Ok(AcceptOutcome::Duplicate)
    );
    assert_eq!(mailbox.protected_snapshot().items.len(), 1);

    // 另一个会话/线程的 mailbox 看不到、也不能确认本 scope 的责任。
    let other = AsyncResultMailbox::new("session-2", "thread-2");
    assert!(!other.has_ready());
    assert!(other.protected_snapshot().is_empty());
    assert!(other.covered_is_empty());
}
