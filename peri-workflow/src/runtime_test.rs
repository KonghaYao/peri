//! `runtime.rs` 的准入与生命周期测试：关闭位、resume 校验、kill 命中语义、
//! 消费者绑定令牌与事件水位（P2 对抗审查修订项）。

use std::sync::Arc;

use peri_acp_types::workflow::{AgentExecutor, AgentRunParams, AgentRunResult, Usage};
use tokio::sync::broadcast;

use crate::journal::{RunState, WorkflowJournalStore};
use crate::runner::WorkflowInput;
use crate::runtime::{WorkflowResourceUpdate, WorkflowRuntime};

struct NullExecutor;

#[async_trait::async_trait]
impl AgentExecutor for NullExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        AgentRunResult::Ok {
            output: "unused".into(),
            usage: Usage { output_tokens: 0 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

fn make_runtime(cwd: &str) -> WorkflowRuntime {
    let (notification_tx, _) = broadcast::channel::<crate::registry::WorkflowTaskResult>(8);
    WorkflowRuntime::new(Arc::new(NullExecutor), cwd, notification_tx, None)
}

fn minimal_input() -> WorkflowInput {
    WorkflowInput {
        script: "export const meta = { name: 't', description: 'd' }\nreturn 1".into(),
        args: None,
        max_concurrency: 1,
        budget_total: None,
        limits: Default::default(),
        workflow_name: "t".into(),
        resume_from: None,
        write_intent: None,
        git_observation: Default::default(),
    }
}

#[tokio::test]
async fn closed_runtime_rejects_new_runs_on_both_entry_points() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );

    runtime.mark_closed();
    assert!(runtime.is_closed());

    let error = runtime
        .start_prepared_run(minimal_input())
        .await
        .expect_err("关闭后不得再产生新 run");
    assert!(
        error.contains("closed"),
        "关闭拒绝必须是明确的关闭语义：{error}"
    );

    let error = runtime
        .start_run(serde_json::json!({ "script": "return 1" }))
        .await
        .expect_err("关闭后工具入口同样拒绝");
    assert!(
        error.contains("closed"),
        "两个入口共用同一关闭检查：{error}"
    );
}

#[tokio::test]
async fn resume_rejects_unsafe_run_id_and_mismatched_state() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );

    // 路径分隔符 / 非 UUID：同一权威校验（is_safe_run_id）。
    for candidate in ["../../etc", "not-a-uuid", "a/b"] {
        let error = runtime
            .resume(candidate)
            .await
            .expect_err("非法 run_id 必须被拒绝");
        assert!(
            error.contains("Invalid resumeFromRunId"),
            "{candidate} 必须走同一权威校验：{error}"
        );
    }

    // 合法 UUID 但 state.json 属于另一个 run：拒绝（目录被替换 / 损坏）。
    let requested = uuid::Uuid::now_v7().to_string();
    let other = uuid::Uuid::now_v7().to_string();
    let journal = WorkflowJournalStore::new(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    journal
        .init_run(&requested, "return 1")
        .expect("the init run fixture result is required for this assertion");
    journal
        .write_state(
            &requested,
            &RunState {
                run_id: other.clone(),
                workflow_name: "t".into(),
                status: "completed".into(),
                execution_status: Default::default(),
                acceptance_status: Default::default(),
                post_processing_status: Default::default(),
                delivery_status: Default::default(),
                delivery_reason: None,
                postcondition_observation: None,
                git_observation_scope: None,
                outcome: Default::default(),
                write_intent: None,
                limits: Default::default(),
                limit_breaches: Vec::new(),
                token_usage: None,
                budget_total: None,
                args: None,
                max_concurrency: 1,
                attempts: Vec::new(),
                return_value: None,
                script: "return 1".into(),
                started_at: "2026-10-11T00:00:00Z".into(),
                finished_at: None,
                error: None,
            },
        )
        .expect("the write state fixture result is required for this assertion");
    let error = runtime
        .resume(&requested)
        .await
        .expect_err("state 归属不一致必须被拒绝");
    assert!(
        error.contains("mismatch"),
        "错误必须说明归属不一致：{error}"
    );
}

