//! `runtime/tasks.rs` 的行为测试：发布幂等、状态机、单次终态提交（CAS）、
//! 取消句柄与任务事件（design §5.1 / §5.3）。

use super::*;
use crate::protocol::Usage;

fn make_params(run_id: &str, agent_id: u64) -> AgentRunParams {
    AgentRunParams {
        run_id: run_id.to_string(),
        agent_id,
        prompt: "do work".into(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    }
}

fn ok_result() -> AgentRunResult {
    AgentRunResult::Ok {
        output: "done".into(),
        usage: Usage { output_tokens: 3 },
        model: None,
        tool_count: None,
        token_count: None,
        phase: None,
        duration_ms: None,
    }
}

fn make_table() -> (
    WorkflowTaskTable,
    broadcast::Receiver<WorkflowResourceUpdate>,
) {
    let (tx, rx) = broadcast::channel(32);
    (WorkflowTaskTable::new(tx, Arc::new(CloseGate::new())), rx)
}

#[test]
fn publish_is_idempotent_and_keeps_first_params() {
    let (table, _rx) = make_table();
    match table.publish("t-1".into(), make_params("run-1", 1)) {
        PublishOutcome::Published(record) => {
            assert_eq!(record.state, WorkflowTaskState::Published);
            assert!(record.published_at_ms > 0, "发布时刻为 wall clock 毫秒");
        }
        other => panic!("首个发布必须提交：{other:?}"),
    }

    let mut changed = make_params("run-1", 1);
    changed.prompt = "changed".into();
    match table.publish("t-1".into(), changed) {
        PublishOutcome::AlreadyKnown(record) => {
            assert_eq!(record.params.prompt, "do work", "重复发布不得覆盖首个参数");
        }
        other => panic!("重复发布必须幂等：{other:?}"),
    }
    assert_eq!(table.snapshot().len(), 1, "重复发布不产生第二条记录");
}

#[test]
fn same_task_id_in_different_runs_is_distinct() {
    let (table, _rx) = make_table();
    assert!(matches!(
        table.publish("t-1".into(), make_params("run-1", 1)),
        PublishOutcome::Published(_)
    ));
    assert!(
        matches!(
            table.publish("t-1".into(), make_params("run-2", 1)),
            PublishOutcome::Published(_)
        ),
        "taskId 作用域是单次 run：跨 run 同名是不同任务"
    );
    assert_eq!(table.snapshot().len(), 2);
}

#[test]
fn lifecycle_transitions_and_single_terminal_settle() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 7));

    let token = CancellationToken::new();
    let executing = match table.mark_executing("run-1", "t-1", token) {
        ClaimOutcome::Claimed(record) => record,
        other => panic!("已发布任务首次 claim 必须成功：{other:?}"),
    };
    assert_eq!(executing.state, WorkflowTaskState::Executing);

    match table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Completed,
        Some(ok_result()),
        None,
    ) {
        SettleOutcome::Settled(record) => {
            assert_eq!(record.state, WorkflowTaskState::Completed);
            assert!(record.result.is_some());
        }
        other => panic!("首次终态提交必须成功：{other:?}"),
    }

    match table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Failed,
        None,
        Some("late".into()),
    ) {
        SettleOutcome::AlreadyTerminal(record) => {
            assert_eq!(record.state, WorkflowTaskState::Completed, "首次终态保留");
            assert!(record.result.is_some(), "重复提交不得覆盖首次结果");
        }
        other => panic!("重复终态提交必须幂等：{other:?}"),
    }
}

#[test]
fn settle_rejects_non_terminal_state_without_mutating_record() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    let token = CancellationToken::new();
    assert!(matches!(
        table.mark_executing("run-1", "t-1", token.clone()),
        ClaimOutcome::Claimed(_)
    ));

    match table.settle(
        "run-1",
        "t-1",
        SettleAdmission::AnyActive,
        WorkflowTaskState::Executing,
        None,
        None,
    ) {
        SettleOutcome::InvalidState(record) => {
            assert_eq!(
                record.state,
                WorkflowTaskState::Executing,
                "非终态 state 不得写入记录"
            );
        }
        other => panic!("非终态 state 必须被显式拒绝：{other:?}"),
    }
    assert_eq!(
        table.get("run-1", "t-1").unwrap().state,
        WorkflowTaskState::Executing,
        "拒绝提交后记录保持原状"
    );
    assert!(!token.is_cancelled(), "拒绝提交不触发取消句柄");
}

