//! `WorkflowMcpServer` 的 wire 测试（生产 handler + rmcp Auto lifecycle client）。
//!
//! 覆盖口径（design §4）：工具/资源声明面、就绪屏障、参数拒绝路径、
//! `Workflow` 的 resume-only 路由（design §6.3）、
//! host-only 回填的服务端授权与准入、`(runId, taskId)` 键的任务事件读取、
//! run 清理后的资源 not-found、单 run 快照的 `eventWatermark`。
//! 真实链路：server 半边是生产 handler（`rmcp::serve_server`），client 半边是
//! rmcp 原生 client；`tools/list`、`tools/call`、`resources/read` 都经真实 wire。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::workflow::AgentRunResult;
use peri_workflow::runtime::WORKFLOW_CONSUMER_META_KEY;
use peri_workflow::runtime::{ClaimOutcome, PublishOutcome, WorkflowTaskState};
use rmcp::model::{ServerNotification, SubscriptionFilter};
use serde_json::{json, Value};

use super::test_fixture::*;
use super::WorkflowMcpServer;
use crate::input::{BindOutcome, WorkflowInstanceInput};
#[tokio::test]
async fn wire_declares_four_tools_and_five_resources() {
    let pair = connect(server(make_runtime())).await;
    let peer = pair.peer();

    let tools = peer.list_all_tools().await.expect("tools/list 必须成功");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec![
            "Workflow",
            "WorkflowStatus",
            "WorkflowKill",
            "WorkflowAgentResult"
        ],
        "handler 必须恰好声明四条工具（顺序即声明段顺序）"
    );
    let workflow_tool = &tools[0];
    let properties = workflow_tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("Workflow 参数必须是对象 schema");
    assert!(
        properties.contains_key("script") && properties.contains_key("writeIntent"),
        "Workflow 参数 schema 必须与 runtime 的单一权威一致"
    );
    let max_concurrency = &properties["maxConcurrency"];
    assert_eq!(
        max_concurrency.get("type").and_then(Value::as_str),
        Some("integer"),
        "maxConcurrency 在 wire 上必须是整数（与运行时解析一致）"
    );
    assert_eq!(
        max_concurrency.get("minimum").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        max_concurrency.get("maximum").and_then(Value::as_u64),
        Some(16),
        "wire 上限必须与运行时 MAX_CONCURRENCY_CAP 一致"
    );
    assert_eq!(
        max_concurrency.get("default").and_then(Value::as_u64),
        Some(3)
    );

    let resources = peer
        .list_resources(None)
        .await
        .expect("resources/list 必须成功");
    let uris: Vec<&str> = resources.resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec![
            "workflow://runs",
            "workflow://runs/{runId}",
            "workflow://runs/{runId}/events",
            "workflow://runs/{runId}/tasks",
            "workflow://tasks",
        ],
        "资源面必须恰好五条"
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn wire_reads_empty_runs_and_status() {
    let pair = connect(server(make_runtime())).await;
    let peer = pair.peer();

    let runs = read_resource(&peer, "workflow://runs")
        .await
        .expect("空 run 索引必须可读");
    assert_eq!(first_resource_text(&runs), "[]");

    let status = call_tool(&peer, "WorkflowStatus", json!({})).await;
    assert_ne!(status.is_error, Some(true), "空状态查询不是失败");
    assert_eq!(first_text(&status), "[]");

    let snapshot = call_tool(&peer, "WorkflowStatus", json!({"runId": "missing"})).await;
    assert_eq!(snapshot.is_error, Some(true), "未知 run 必须报错");
    assert!(first_text(&snapshot).contains("not found"));

    let events = read_resource(&peer, "workflow://runs/missing/events").await;
    assert!(events.is_err(), "未登记的 run 事件必须 not-found");

    pair.shutdown().await;
}

