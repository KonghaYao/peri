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
        git_baseline: None,
    }
}

#[tokio::test]
async fn closed_runtime_rejects_new_runs_on_both_entry_points() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());

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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());

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
    let journal = WorkflowJournalStore::new(dir.path().to_str().unwrap());
    journal.init_run(&requested, "return 1").unwrap();
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
                write_intent: None,
                limits: Default::default(),
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
        .unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
    runtime.tasks().publish(
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
#[test]
fn event_watermark_does_not_regress_when_events_are_evicted() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());

    runtime.run_events().append(run_event("run-1"));
    for _ in 0..=super::RUN_EVENT_LOG_CAPACITY {
        runtime.run_events().append(run_event("run-2"));
    }

    assert_eq!(
        runtime.event_watermark("run-1"),
        1,
        "run-1 的事件被淘汰后水位仍须保持在其已提交值"
    );
    assert_eq!(
        runtime.event_watermark("run-2"),
        (super::RUN_EVENT_LOG_CAPACITY + 1) as u64
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime(dir.path().to_str().unwrap());
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