#[test]
fn settle_executing_only_rejects_unclaimed_task() {
    // 回填授权面：published（消费者尚未 claim）不得被直接结算为终态。
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));

    match table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Completed,
        Some(ok_result()),
        None,
    ) {
        SettleOutcome::NotAdmitted(record) => {
            assert_eq!(record.state, WorkflowTaskState::Published);
        }
        other => panic!("未 claim 的任务不得被回填结算：{other:?}"),
    }
    assert_eq!(
        table.get("run-1", "t-1").unwrap().state,
        WorkflowTaskState::Published,
        "拒绝提交后记录保持原状"
    );
}

#[test]
fn mark_executing_is_exclusive_claim() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    assert!(matches!(
        table.mark_executing("run-1", "t-1", first.clone()),
        ClaimOutcome::Claimed(_)
    ));

    match table.mark_executing("run-1", "t-1", second.clone()) {
        ClaimOutcome::AlreadyExecuting(record) => {
            assert_eq!(record.state, WorkflowTaskState::Executing);
        }
        other => panic!("第二次 claim 必须被识别为已被 claim：{other:?}"),
    }

    // 第二个 claim 的句柄不得生效：取消只触发唯一 claim 的句柄。
    assert!(matches!(
        table.cancel("run-1", "t-1", Some("killed".into())),
        SettleOutcome::Settled(_)
    ));
    assert!(first.is_cancelled(), "唯一 claim 的句柄必须被触发");
    assert!(!second.is_cancelled(), "落败 claim 的句柄不得被触发");
}

#[test]
fn settle_unknown_task_is_not_found() {
    let (table, _rx) = make_table();
    assert!(matches!(
        table.settle(
            "run-1",
            "nope",
            SettleAdmission::AnyActive,
            WorkflowTaskState::Cancelled,
            None,
            None
        ),
        SettleOutcome::NotFound
    ));
}

#[test]
fn cancel_triggers_registered_token_exactly_once() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    let token = CancellationToken::new();
    table.mark_executing("run-1", "t-1", token.clone());
    assert!(!token.is_cancelled());

    match table.cancel("run-1", "t-1", Some("killed".into())) {
        SettleOutcome::Settled(record) => {
            assert_eq!(record.state, WorkflowTaskState::Cancelled);
            assert_eq!(record.reason.as_deref(), Some("killed"));
        }
        other => panic!("取消必须提交终态：{other:?}"),
    }
    assert!(token.is_cancelled(), "cancel 必须触发已登记的取消句柄");

    // 第二次取消幂等：不覆盖终态、不重复触发（token 已被终态提交清空）。
    assert!(matches!(
        table.cancel("run-1", "t-1", None),
        SettleOutcome::AlreadyTerminal(_)
    ));
}

#[test]
fn settle_wins_over_later_cancel_first_terminal_state_kept() {
    // 「结果与 kill 同时到达」：以先进入终态者为准。
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    let token = CancellationToken::new();
    table.mark_executing("run-1", "t-1", token.clone());

    table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Completed,
        Some(ok_result()),
        None,
    );
    assert!(matches!(
        table.cancel("run-1", "t-1", Some("too late".into())),
        SettleOutcome::AlreadyTerminal(_)
    ));
    assert_eq!(
        table.get("run-1", "t-1").unwrap().state,
        WorkflowTaskState::Completed,
        "迟到 kill 不得覆盖已完成"
    );
    assert!(!token.is_cancelled(), "终态已提交：不再触发取消句柄");
}

#[test]
fn cancel_run_only_touches_unsettled_tasks_of_that_run() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    table.publish("t-2".into(), make_params("run-1", 2));
    table.publish("t-3".into(), make_params("run-2", 1));
    let token = CancellationToken::new();
    assert!(matches!(
        table.mark_executing("run-1", "t-1", token),
        ClaimOutcome::Claimed(_)
    ));
    table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Completed,
        Some(ok_result()),
        None,
    );

    let cancelled = table.cancel_run("run-1", Some("run killed".into()));
    assert_eq!(cancelled, vec!["t-2".to_string()], "只取消未终态任务");
    assert_eq!(
        table.get("run-1", "t-1").unwrap().state,
        WorkflowTaskState::Completed
    );
    assert_eq!(
        table.get("run-2", "t-3").unwrap().state,
        WorkflowTaskState::Published,
        "其他 run 的任务不受影响"
    );
}

#[test]
fn mark_executing_after_terminal_keeps_terminal_state() {
    // kill 先于消费：任务已 cancelled，mark_executing 不得把它改回 executing。
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    table.cancel("run-1", "t-1", Some("kill first".into()));

    let claim = table.mark_executing("run-1", "t-1", CancellationToken::new());
    match claim {
        ClaimOutcome::AlreadyTerminal(record) => {
            assert_eq!(
                record.state,
                WorkflowTaskState::Cancelled,
                "终态不被 mark_executing 覆盖"
            );
        }
        other => panic!("终态任务的 claim 必须被识别：{other:?}"),
    }
}