#[test]
fn consumer_token_is_bound_once_and_verified() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    assert!(!runtime.consumer_bound(), "未绑定时回填一律拒绝");
    assert!(!runtime.verify_consumer_token(""), "空令牌不构成绑定");

    let token = runtime.bind_consumer_token();
    assert!(runtime.consumer_bound());
    assert_eq!(runtime.bind_consumer_token(), token, "重复绑定返回同一令牌");
    assert!(runtime.verify_consumer_token(&token));
    assert!(!runtime.verify_consumer_token("forged"));
}

#[test]
fn kill_run_only_reports_running_runs_as_killed() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = uuid::Uuid::now_v7().to_string();

    // 未登记的 run：不算命中。
    assert!(!runtime.kill_run(&run_id));

    // 已完成（仅留档）的 run：记录被移除，但不得报告「终止了运行中的 run」。
    let _ = runtime.registry().reserve(crate::registry::WorkflowRun {
        run_id: run_id.clone(),
        workflow_name: "done".into(),
        script_preview: "return 1".into(),
        status: crate::registry::WorkflowRunStatus::Completed,
        started_at: peri_time::monotonic_now(),
        child_handle: None,
        kill_tx: None,
    });
    assert!(
        !runtime.kill_run(&run_id),
        "已完成 run 不得报告为「终止了运行中的 run」"
    );
}

#[test]
fn kill_run_fences_run_tasks_before_registry_kill() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = uuid::Uuid::now_v7().to_string();
    let _ = runtime.registry().reserve(crate::registry::WorkflowRun {
        run_id: run_id.clone(),
        workflow_name: "live".into(),
        script_preview: "return 1".into(),
        status: crate::registry::WorkflowRunStatus::Running,
        started_at: peri_time::monotonic_now(),
        child_handle: None,
        kill_tx: None,
    });
    let _ = runtime.tasks().publish(
        "t-1".into(),
        crate::protocol::AgentRunParams {
            run_id: run_id.clone(),
            agent_id: 1,
            prompt: "p".into(),
            schema: None,
            model: None,
            max_tokens: None,
            agent_type: None,
            isolation: None,
            allowed_tools: None,
            label: None,
            phase: None,
        },
    );

    assert!(runtime.kill_run(&run_id), "运行中的 run 必须报告命中");
    assert!(
        runtime.tasks().active().is_empty(),
        "kill 必须先经由任务表取消全部未终态任务"
    );
    // 栅栏持续有效：kill 之后发布的同 run 任务直接落终态。
    match runtime.tasks().publish(
        "t-late".into(),
        crate::protocol::AgentRunParams {
            run_id: run_id.clone(),
            agent_id: 1,
            prompt: "p".into(),
            schema: None,
            model: None,
            max_tokens: None,
            agent_type: None,
            isolation: None,
            allowed_tools: None,
            label: None,
            phase: None,
        },
    ) {
        crate::runtime::PublishOutcome::Published(record) => assert!(
            record.state.is_terminal(),
            "kill 栅栏命中后不得留下可执行任务"
        ),
        other => panic!("迟到发布仍需提交：{other:?}"),
    }
}