#[tokio::test]
async fn workflow_start_is_rejected_before_consumer_ready() {
    let pair = connect(server(make_runtime())).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({"script": "return 1", "writeIntent": {"kind": "read_only"}}),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "就绪屏障必须先于启动生效");
    assert!(
        first_text(&result).contains("not ready"),
        "错误文本必须说明消费者未就绪: {}",
        first_text(&result)
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn workflow_start_rejects_strict_preflight_without_launching_node() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({"script": "return 1", "strictPreflight": true}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        first_text(&result).contains("strict preflight"),
        "strictPreflight 必须在任何启动动作前被拒: {}",
        first_text(&result)
    );
    assert!(
        runtime.runs_snapshot().is_empty(),
        "被拒的启动不得留下 run 记录"
    );

    pair.shutdown().await;
}

/// resume-only（design §6.3）：`resumeFromRunId` 且无 `script`/`scriptPath` 走内核
/// `resume`——state/journal 读取在核内完成，不再以「缺 script」拒绝；run 不存在时
/// 以内核 state 读取失败文本收敛（工具级），且不登记 run。
#[tokio::test]
async fn workflow_resume_only_routes_to_kernel_resume() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let missing_run = uuid::Uuid::now_v7().to_string();
    let result = call_tool(&peer, "Workflow", json!({"resumeFromRunId": missing_run})).await;
    assert_eq!(
        result.is_error,
        Some(true),
        "无 state 的 resume 必须以工具级错误收敛"
    );
    let text = first_text(&result);
    assert!(
        text.contains("Failed to read workflow state"),
        "resume-only 必须进入内核 resume 分支: {text}"
    );
    assert!(
        !text.contains("missing 'script'"),
        "resume-only 不得被 start 分支的缺参校验拦截: {text}"
    );
    assert!(
        runtime.runs_snapshot().is_empty(),
        "失败的 resume 不得留下 run 记录"
    );

    pair.shutdown().await;
}

/// F11：resume-only 不得静默忽略启动期覆盖参数——只接受 `resumeFromRunId`。
#[tokio::test]
async fn workflow_resume_only_rejects_new_limits_instead_of_ignoring() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({
            "resumeFromRunId": uuid::Uuid::now_v7().to_string(),
            "maxAgents": 1
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "覆盖参数必须显式拒绝");
    let text = first_text(&result);
    assert!(text.contains("maxAgents"), "错误必须点名字段: {text}");
    assert!(text.contains("resume-only"), "{text}");
    assert!(
        text.contains("script + resumeFromRunId"),
        "错误必须给出改变输入的路径: {text}"
    );
    assert!(
        !text.contains("Failed to read workflow state"),
        "拒绝必须发生在 state 读取前，不能被静默忽略: {text}"
    );
    assert!(
        runtime.runs_snapshot().is_empty(),
        "被拒的 resume 不得留下 run 记录"
    );

    pair.shutdown().await;
}