#[test]
fn active_excludes_terminal_tasks() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    table.publish("t-2".into(), make_params("run-1", 2));
    assert!(matches!(
        table.mark_executing("run-1", "t-1", CancellationToken::new()),
        ClaimOutcome::Claimed(_)
    ));
    assert!(matches!(
        table.mark_executing("run-1", "t-2", CancellationToken::new()),
        ClaimOutcome::Claimed(_)
    ));
    table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Failed,
        None,
        None,
    );

    let active: Vec<_> = table.active().into_iter().map(|r| r.task_id).collect();
    assert_eq!(active, vec!["t-2".to_string()]);
}

#[test]
fn events_carry_sequence_kind_and_stable_event_id() {
    let (table, mut rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 5));
    let token = CancellationToken::new();
    assert!(matches!(
        table.mark_executing("run-1", "t-1", token),
        ClaimOutcome::Claimed(_)
    ));
    table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Completed,
        Some(ok_result()),
        None,
    );

    let page = table.events().page(None, 10);
    let kinds: Vec<_> = page.entries.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            WorkflowTaskEventKind::Published,
            WorkflowTaskEventKind::Executing,
            WorkflowTaskEventKind::Completed
        ]
    );
    let sequences: Vec<_> = page.entries.iter().map(|event| event.sequence).collect();
    assert_eq!(sequences, vec![1, 2, 3], "session 级任务流单调 sequence");
    assert_eq!(page.entries[0].event_id, "run-1:t-1:task.published");
    assert_eq!(
        page.entries[2].task.state,
        WorkflowTaskState::Completed,
        "终态事件携带结果快照"
    );
    assert_eq!(page.entries[0].agent_id, 5);

    // eventId 内的 kind 字面量与 serde 序列化一致（不用 Debug 输出）。
    for event in &page.entries {
        assert_eq!(
            event.event_id,
            format!(
                "{}:{}:task.{}",
                event.run_id,
                event.task_id,
                event.kind.as_str()
            )
        );
        assert_eq!(
            serde_json::to_value(event.kind).unwrap(),
            serde_json::Value::String(event.kind.as_str().to_string()),
            "eventId 与 wire 序列化共用同一份 kind 字面量"
        );
    }

    // 订阅唤醒：任务表变化发 Tasks 与 RunTasks（正文经 read 补读）。
    let mut updates = Vec::new();
    while let Ok(update) = rx.try_recv() {
        updates.push(update);
    }
    assert!(updates.contains(&WorkflowResourceUpdate::Tasks));
    assert!(updates.contains(&WorkflowResourceUpdate::RunTasks {
        run_id: "run-1".into()
    }));
}

#[test]
fn for_agent_filters_within_run() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    table.publish("t-2".into(), make_params("run-1", 2));
    table.publish("t-3".into(), make_params("run-2", 1));

    let records = table.for_agent("run-1", 2);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id, "t-2");
    assert_eq!(table.for_run("run-1").len(), 2);
    assert!(table.for_agent("run-1", 9).is_empty());
}

#[test]
fn terminal_state_helpers() {
    assert!(!WorkflowTaskState::Published.is_terminal());
    assert!(!WorkflowTaskState::Executing.is_terminal());
    assert!(WorkflowTaskState::Completed.is_terminal());
    assert!(WorkflowTaskState::Failed.is_terminal());
    assert!(WorkflowTaskState::Cancelled.is_terminal());
}

#[test]
fn cancel_run_fences_tasks_published_after_the_sweep() {
    // kill 是栅栏不是快照：sweep 之后（Node 迟到 / 并发）发布的任务直接落 cancelled。
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    let cancelled = table.cancel_run("run-1", Some("run killed".into()));
    assert_eq!(cancelled, vec!["t-1".to_string()]);

    match table.publish("t-late".into(), make_params("run-1", 2)) {
        PublishOutcome::Published(record) => {
            assert_eq!(
                record.state,
                WorkflowTaskState::Cancelled,
                "kill 栅栏命中：迟到发布不进入可执行状态"
            );
            assert_eq!(record.reason.as_deref(), Some("run killed"));
        }
        other => panic!("迟到发布仍需提交（事实源完整）：{other:?}"),
    }
    assert!(
        table.active().is_empty(),
        "栅栏命中后不存在可被消费者 claim 的任务"
    );

    let kinds: Vec<_> = table
        .events()
        .page(None, 10)
        .entries
        .iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            WorkflowTaskEventKind::Published,
            WorkflowTaskEventKind::Cancelled,
            WorkflowTaskEventKind::Published,
            WorkflowTaskEventKind::Cancelled,
        ],
        "迟到发布的事件顺序仍是 published → cancelled"
    );

    // 其他 run 不受 run 级栅栏影响。
    match table.publish("t-other".into(), make_params("run-2", 1)) {
        PublishOutcome::Published(record) => {
            assert_eq!(record.state, WorkflowTaskState::Published);
        }
        other => panic!("其他 run 不应被栅栏命中：{other:?}"),
    }
}