#[test]
fn event_watermark_tracks_the_run_event_log() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let mut updates: broadcast::Receiver<WorkflowResourceUpdate> = runtime.subscribe();

    assert_eq!(runtime.event_watermark("run-1"), 0, "无事件时水位为 0");
    runtime
        .run_events()
        .append(crate::runtime::WorkflowRunEvent {
            sequence: 0,
            run_id: "run-1".into(),
            kind: crate::runtime::WorkflowRunEventKind::RunStarted,
            at_ms: 1,
            payload: serde_json::json!({}),
        });
    runtime
        .run_events()
        .append(crate::runtime::WorkflowRunEvent {
            sequence: 0,
            run_id: "run-2".into(),
            kind: crate::runtime::WorkflowRunEventKind::RunStarted,
            at_ms: 2,
            payload: serde_json::json!({}),
        });
    assert_eq!(runtime.event_watermark("run-1"), 1);
    assert_eq!(runtime.event_watermark("run-2"), 2);
    assert_eq!(runtime.event_watermark("run-3"), 0);
    assert!(updates.try_recv().is_err(), "水位查询不产生订阅唤醒");
}

/// F4 回归：保留窗口淘汰不得让已提交水位倒退（`eventWatermark` 不是「保留窗口内
/// 最新事件」而是「该 run 最后一条已提交事件」）。
///
/// 水位口径：sequence 由有界日志在**全局**空间从 1 起单调分配（跨 run 共享），
/// 水位 = 该 run 最后一条已提交事件的 sequence。因此本用例中 run-1 占 sequence 1，
/// run-2 的 `CAPACITY + 1` 条事件占 2..=CAPACITY+2，水位期望 `CAPACITY + 2`。
#[test]
fn event_watermark_does_not_regress_when_events_are_evicted() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );

    assert_eq!(
        runtime.run_events().append(run_event("run-1")),
        1,
        "全局 sequence 从 1 起分配"
    );
    let last_sequence = (0..=crate::runtime::RUN_EVENT_LOG_CAPACITY)
        .fold(0, |_, _| runtime.run_events().append(run_event("run-2")));
    assert_eq!(
        last_sequence,
        (crate::runtime::RUN_EVENT_LOG_CAPACITY + 2) as u64,
        "run-1 已消耗首个 sequence；run-2 的末条事件为 CAPACITY + 2"
    );

    assert_eq!(
        runtime.event_watermark("run-1"),
        1,
        "run-1 的事件被淘汰后水位仍须保持在其已提交值"
    );
    assert_eq!(
        runtime.event_watermark("run-2"),
        (crate::runtime::RUN_EVENT_LOG_CAPACITY + 2) as u64
    );
    // 防假阳性：淘汰必须真的发生（保留窗口容量即 CAPACITY），否则本用例会退化为
    // 「未触发淘汰路径仍通过」。
    let page = runtime.run_events().page_where(None, 1, |_| true);
    assert_eq!(page.head_sequence, last_sequence);
    assert!(
        page.truncated_before > 1,
        "追加 CAPACITY + 2 条后最旧事件必须被淘汰（truncated_before 前进），实际 {}",
        page.truncated_before
    );
    assert_eq!(
        runtime.event_watermark("run-unknown"),
        0,
        "无提交的 run 水位为 0"
    );
}

/// F5 回归：run 索引正文（状态 / 进度 / 清理）变化必须在提交后发索引唤醒，
/// 而不是只在启动 fast-path 补发一次。
#[test]
fn runs_index_is_notified_on_index_affecting_commits() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let mut updates = runtime.subscribe();

    runtime
        .progress_store()
        .apply_event(&crate::protocol::ProgressEvent::RunStarted {
            run_id: "run-1".into(),
            workflow_name: "t".into(),
            meta: None,
        });
    let seen = drain_updates(&mut updates);
    assert!(
        seen.contains(&WorkflowResourceUpdate::RunsIndex),
        "run 状态提交后必须发索引唤醒：{seen:?}"
    );

    // Log 不改变索引正文（只有事件日志变化）：不产生索引唤醒，避免无谓补读。
    runtime
        .progress_store()
        .apply_event(&crate::protocol::ProgressEvent::Log {
            run_id: "run-1".into(),
            message: "noise".into(),
        });
    let seen = drain_updates(&mut updates);
    assert!(
        !seen.contains(&WorkflowResourceUpdate::RunsIndex),
        "Log 不改变索引正文：{seen:?}"
    );
    assert!(
        seen.contains(&WorkflowResourceUpdate::Run {
            run_id: "run-1".into()
        }),
        "单 run 快照与事件日志仍须唤醒：{seen:?}"
    );
}

