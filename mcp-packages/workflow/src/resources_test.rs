//! `resources.rs` 的行为测试：URI 形状、分页参数、订阅 URI 映射与读取契约
//! （活跃任务基线、已提交水位；design §4.3 补读协议）。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::workflow::{AgentExecutor, AgentRunParams, AgentRunResult, Usage};
use peri_workflow::protocol::ProgressEvent;
use peri_workflow::runtime::{SettleAdmission, WorkflowRuntime, WorkflowTaskState};

use super::*;

struct NoopExecutor;

#[async_trait]
impl AgentExecutor for NoopExecutor {
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

fn make_runtime() -> Arc<WorkflowRuntime> {
    let (notification_tx, _) = tokio::sync::broadcast::channel(32);
    Arc::new(WorkflowRuntime::new(
        Arc::new(NoopExecutor),
        std::env::temp_dir().to_str().expect("temp dir 可用"),
        notification_tx,
        None,
    ))
}

fn published_params(run_id: &str, agent_id: u64) -> AgentRunParams {
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

fn completed_workflow(run_id: &str) -> peri_workflow::registry::WorkflowTaskResult {
    peri_workflow::registry::WorkflowTaskResult {
        run_id: run_id.into(),
        workflow_name: "retained source".into(),
        success: true,
        status: peri_workflow::registry::WorkflowRunStatus::Completed,
        execution_status: Default::default(),
        acceptance_status: Default::default(),
        post_processing_status: Default::default(),
        delivery_status: Default::default(),
        state_artifact_exists: false,
        duration_ms: 1,
        agent_count: 0,
        tool_calls_count: 0,
        error: None,
        phase_summaries: vec![],
        attempts: vec![],
    }
}

#[tokio::test]
async fn unacknowledged_completions_keep_source_records_after_display_eviction() {
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskShutdownReport};
    use peri_workflow::registry::{WorkflowRun, WorkflowRunStatus};

    let runtime = make_runtime();
    let _input = crate::WorkflowInstanceInput::new(Arc::clone(&runtime));
    let owner = runtime.bg_registry().unwrap();
    let mut first_started_at = None;
    for index in 0..600 {
        let run_id = format!("unacked-{index}");
        let source_ticket = runtime
            .registry()
            .reserve(WorkflowRun {
                run_id: run_id.clone(),
                workflow_name: "retained source".into(),
                script_preview: "return 1".into(),
                status: WorkflowRunStatus::Running,
                started_at: std::time::Instant::now(),
                child_handle: None,
                kill_tx: None,
            })
            .unwrap();
        let owner_ticket = owner
            .register(BgTaskRegistration {
                task_id: run_id.clone(),
                kind: BgTaskKind::Workflow,
                summary: "retained source".into(),
                pid: None,
                kill: None,
            })
            .unwrap();
        let record = owner
            .snapshot()
            .tasks
            .into_iter()
            .find(|task| task.task_id == run_id)
            .unwrap();
        if index == 0 {
            first_started_at = Some(record.started_at.clone());
        }
        let terminal = completed_workflow(&run_id);
        assert!(runtime
            .registry()
            .complete(&source_ticket, terminal.clone(), record));
        owner.confirm_external_execution_stopped(&owner_ticket);
        owner
            .settle_completed(
                &owner_ticket,
                terminal.background_result(),
                Arc::new(|_, _| Ok(())),
            )
            .unwrap();
    }
    let display = owner.snapshot();
    assert!(
        display.tasks.len() < 600,
        "the display window must actually evict old terminals"
    );
    assert!(display
        .tasks
        .iter()
        .all(|record| record.task_id != "unacked-0"));

    let payload = read_payload(&runtime, "workflow://background-tasks").unwrap();
    assert_eq!(payload["completions"].as_array().unwrap().len(), 600);
    let source_records = payload["snapshot"]["tasks"].as_array().unwrap();
    assert_eq!(source_records.len(), 600);
    let first = source_records
        .iter()
        .find(|record| record["task_id"] == "unacked-0")
        .unwrap();
    assert_eq!(first["started_at"].as_str(), first_started_at.as_deref());
    assert_eq!(first["status"], "completed");

    for index in 0..600 {
        runtime
            .registry()
            .acknowledge_completion(&format!("unacked-{index}"));
    }
    let payload = read_payload(&runtime, "workflow://background-tasks").unwrap();
    assert!(payload["completions"].as_array().unwrap().is_empty());
    assert!(runtime.registry().list_runs().is_empty());
    assert!(payload["snapshot"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|record| record["task_id"] != "unacked-0"));
    assert_eq!(owner.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn source_ack_before_owner_settlement_cannot_republish_a_running_task() {
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskShutdownReport};
    use peri_workflow::registry::{WorkflowRun, WorkflowRunStatus};

    let runtime = make_runtime();
    let _input = crate::WorkflowInstanceInput::new(Arc::clone(&runtime));
    let owner = runtime.bg_registry().unwrap();
    let source_ticket = runtime
        .registry()
        .reserve(WorkflowRun {
            run_id: "fast-ack".into(),
            workflow_name: "fast ACK".into(),
            script_preview: "return 1".into(),
            status: WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: None,
            kill_tx: None,
        })
        .unwrap();
    let owner_ticket = owner
        .register(BgTaskRegistration {
            task_id: "fast-ack".into(),
            kind: BgTaskKind::Workflow,
            summary: "fast ACK".into(),
            pid: None,
            kill: None,
        })
        .unwrap();
    let record = owner.snapshot().tasks.pop().unwrap();
    let result = completed_workflow("fast-ack");
    assert!(runtime
        .registry()
        .complete(&source_ticket, result.clone(), record));
    runtime.registry().acknowledge_completion("fast-ack");
    assert_eq!(
        owner.snapshot().tasks[0].status,
        "running",
        "reproduce the window before owner settlement"
    );
    let payload = read_payload(&runtime, "workflow://background-tasks").unwrap();
    assert!(payload["completions"].as_array().unwrap().is_empty());
    assert!(
        payload["snapshot"]["tasks"].as_array().unwrap().is_empty(),
        "an acknowledged source must not be re-admitted from a stale display row"
    );

    owner.confirm_external_execution_stopped(&owner_ticket);
    owner
        .settle_completed(
            &owner_ticket,
            result.background_result(),
            Arc::new(|_, _| Ok(())),
        )
        .unwrap();
    assert_eq!(owner.shutdown().await, TaskShutdownReport::Complete);
}

/// F3 回归：长寿命活跃任务的发布事件被事件日志淘汰后，读取方必须仍能从
/// 任务资源返回的基线快照找回未终态任务（不能只依赖事件页）。
#[test]
fn tasks_baseline_survives_event_log_eviction() {
    let runtime = make_runtime();
    // run-1 已登记（run 级任务视图的存在性以 progress store 裁决）。
    runtime
        .progress_store()
        .apply_event(&ProgressEvent::RunStarted {
            run_id: "run-1".into(),
            workflow_name: "t".into(),
            meta: None,
        });
    // 长寿命活跃任务：published 后一直未被消费者 claim（其后没有它的事件）。
    runtime
        .tasks()
        .publish("t-long".to_string(), published_params("run-1", 1));
    // 其他任务产生超过任务事件日志容量（512）的事件，淘汰 t-long 的发布事件。
    for index in 0..600u64 {
        let task_id = format!("t-filler-{index}");
        runtime
            .tasks()
            .publish(task_id.clone(), published_params("run-2", index));
        runtime.tasks().settle(
            "run-2",
            &task_id,
            SettleAdmission::AnyActive,
            WorkflowTaskState::Completed,
            None,
            None,
        );
    }

    let payload = read_payload(&runtime, "workflow://tasks").expect("任务流必须可读");
    assert!(
        payload["truncatedBefore"].as_u64().unwrap_or(0) > 1,
        "前提：保留边界已前进"
    );
    let entries = payload["entries"].as_array().expect("entries 是数组");
    assert!(
        entries
            .iter()
            .all(|event| event["taskId"].as_str() != Some("t-long")),
        "前提：t-long 的发布事件已被淘汰"
    );
    let active = payload["activeTasks"]
        .as_array()
        .expect("任务资源必须随页返回活跃任务基线");
    assert!(
        active
            .iter()
            .any(|record| record["taskId"].as_str() == Some("t-long")),
        "活跃任务必须可从基线重建：{payload}"
    );

    // run 级任务视图同样携带基线（同一 run 的活跃任务）。
    let run_payload =
        read_payload(&runtime, "workflow://runs/run-1/tasks").expect("run 任务视图必须可读");
    let run_active = run_payload["activeTasks"]
        .as_array()
        .expect("run 任务视图必须随页返回活跃任务基线");
    assert!(
        run_active
            .iter()
            .any(|record| record["taskId"].as_str() == Some("t-long")),
        "run 级基线必须只含该 run 的活跃任务：{run_payload}"
    );
}

/// F4 回归：run 事件被保留窗口淘汰后 `eventWatermark` 不得倒退为 0
/// （已提交水位不因可读窗口收缩而回退）。
#[test]
fn run_snapshot_watermark_does_not_regress_after_eviction() {
    let runtime = make_runtime();
    runtime
        .progress_store()
        .apply_event(&ProgressEvent::RunStarted {
            run_id: "run-wm".into(),
            workflow_name: "t".into(),
            meta: None,
        });
    // 洪水事件把 run-wm 的事件挤出保留窗口（容量 1024）。
    for index in 0..1_100u64 {
        runtime.progress_store().apply_event(&ProgressEvent::Log {
            run_id: "run-flood".into(),
            message: format!("noise {index}"),
        });
    }

    let payload = read_payload(&runtime, "workflow://runs/run-wm").expect("已登记 run 必须可读");
    assert!(
        payload["status"].as_str() == Some("running"),
        "前提：run 快照仍在"
    );
    assert_eq!(
        payload["eventWatermark"].as_u64(),
        Some(1),
        "已提交水位不得因事件淘汰而倒退：{payload}"
    );
}

#[test]
fn parses_registered_uri_shapes() {
    assert_eq!(parse_uri("workflow://runs"), Some(WorkflowResource::Runs));
    assert_eq!(parse_uri("workflow://tasks"), Some(WorkflowResource::Tasks));
    assert_eq!(
        parse_uri("workflow://runs/0199-a"),
        Some(WorkflowResource::Run {
            run_id: "0199-a".to_string()
        })
    );
    assert_eq!(
        parse_uri("workflow://runs/0199-a/events"),
        Some(WorkflowResource::RunEvents {
            run_id: "0199-a".to_string()
        })
    );
    assert_eq!(
        parse_uri("workflow://runs/0199-a/tasks"),
        Some(WorkflowResource::RunTasks {
            run_id: "0199-a".to_string()
        })
    );
    // query 串不影响形状解析。
    assert_eq!(
        parse_uri("workflow://tasks?after=3&limit=2"),
        Some(WorkflowResource::Tasks)
    );
}

#[test]
fn rejects_unregistered_uri_shapes() {
    for uri in [
        "workflow://",
        "workflow://runs/",
        "workflow://runs/a/b",
        "workflow://runs/a/events/extra",
        "workspace://git/ref",
        "https://example.com",
    ] {
        assert!(parse_uri(uri).is_none(), "{uri} 不得被解析为资源");
    }
}

#[test]
fn parses_page_query_with_bounds() {
    let default = parse_page_query("workflow://tasks");
    assert_eq!(default.after, None);
    assert_eq!(default.limit, DEFAULT_PAGE_LIMIT);

    let parsed = parse_page_query("workflow://tasks?after=7&limit=3");
    assert_eq!(parsed.after, Some(7));
    assert_eq!(parsed.limit, 3);

    // limit 收窄到 [1, MAX_PAGE_LIMIT]；非法值回落缺省。
    assert_eq!(parse_page_query("workflow://tasks?limit=0").limit, 1);
    assert_eq!(
        parse_page_query(&format!("workflow://tasks?limit={}", MAX_PAGE_LIMIT + 50)).limit,
        MAX_PAGE_LIMIT
    );
    assert_eq!(
        parse_page_query("workflow://tasks?limit=abc").limit,
        DEFAULT_PAGE_LIMIT
    );
    assert_eq!(parse_page_query("workflow://tasks?after=abc").after, None);
}

#[test]
fn maps_updates_to_canonical_uris() {
    assert_eq!(
        update_uri(&WorkflowResourceUpdate::RunsIndex),
        "workflow://runs"
    );
    assert_eq!(
        update_uri(&WorkflowResourceUpdate::Run {
            run_id: "r1".to_string()
        }),
        "workflow://runs/r1"
    );
    assert_eq!(
        update_uri(&WorkflowResourceUpdate::RunEvents {
            run_id: "r1".to_string()
        }),
        "workflow://runs/r1/events"
    );
    assert_eq!(
        update_uri(&WorkflowResourceUpdate::RunTasks {
            run_id: "r1".to_string()
        }),
        "workflow://runs/r1/tasks"
    );
    assert_eq!(
        update_uri(&WorkflowResourceUpdate::Tasks),
        "workflow://tasks"
    );
}