/// `strictPreflight:true` 在 resume-only 与 start 路径使用同一拒绝文案。
#[tokio::test]
async fn workflow_resume_only_rejects_strict_preflight_like_start() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({
            "resumeFromRunId": uuid::Uuid::now_v7().to_string(),
            "strictPreflight": true
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let text = first_text(&result);
    assert!(
        text.contains("strict preflight is unavailable"),
        "resume-only 必须拒绝 strictPreflight:true: {text}"
    );
    assert!(
        runtime.runs_snapshot().is_empty(),
        "被拒的 resume 不得留下 run 记录"
    );

    pair.shutdown().await;
}

/// resume-only 的 run id 校验仍归内核（与 `resumeFromRunId` 参数同一规则）：
/// 非法 uuid 在读取 state 前被拒，不回落为「缺 script」文本。
#[tokio::test]
async fn workflow_resume_only_rejects_invalid_run_id_before_io() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({"resumeFromRunId": "../not-a-run"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let text = first_text(&result);
    assert!(text.contains("Invalid resumeFromRunId"), "{text}");
    assert!(!text.contains("missing 'script'"), "{text}");

    pair.shutdown().await;
}

/// `resumeFromRunId` + `script` 仍是 start（journal 缓存重放的既有语义），不被
/// resume-only 分支劫持：以 start 独有的校验（strictPreflight 拒绝）作判别。
#[tokio::test]
async fn workflow_script_with_resume_from_still_uses_start() {
    let runtime = make_runtime();
    runtime.set_consumer_ready(true);
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let result = call_tool(
        &peer,
        "Workflow",
        json!({
            "script": "return 1",
            "resumeFromRunId": uuid::Uuid::now_v7().to_string(),
            "strictPreflight": true
        }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    let text = first_text(&result);
    assert!(
        text.contains("strict preflight"),
        "script + resumeFromRunId 必须仍走 start: {text}"
    );
    assert!(
        !text.contains("Failed to read workflow state"),
        "script 声明时不得改走 resume 分支: {text}"
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn agent_result_requires_the_bound_consumer_token() {
    let runtime = make_runtime();
    runtime
        .tasks()
        .publish("task-auth".to_string(), published_params("run-auth", 1));
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    // 消费者尚未绑定：即使携带令牌也拒绝（未绑定时不存在合法调用方）。
    let unbound = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-auth",
            "taskId": "task-auth",
            "result": {"kind": "skipped"}
        }),
        Some(consumer_meta("any-token")),
    )
    .await;
    assert_invalid_params(&unbound, "未绑定消费者");
    assert!(
        unbound.message.contains("not bound"),
        "错误必须点名消费者未绑定: {}",
        unbound.message
    );

    // 绑定后：缺 `_meta` 令牌与伪造令牌都拒绝（绑定的真令牌见其余用例）。
    runtime.bind_consumer_token();
    let missing = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-auth",
            "taskId": "task-auth",
            "result": {"kind": "skipped"}
        }),
        None,
    )
    .await;
    assert_invalid_params(&missing, "缺令牌");
    assert!(
        missing.message.contains(WORKFLOW_CONSUMER_META_KEY),
        "错误必须点名缺失的 `_meta` 键: {}",
        missing.message
    );

    let forged = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-auth",
            "taskId": "task-auth",
            "result": {"kind": "skipped"}
        }),
        Some(consumer_meta("forged-token")),
    )
    .await;
    assert_invalid_params(&forged, "伪造令牌");
    assert!(
        !forged.message.contains("forged-token"),
        "错误文本不得回显调用方令牌"
    );

    // 三次拒绝都不得改变任务记录。
    let record = runtime
        .tasks()
        .get("run-auth", "task-auth")
        .expect("任务必须仍存在");
    assert_eq!(record.state, WorkflowTaskState::Published);
    assert!(record.result.is_none(), "未授权调用不得写入结果");

    pair.shutdown().await;
}

#[tokio::test]
async fn agent_result_rejects_unclaimed_published_task() {
    let runtime = make_runtime();
    runtime
        .tasks()
        .publish("task-pub".to_string(), published_params("run-pub", 2));
    let token = runtime.bind_consumer_token();
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    // 回填只接受已被唯一 claim（`executing`）的任务：`published` 任务即使令牌正确
    // 也不得被结算（design §5.1）。
    let error = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-pub",
            "taskId": "task-pub",
            "result": {"kind": "ok", "output": "host result", "usage": {"outputTokens": 1}}
        }),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_invalid_params(&error, "未 claim 的 published 任务");
    assert!(
        error.message.contains("not 'executing'"),
        "错误必须说明准入条件: {}",
        error.message
    );

    let record = runtime
        .tasks()
        .get("run-pub", "task-pub")
        .expect("任务必须仍存在");
    assert_eq!(
        record.state,
        WorkflowTaskState::Published,
        "拒绝不得改变记录"
    );
    assert!(record.result.is_none(), "未 claim 的任务不得被写入结果");

    pair.shutdown().await;
}