/// F6 回归：关闭位必须封住任务发布（关闭顺序第 ① 步：停止接受新 run 与新任务发布）。
#[test]
fn closed_runtime_refuses_task_publication() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    runtime.mark_closed();

    let _ = runtime
        .tasks()
        .publish("t-late".into(), task_params("run-1", 1));
    assert!(
        runtime.tasks().snapshot().is_empty(),
        "关闭后 publish 不得写入任务表"
    );
    assert_eq!(
        runtime.tasks().events().head_sequence(),
        0,
        "被拒绝的发布不得产生任务事件"
    );
}

fn run_event(run_id: &str) -> crate::runtime::WorkflowRunEvent {
    crate::runtime::WorkflowRunEvent {
        sequence: 0,
        run_id: run_id.to_string(),
        kind: crate::runtime::WorkflowRunEventKind::Log,
        at_ms: 1,
        payload: serde_json::json!({}),
    }
}

fn task_params(run_id: &str, agent_id: u64) -> AgentRunParams {
    AgentRunParams {
        run_id: run_id.to_string(),
        agent_id,
        prompt: "p".into(),
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

fn drain_updates(
    receiver: &mut broadcast::Receiver<WorkflowResourceUpdate>,
) -> Vec<WorkflowResourceUpdate> {
    let mut seen = Vec::new();
    while let Ok(update) = receiver.try_recv() {
        seen.push(update);
    }
    seen
}

#[tokio::test]
#[serial_test::serial]
async fn unsettled_child_scope_fences_real_engine_retries_and_forces_failed_completion() {
    if std::env::var_os("PERI_WORKFLOW_ARTIFACT_DIR").is_none() {
        eprintln!("Set PERI_WORKFLOW_ARTIFACT_DIR to exercise the real engine scope fence");
        return;
    }
    use crate::runtime::tasks::{ClaimOutcome, WorkflowTaskState};
    use peri_acp_types::workflow::{DeliveryStatus, WorkflowRunStatus};
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut completions) = broadcast::channel(8);
    let runtime = Arc::new(WorkflowRuntime::new(
        Arc::new(NullExecutor),
        dir.path().to_str().unwrap(),
        tx,
        None,
    ));
    let mut updates = runtime.subscribe();
    let mut input = minimal_input();
    // Even a script that swallows aborts must not acquire another assignment or
    // turn a host scope failure back into a successful completion.
    input.script = "export const meta = { name: 'scope-fence', description: 'scope-close retry regression' }; try { await agent('first'); } catch (_) {} try { await agent('must not execute'); } catch (_) {} return { status: 'completed' };".into();
    let launching = Arc::clone(&runtime);
    let launch = tokio::spawn(async move { launching.start_prepared_run(input).await });
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(task) = runtime.tasks().active().first().cloned() {
                break task;
            }
            let _ = updates.recv().await;
        }
    })
    .await
    .expect("the real engine must publish its first assignment");
    assert!(matches!(
        runtime.tasks().mark_executing(
            &first.run_id,
            &first.task_id,
            tokio_util::sync::CancellationToken::new()
        ),
        ClaimOutcome::Claimed(_)
    ));
    let detail = "Scope close incomplete: background writer has not settled";
    let _ = runtime.settle_agent_task(
        &first.run_id,
        &first.task_id,
        WorkflowTaskState::Failed,
        Some(AgentRunResult::Dead {
            scope_close_incomplete: false,
            reason: Some("scope-close-incomplete".into()),
            detail: Some(detail.into()),
            duration_ms: None,
            token_count: None,
        }),
        Some(detail.into()),
    );
    let completion = tokio::time::timeout(std::time::Duration::from_secs(10), completions.recv())
        .await
        .expect("a fenced run must terminate")
        .unwrap();
    assert_eq!(completion.status, WorkflowRunStatus::Failed);
    assert_eq!(completion.delivery_status, DeliveryStatus::Blocked);
    assert_eq!(completion.error.as_deref(), Some(detail));
    assert!(completion.to_notification().contains(detail));
    let records = runtime.tasks().snapshot();
    assert!(records
        .iter()
        .filter(|task| task.task_id != first.task_id)
        .all(|task| task.state == WorkflowTaskState::Cancelled));
    assert!(runtime.tasks().active().is_empty());
    let _ = launch.await.unwrap();
}