#[test]
fn cancel_agent_fences_tasks_of_that_agent_only() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 7));
    table.publish("t-2".into(), make_params("run-1", 8));
    let cancelled = table.cancel_agent("run-1", 7, Some("agent 7 killed".into()));
    assert_eq!(cancelled, vec!["t-1".to_string()]);

    match table.publish("t-late".into(), make_params("run-1", 7)) {
        PublishOutcome::Published(record) => {
            assert_eq!(record.state, WorkflowTaskState::Cancelled);
        }
        other => panic!("同 agent 的迟到发布必须被栅栏命中：{other:?}"),
    }
    match table.publish("t-3".into(), make_params("run-1", 8)) {
        PublishOutcome::Published(record) => {
            assert_eq!(
                record.state,
                WorkflowTaskState::Published,
                "其他 agent 不受影响"
            );
        }
        other => panic!("其他 agent 不应被栅栏命中：{other:?}"),
    }
}

#[test]
fn terminal_tasks_are_evicted_after_retention_window() {
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    table.settle(
        "run-1",
        "t-1",
        SettleAdmission::AnyActive,
        WorkflowTaskState::Cancelled,
        None,
        Some("killed".into()),
    );
    table.publish("t-2".into(), make_params("run-1", 2));

    // 保留窗口内：终态任务仍在快照表（资源可读）。
    let now = now_ms();
    table.trim_at(now);
    assert_eq!(table.snapshot().len(), 2);

    // 窗口之后：终态任务被淘汰，活跃任务保留。
    table.trim_at(now + TASK_RETENTION_MS);
    let remaining: Vec<_> = table.snapshot().into_iter().map(|r| r.task_id).collect();
    assert_eq!(remaining, vec!["t-2".to_string()], "只淘汰终态任务");
    assert!(table.get("run-1", "t-1").is_none(), "过期终态任务不可再读");
}

/// 测试 seam：以指定时刻执行保留窗口淘汰（生产路径用 `now_ms()`）。
impl WorkflowTaskTable {
    fn trim_at(&self, now: i64) {
        let mut inner = self.inner.lock();
        Self::trim_locked(&mut inner, now);
    }
}

#[test]
fn snapshot_table_has_a_hard_upper_bound() {
    let (table, _rx) = make_table();
    for index in 0..(MAX_TASK_RECORDS + 8) {
        let task_id = format!("t-{index}");
        table.publish(task_id.clone(), make_params("run-1", index as u64));
        table.settle(
            "run-1",
            &task_id,
            SettleAdmission::AnyActive,
            WorkflowTaskState::Completed,
            None,
            None,
        );
    }
    let snapshot = table.snapshot();
    assert!(
        snapshot.len() <= MAX_TASK_RECORDS,
        "快照表必须有硬上界：{} > {MAX_TASK_RECORDS}",
        snapshot.len()
    );
    assert_eq!(
        snapshot.last().map(|record| record.task_id.clone()),
        Some(format!("t-{}", MAX_TASK_RECORDS + 7)),
        "淘汰从最旧的终态任务开始，最新任务保留"
    );
    assert!(
        table.get("run-1", "t-0").is_none(),
        "最旧的终态任务已被淘汰"
    );
}

#[test]
fn conflicting_late_settle_keeps_first_result() {
    // 冲突回填：保留首次结果并返回既有终态（告警经 tracing，不在断言面）。
    let (table, _rx) = make_table();
    table.publish("t-1".into(), make_params("run-1", 1));
    assert!(matches!(
        table.mark_executing("run-1", "t-1", CancellationToken::new()),
        ClaimOutcome::Claimed(_)
    ));
    assert!(matches!(
        table.settle(
            "run-1",
            "t-1",
            SettleAdmission::ExecutingOnly,
            WorkflowTaskState::Completed,
            Some(ok_result()),
            None
        ),
        SettleOutcome::Settled(_)
    ));
    match table.settle(
        "run-1",
        "t-1",
        SettleAdmission::ExecutingOnly,
        WorkflowTaskState::Failed,
        Some(AgentRunResult::Dead {
            reason: Some("conflict".into()),
            detail: None,
        }),
        Some("conflict".into()),
    ) {
        SettleOutcome::AlreadyTerminal(record) => {
            assert_eq!(record.state, WorkflowTaskState::Completed, "首次终态保留");
            assert!(
                matches!(record.result, Some(AgentRunResult::Ok { .. })),
                "首次结果保留"
            );
        }
        other => panic!("冲突回填必须幂等返回既有终态：{other:?}"),
    }
}