#[tokio::test]
async fn agent_result_settles_claimed_task_once() {
    let runtime = make_runtime();
    runtime
        .tasks()
        .publish("task-exec".to_string(), published_params("run-exec", 3));
    // 消费者 claim（唯一执行权）：只有执行态接受回填。取消句柄由形参类型推断。
    let claimed = runtime
        .tasks()
        .mark_executing("run-exec", "task-exec", Default::default());
    assert!(
        matches!(claimed, ClaimOutcome::Claimed(_)),
        "claim 必须成功"
    );
    let token = runtime.bind_consumer_token();
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let settled = call_tool_with_meta(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-exec",
            "taskId": "task-exec",
            "result": {"kind": "ok", "output": "first", "usage": {"outputTokens": 1}}
        }),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_ne!(settled.is_error, Some(true), "{}", first_text(&settled));
    assert!(
        first_text(&settled).contains("settled as 'completed'"),
        "结算必须报出终态: {}",
        first_text(&settled)
    );

    // 重复回填（不同结果）幂等：返回既有终态，不覆盖首次结果。
    let repeated = call_tool_with_meta(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-exec",
            "taskId": "task-exec",
            "result": {"kind": "dead", "reason": "late retry"}
        }),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_ne!(repeated.is_error, Some(true));
    assert!(
        first_text(&repeated).contains("already settled as 'completed'"),
        "重复回填必须幂等返回既有终态: {}",
        first_text(&repeated)
    );
    let record = runtime
        .tasks()
        .get("run-exec", "task-exec")
        .expect("任务必须存在");
    assert!(
        matches!(record.result, Some(AgentRunResult::Ok { .. })),
        "首次结果为准，迟到结果不得覆盖"
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn unknown_task_result_is_a_tool_level_failure() {
    let runtime = make_runtime();
    let token = runtime.bind_consumer_token();
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    // 未知任务是业务失败（工具级 `is_error` 文本），不是请求本身不合法。
    let result = call_tool_with_meta(
        &peer,
        "WorkflowAgentResult",
        json!({
            "runId": "run-missing",
            "taskId": "task-missing",
            "result": {"kind": "skipped"}
        }),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "未知任务不得被受理");
    assert!(
        first_text(&result).contains("unknown workflow task"),
        "服务端必须点名未知任务: {}",
        first_text(&result)
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn task_events_reflect_published_tasks_and_are_vertically_scoped() {
    let runtime = make_runtime();
    register_run(&runtime, "run-1");
    runtime
        .tasks()
        .publish("task-1".to_string(), published_params("run-1", 7));
    runtime
        .tasks()
        .publish("task-2".to_string(), published_params("run-2", 9));
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    // session 级任务流：两条都在。
    let tasks = read_resource(&peer, "workflow://tasks")
        .await
        .expect("任务流必须可读");
    let payload: Value = serde_json::from_str(&first_resource_text(&tasks)).expect("任务流是 JSON");
    assert_eq!(payload["entries"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["headSequence"].as_u64(), Some(2));
    assert_eq!(payload["truncatedBefore"].as_u64(), Some(1));

    // 分页：只取 sequence > 1 的一条。
    let paged = read_resource(&peer, "workflow://tasks?after=1&limit=10")
        .await
        .expect("分页读取必须成功");
    let paged_payload: Value =
        serde_json::from_str(&first_resource_text(&paged)).expect("分页结果是 JSON");
    assert_eq!(paged_payload["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        paged_payload["entries"][0]["task"]["taskId"].as_str(),
        Some("task-2")
    );

    // run 级任务视图：只含该 run（run 已登记）。
    let run_tasks = read_resource(&peer, "workflow://runs/run-1/tasks")
        .await
        .expect("已登记 run 的任务视图必须可读");
    let run_payload: Value =
        serde_json::from_str(&first_resource_text(&run_tasks)).expect("run 任务视图是 JSON");
    assert_eq!(run_payload["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        run_payload["entries"][0]["task"]["taskId"].as_str(),
        Some("task-1")
    );

    // 未登记 run（任务表有记录但 run 索引没有，等价于清理后）：not-found。
    let missing = read_resource(&peer, "workflow://runs/run-2/tasks").await;
    assert!(missing.is_err(), "未登记 run 的任务视图必须 not-found");

    pair.shutdown().await;
}

#[tokio::test]
async fn run_scoped_resources_are_not_found_after_run_cleanup() {
    let runtime = make_runtime();
    // 任务表仍保留该 run 的记录（终态保留窗口 300s），progress 索引已无该 run：
    // 这正是 run 过保留窗口被清理后的状态（审查 M14）。
    runtime.tasks().publish(
        "task-orphan".to_string(),
        published_params("run-cleaned", 5),
    );
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    for uri in [
        "workflow://runs/run-cleaned",
        "workflow://runs/run-cleaned/events",
        "workflow://runs/run-cleaned/tasks",
    ] {
        assert!(
            read_resource(&peer, uri).await.is_err(),
            "{uri} 在 run 清理后必须 not-found"
        );
    }

    // session 级聚合仍保留该 run 的终态摘要（设计 §4.3 第 5 条）。
    let tasks = read_resource(&peer, "workflow://tasks")
        .await
        .expect("聚合任务流必须可读");
    let payload: Value = serde_json::from_str(&first_resource_text(&tasks)).expect("任务流是 JSON");
    assert_eq!(payload["entries"].as_array().map(Vec::len), Some(1));

    pair.shutdown().await;
}

#[tokio::test]
async fn run_snapshot_carries_event_watermark() {
    let runtime = make_runtime();
    register_run(&runtime, "run-wm");
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let snapshot = read_resource(&peer, "workflow://runs/run-wm")
        .await
        .expect("已登记 run 必须可读");
    let payload: Value =
        serde_json::from_str(&first_resource_text(&snapshot)).expect("快照是 JSON");
    // `RunStarted` 已提交一条 run 事件 ⇒ 已提交上界为 1。
    assert_eq!(
        payload["eventWatermark"].as_u64(),
        Some(1),
        "单 run 快照必须携带 eventWatermark"
    );
    // 既有字段保持不变（消费方 TUI 依赖字段名）。
    assert_eq!(payload["run_id"].as_str(), Some("run-wm"));
    assert_eq!(payload["workflow_name"].as_str(), Some("test-workflow"));
    assert_eq!(payload["status"].as_str(), Some("running"));

    pair.shutdown().await;
}

#[tokio::test]
async fn kill_tool_reports_missing_run_without_error_flag() {
    let pair = connect(server(make_runtime())).await;
    let peer = pair.peer();

    let result = call_tool(&peer, "WorkflowKill", json!({"runId": "run-missing"})).await;
    assert_ne!(
        result.is_error,
        Some(true),
        "未命中不是工具级失败（幂等语义）"
    );
    assert!(first_text(&result).contains("not found"));

    pair.shutdown().await;
}

#[tokio::test]
async fn kill_rejects_string_agent_id_without_killing_the_run() {
    let runtime = make_runtime();
    runtime
        .tasks()
        .publish("task-kill".to_string(), published_params("run-kill", 3));
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    // LLM 常见形态 `"agentId": "3"`：必须是参数错误，不得静默降级为整 run kill。
    let error = call_tool_protocol_error(
        &peer,
        "WorkflowKill",
        json!({"runId": "run-kill", "agentId": "3"}),
        None,
    )
    .await;
    assert_invalid_params(&error, "字符串 agentId");
    assert!(
        error.message.contains("agentId") && error.message.contains("integer"),
        "错误必须点名字段与期望类型: {}",
        error.message
    );
    let record = runtime
        .tasks()
        .get("run-kill", "task-kill")
        .expect("任务必须仍存在");
    assert_eq!(
        record.state,
        WorkflowTaskState::Published,
        "字符串 agentId 不得触发整 run kill（run kill 会取消该任务）"
    );

    // 对照：整数 agentId 走单 agent 路径，命中该 agent 的任务。
    let killed = call_tool(
        &peer,
        "WorkflowKill",
        json!({"runId": "run-kill", "agentId": 3}),
    )
    .await;
    assert_ne!(killed.is_error, Some(true));
    assert!(
        first_text(&killed).contains("Agent 3 in workflow run 'run-kill' killed."),
        "整数 agentId 必须走单 agent kill: {}",
        first_text(&killed)
    );
    assert_eq!(
        runtime
            .tasks()
            .get("run-kill", "task-kill")
            .expect("任务必须仍存在")
            .state,
        WorkflowTaskState::Cancelled,
        "单 agent kill 取消该 agent 的未终态任务"
    );

    pair.shutdown().await;
}

#[tokio::test]
async fn invalid_arguments_and_unknown_tools_are_protocol_errors() {
    let runtime = make_runtime();
    let token = runtime.bind_consumer_token();
    let pair = connect(server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let unknown = call_tool_protocol_error(&peer, "WorkflowNope", json!({}), None).await;
    assert_invalid_params(&unknown, "未知工具");
    assert!(
        unknown.message.contains("unknown workflow tool"),
        "{}",
        unknown.message
    );

    let missing_run = call_tool_protocol_error(&peer, "WorkflowKill", json!({}), None).await;
    assert_invalid_params(&missing_run, "缺 runId");
    assert!(
        missing_run.message.contains("missing 'runId'"),
        "{}",
        missing_run.message
    );

    let typed_run =
        call_tool_protocol_error(&peer, "WorkflowStatus", json!({"runId": 7}), None).await;
    assert_invalid_params(&typed_run, "runId 类型错误");
    assert!(
        typed_run.message.contains("expected a string"),
        "{}",
        typed_run.message
    );

    let typed_limit =
        call_tool_protocol_error(&peer, "WorkflowStatus", json!({"limit": "5"}), None).await;
    assert_invalid_params(&typed_limit, "limit 类型错误");
    assert!(
        typed_limit
            .message
            .contains("expected a non-negative integer"),
        "{}",
        typed_limit.message
    );

    let missing_task = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({"runId": "run-1", "result": {"kind": "skipped"}}),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_invalid_params(&missing_task, "缺 taskId");
    assert!(
        missing_task.message.contains("missing 'taskId'"),
        "{}",
        missing_task.message
    );

    let bad_result = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({"runId": "run-1", "taskId": "task-1", "result": {"kind": "nope"}}),
        Some(consumer_meta(&token)),
    )
    .await;
    assert_invalid_params(&bad_result, "result 反序列化失败");
    assert!(
        bad_result.message.contains("invalid 'result'"),
        "{}",
        bad_result.message
    );

    pair.shutdown().await;
}

/// 装配期时序（design §6.2）：实例在会话环境装配期 spawn，早于会话旧链构造
/// `WorkflowRuntime`；槽位未绑定时工具/资源/回填以**明确错误**收敛——不伪装成
/// 实例缺失、空结果或业务失败。
#[tokio::test]
async fn unbound_slot_reports_explicit_runtime_errors() {
    let pair = connect(WorkflowMcpServer::new(WorkflowInstanceInput::slot())).await;
    let peer = pair.peer();

    // 工具面：实例可见、参数合法，但运行时未就绪 ⇒ 工具级失败文本。
    let workflow = call_tool(&peer, "Workflow", json!({})).await;
    assert_eq!(workflow.is_error, Some(true), "未绑定时 Workflow 必须失败");
    assert!(
        first_text(&workflow).contains("not bound"),
        "{}",
        first_text(&workflow)
    );

    let status = call_tool(&peer, "WorkflowStatus", json!({})).await;
    assert_eq!(
        status.is_error,
        Some(true),
        "未绑定时 WorkflowStatus 必须失败"
    );
    assert!(
        first_text(&status).contains("not bound"),
        "{}",
        first_text(&status)
    );

    // 参数违约仍是协议级错误（先于槽位解析，N3 分层不因未绑定而改变）。
    let typed =
        call_tool_protocol_error(&peer, "WorkflowStatus", json!({"limit": "5"}), None).await;
    assert_invalid_params(&typed, "未绑定 + limit 类型错误");

    // 回填面：没有可校验的消费者绑定 ⇒ 协议级拒绝。
    let result = call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({"runId": "run-1", "taskId": "task-1", "result": {"kind": "skipped"}}),
        None,
    )
    .await;
    assert_invalid_params(&result, "未绑定槽位的回填");

    // 资源面：不伪造空结果（内部错误，明确「尚未绑定」）。
    let error = read_resource(&peer, "workflow://runs")
        .await
        .expect_err("未绑定时资源读取必须失败");
    assert!(error.contains("not bound"), "{error}");

    pair.shutdown().await;
}

/// F2（对抗审查）：生产存在「先订阅、后绑定」窗口——`activate()` 之后池初始化即建立
/// `subscriptions/listen`（`peri-middlewares/src/mcp/initialize.rs` 的
/// `setup_subscription`），而 builtin `workflow` 实例的运行时槽位要到会话发布前才绑定
/// （`session_lifecycle.rs` 的 bind 点）。SDK 在 handler 执行**之前**就发出
/// acknowledge，因此客户端此时看到的订阅「连接正常」；订阅若在绑定前放弃注册唤醒，
/// 之后绑定也不会重建 receiver ⇒ 通知永远不来（静默失效）。
///
/// 契约：绑定前的订阅必须保持可唤醒，绑定后发布的任务更新必须到达。
#[tokio::test]
async fn subscription_listening_before_bind_is_woken_after_binding() {
    let slot = WorkflowInstanceInput::slot();
    let pair = connect(WorkflowMcpServer::new(slot.clone())).await;
    let peer = pair.peer();

    // ① 未绑定即 listen（生产时序：池初始化先于会话就绪）。
    let mut subscription = peer
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions([crate::resources::TASKS_URI])
                .build(),
        )
        .await
        .expect("绑定前必须能建立订阅（acknowledge 先于 handler）");

    // ② 会话就绪：绑定运行时（生产 bind 点）。
    let runtime = make_runtime();
    assert_eq!(
        slot.bind(Arc::clone(&runtime)),
        BindOutcome::Bound,
        "空槽首次绑定必须成功"
    );

    // ③ 绑定后发布任务：先可见后通知。绑定等待存在固有时序（通知只承担唤醒信号，
    //    不承担可靠投递），因此循环发布 + 等待，直到收到该 URI 的 updated 通知。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seq = 0u64;
    let uri = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "绑定后订阅仍未收到唤醒信号：订阅在未绑定窗口静默失效（F2）"
        );
        let outcome = runtime
            .tasks()
            .publish(format!("task-{seq}"), task_params("run-1"));
        assert!(
            matches!(outcome, PublishOutcome::Published(_)),
            "任务发布必须提交"
        );
        seq += 1;
        match tokio::time::timeout(Duration::from_millis(100), subscription.next()).await {
            Ok(Ok(Some(ServerNotification::ResourceUpdatedNotification(update)))) => {
                break update.params.uri;
            }
            Ok(Ok(Some(other))) => panic!("收到与订阅无关的通知：{other:?}"),
            Ok(Err(error)) => panic!("订阅流读取失败：{error}"),
            Ok(Ok(None)) => panic!("订阅流提前结束（end = {:?}）", subscription.end()),
            // 本轮未收到：继续发布，覆盖「发布早于 receiver 注册」的竞态窗口。
            Err(_) => {}
        }
    };
    assert_eq!(
        uri,
        crate::resources::TASKS_URI,
        "唤醒 URI 必须是订阅的规范 URI"
    );

    let _ = subscription.cancel().await;
    pair.shutdown().await;
}