/// [回归测试] 终态 run 退役后必须回收事件水位与 run/agent 取消栅栏（#468）。
///
/// 隔离运行结果：8 个 run 完成后把清理时钟推进 301 秒，可见 run 归零、环形日志
/// 只剩保留窗口内的条目，但仍能按 8/8 个退役 run ID 查到水位。
#[test]
fn retired_runs_release_event_watermarks_and_cancel_fences() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );

    let run_count: u64 = 8;
    let mut completed_at = None;
    for index in 0..run_count {
        let run_id = format!("run-{index}");
        apply_run_started(&runtime, &run_id);
        // Log 也会进入 run 事件日志（水位前进），但不改变 run 可读状态。
        runtime
            .progress_store()
            .apply_event(&crate::protocol::ProgressEvent::Log {
                run_id: run_id.clone(),
                message: "noise".into(),
            });
        let _ = runtime
            .tasks()
            .publish(format!("t-{index}"), task_params(&run_id, index));
        runtime
            .tasks()
            .cancel_run(&run_id, Some("workflow run killed".into()));
        runtime
            .tasks()
            .cancel_agent(&run_id, index + 1000, Some("agent killed".into()));
        apply_run_done(&runtime, &run_id, "killed");
        let snapshot = runtime
            .progress_store()
            .run_snapshot(&run_id)
            .expect("the fixture run must be visible before retirement");
        completed_at = snapshot.completed_at;
        assert!(
            runtime.event_watermark(&run_id) > 0,
            "可见 run 的已提交事件必须留下水位"
        );
        assert_eq!(
            publish_state(&runtime, &run_id, index + 100, "before"),
            Some(crate::runtime::WorkflowTaskState::Cancelled),
            "退役前迟到任务仍受 run 级取消栅栏约束"
        );
        assert_eq!(
            publish_state(&runtime, &run_id, index + 1000, "before"),
            Some(crate::runtime::WorkflowTaskState::Cancelled),
            "退役前迟到任务仍受 agent 级取消栅栏约束"
        );
        // 生产者的终态收敛点（runner 返回 + completion 投影完成）：run 仍可见。
        runtime.progress_store().note_producer_finished(&run_id);
    }

    // 退役前：水位仍单调可读（清理时钟未过保留窗口时不得回收）。
    let completed_at = completed_at.expect("the fixture run must be terminal");
    runtime
        .progress_store()
        .cleanup_completed_at(completed_at + std::time::Duration::from_secs(1));
    assert_eq!(
        runtime.progress_store().list_runs().len(),
        run_count as usize
    );
    for index in 0..run_count {
        assert!(
            runtime.event_watermark(&format!("run-{index}")) > 0,
            "可见 run 的水位不得因清理尝试而回退或消失"
        );
    }

    // 退役：清理时钟推进 301 秒（超过 300 秒保留窗口）。
    runtime
        .progress_store()
        .cleanup_completed_at(completed_at + std::time::Duration::from_secs(301));
    assert!(
        runtime.progress_store().list_runs().is_empty(),
        "终态 run 必须按保留窗口退役"
    );
    for index in 0..run_count {
        let run_id = format!("run-{index}");
        assert_eq!(
            runtime.event_watermark(&run_id),
            0,
            "退役 run 的水位必须随 run 一起回收"
        );
        assert_eq!(
            publish_state(&runtime, &run_id, index + 100, "after"),
            Some(crate::runtime::WorkflowTaskState::Published),
            "退役后不再有生产者，run 级取消栅栏不得继续驻留"
        );
        assert_eq!(
            publish_state(&runtime, &run_id, index + 1000, "after"),
            Some(crate::runtime::WorkflowTaskState::Published),
            "退役后不再有生产者，agent 级取消栅栏不得继续驻留"
        );
    }
}

/// [回归测试] 回收必须等 run 退役与生产者结束同时成立（#468）：退役但生产者
/// 仍可能发布时，水位与取消栅栏都不得提前释放。
#[test]
fn retirement_without_producer_end_keeps_watermark_and_cancel_fence() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = "run-live-producer";
    apply_run_started(&runtime, run_id);
    runtime
        .progress_store()
        .apply_event(&crate::protocol::ProgressEvent::Log {
            run_id: run_id.to_string(),
            message: "noise".into(),
        });
    let _ = runtime
        .tasks()
        .publish("t-live".into(), task_params(run_id, 1));
    runtime
        .tasks()
        .cancel_run(run_id, Some("workflow run killed".into()));
    apply_run_done(&runtime, run_id, "killed");
    let completed_at = runtime
        .progress_store()
        .run_snapshot(run_id)
        .and_then(|snapshot| snapshot.completed_at)
        .expect("the fixture run must be terminal");

    // 清理未过保留窗口：run 仍可见，水位保持。
    runtime
        .progress_store()
        .cleanup_completed_at(completed_at + std::time::Duration::from_secs(1));
    assert_eq!(runtime.progress_store().list_runs().len(), 1);
    assert!(runtime.event_watermark(run_id) > 0);

    // 退役但生产者未结束：水位与栅栏继续驻留（迟到任务仍受取消约束）。
    runtime
        .progress_store()
        .cleanup_completed_at(completed_at + std::time::Duration::from_secs(301));
    assert!(runtime.progress_store().list_runs().is_empty());
    assert!(
        runtime.event_watermark(run_id) > 0,
        "生产者未结束前不得回收水位"
    );
    assert_eq!(
        publish_state(&runtime, run_id, 7, "before-producer-end"),
        Some(crate::runtime::WorkflowTaskState::Cancelled),
        "生产者未结束前迟到任务仍受取消约束"
    );

    // 生产者结束：立即回收已退役 run 的水位与栅栏。
    runtime.progress_store().note_producer_finished(run_id);
    assert_eq!(runtime.event_watermark(run_id), 0);
    assert_eq!(
        publish_state(&runtime, run_id, 7, "after-producer-end"),
        Some(crate::runtime::WorkflowTaskState::Published),
        "生产者结束后不再有迟到发布，栅栏可以回收"
    );
}

/// [回归测试] 最后一个 run 结束后没有后续 workflow 工作：实例必须按最近的终态
/// 到期时间自行唤醒清扫，释放含完整输出的进度快照（内存审查 P1-B）。
#[tokio::test(start_paused = true)]
async fn idle_instance_releases_terminal_run_at_retention_expiry() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = "run-idle";
    apply_run_started(&runtime, run_id);
    apply_run_done(&runtime, run_id, "completed");

    // 未到期：完整结果仍可按产品契约查询。
    advance_sweeper_clock(std::time::Duration::from_secs(299)).await;
    assert!(
        runtime.run_snapshot(run_id).is_some(),
        "五分钟保留窗口内的终态快照必须可查"
    );

    // 到期：即使没有下一次 workflow 工作，快照也必须释放。
    advance_sweeper_clock(std::time::Duration::from_secs(2)).await;
    assert!(
        runtime.run_snapshot(run_id).is_none(),
        "空闲实例必须按到期释放最后一批完整结果（P1-B）"
    );
}

/// [回归测试] 新 run 到达重置清扫定时器：到期时间按当前终态重新计算，运行中的
/// run 不被旧截止时刻提前淘汰（内存审查 P1-B）。
#[tokio::test(start_paused = true)]
async fn new_run_resets_expiry_deadline() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    apply_run_started(&runtime, "run-a");
    apply_run_done(&runtime, "run-a", "completed");

    // 新 run 在 run-a 的保留窗口内到达：定时器按当前状态重置。
    advance_sweeper_clock(std::time::Duration::from_secs(120)).await;
    apply_run_started(&runtime, "run-b");

    // run-a 到期：只退役自身；运行中的 run-b 继续可见。
    advance_sweeper_clock(std::time::Duration::from_secs(181)).await;
    assert!(
        runtime.run_snapshot("run-a").is_none(),
        "最早的终态到期时刻必须仍被唤醒清扫"
    );
    assert!(
        runtime.run_snapshot("run-b").is_some(),
        "运行中的 run 不得被上一次的到期时刻淘汰"
    );

    // run-b 结束后按它自己的完成时刻重新开窗。
    apply_run_done(&runtime, "run-b", "completed");
    advance_sweeper_clock(std::time::Duration::from_secs(299)).await;
    assert!(
        runtime.run_snapshot("run-b").is_some(),
        "新终态的保留窗口从自身完成时刻起算"
    );
    advance_sweeper_clock(std::time::Duration::from_secs(2)).await;
    assert!(
        runtime.run_snapshot("run-b").is_none(),
        "新终态同样按到期释放"
    );
}

/// [回归测试] 实例关闭撤销到期唤醒：关闭后不再有定时清扫（内存审查 P1-B）。
#[tokio::test(start_paused = true)]
async fn close_cancels_expiry_sweep() {
    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = "run-closed";
    apply_run_started(&runtime, run_id);
    apply_run_done(&runtime, run_id, "completed");

    runtime.mark_closed();
    // 关闭后迟到的终态事件也不得重新登记唤醒。
    apply_run_done(&runtime, run_id, "completed");

    advance_sweeper_clock(std::time::Duration::from_secs(301)).await;
    assert!(
        runtime.run_snapshot(run_id).is_some(),
        "实例关闭后清扫唤醒必须撤销"
    );
}

/// [回归测试] 到期清扫只退役进度快照：未 ACK 的完成通知、迟到事件的取消栅栏与
/// 事件水位仍按既有保留窗口和生产者结算语义保留（内存审查 P1-B）。
#[tokio::test(start_paused = true)]
async fn expiry_keeps_unacked_completion_and_cancel_fences() {
    use peri_acp_types::workflow::{
        AcceptanceStatus, DeliveryStatus, ExecutionStatus, PostProcessingStatus,
    };

    let dir = tempfile::tempdir().expect("the temporary test directory must be available");
    let runtime = make_runtime(
        dir.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    );
    let run_id = "run-delivery";
    apply_run_started(&runtime, run_id);
    runtime
        .tasks()
        .cancel_run(run_id, Some("workflow run killed".into()));
    apply_run_done(&runtime, run_id, "killed");

    let ticket = runtime
        .registry()
        .reserve(crate::registry::WorkflowRun {
            run_id: run_id.into(),
            workflow_name: "t".into(),
            script_preview: "return 1".into(),
            status: crate::registry::WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: None,
            kill_tx: None,
        })
        .expect("the run reservation fixture result is required for this assertion");
    assert!(runtime.registry().complete(
        &ticket,
        crate::registry::WorkflowTaskResult {
            run_id: run_id.into(),
            workflow_name: "t".into(),
            success: false,
            status: crate::registry::WorkflowRunStatus::Killed,
            execution_status: ExecutionStatus::Killed,
            acceptance_status: AcceptanceStatus::Unknown,
            post_processing_status: PostProcessingStatus::Unknown,
            delivery_status: DeliveryStatus::Blocked,
            outcome: Default::default(),
            state_artifact_exists: false,
            duration_ms: 0,
            agent_count: 0,
            tool_calls_count: 0,
            error: Some("killed".into()),
            result_preview: None,
            phase_summaries: Vec::new(),
            attempts: Vec::new(),
        },
        peri_acp_types::tasks::TaskRecord {
            task_id: run_id.into(),
            kind: peri_acp_types::tasks::BgTaskKind::Workflow,
            summary: "t".into(),
            started_at: "2026-11-11T00:00:00Z".into(),
            status: "cancelled".into(),
            duration_ms: 0,
            output_preview: None,
            initiator_session_id: None,
            child_thread_id: None,
            external_owner: None,
        },
    ));

    // 窗口内：迟到任务仍受取消栅栏约束，完成通知等待 ACK。
    advance_sweeper_clock(std::time::Duration::from_secs(299)).await;
    assert_eq!(
        publish_state(&runtime, run_id, 21, "in-window"),
        Some(crate::runtime::WorkflowTaskState::Cancelled),
        "可见 run 的取消栅栏不得被清扫提前释放"
    );
    assert_eq!(runtime.registry().pending_completions().len(), 1);

    // 到期：只退役进度快照；生产者未结算时水位与栅栏继续驻留。
    advance_sweeper_clock(std::time::Duration::from_secs(2)).await;
    assert!(runtime.run_snapshot(run_id).is_none());
    assert!(
        runtime.event_watermark(run_id) > 0,
        "生产者未结束前事件水位不得回收"
    );
    assert_eq!(
        publish_state(&runtime, run_id, 22, "after-expiry"),
        Some(crate::runtime::WorkflowTaskState::Cancelled),
        "生产者未结束前迟到任务仍受取消约束"
    );
    assert_eq!(
        runtime.registry().pending_completions().len(),
        1,
        "未 ACK 的完成通知不随进度快照退役"
    );

    // 生产者结束：既有回收语义不变。
    runtime.progress_store().note_producer_finished(run_id);
    assert_eq!(runtime.event_watermark(run_id), 0);
    assert_eq!(
        publish_state(&runtime, run_id, 23, "after-producer"),
        Some(crate::runtime::WorkflowTaskState::Published)
    );
    assert_eq!(
        runtime.registry().pending_completions().len(),
        1,
        "完成通知仍等宿主 ACK"
    );
}

/// 推进暂停的 tokio 时钟，并给到期清扫任务一次运行机会。
async fn advance_sweeper_clock(duration: std::time::Duration) {
    tokio::time::advance(duration).await;
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn apply_run_started(runtime: &WorkflowRuntime, run_id: &str) {
    runtime
        .progress_store()
        .apply_event(&crate::protocol::ProgressEvent::RunStarted {
            run_id: run_id.to_string(),
            workflow_name: "t".into(),
            meta: None,
        });
}

fn apply_run_done(runtime: &WorkflowRuntime, run_id: &str, status: &str) {
    runtime
        .progress_store()
        .apply_event(&crate::protocol::ProgressEvent::RunDone {
            run_id: run_id.to_string(),
            status: status.to_string(),
            return_value: None,
            error: None,
        });
}

/// 发布一个迟到任务并返回其提交后的状态（不存在时 `None`）。
fn publish_state(
    runtime: &WorkflowRuntime,
    run_id: &str,
    agent_id: u64,
    phase: &str,
) -> Option<crate::runtime::WorkflowTaskState> {
    let task_id = format!("{run_id}-probe-{agent_id}-{phase}");
    match runtime
        .tasks()
        .publish(task_id, task_params(run_id, agent_id))
    {
        crate::runtime::PublishOutcome::Published(record) => Some(record.state),
        crate::runtime::PublishOutcome::AlreadyKnown(record) => Some(record.state),
        crate::runtime::PublishOutcome::Closed => None,
    }
}
