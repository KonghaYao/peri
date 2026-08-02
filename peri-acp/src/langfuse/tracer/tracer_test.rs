//! LangfuseTracer 集成烟雾测试。
//!
//! 覆盖完整的 turn 生命周期、采样率控制、ErrorSpan 机制、
//! text chunk 累积和 LLM generation 事件流。
//!
//! 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
//! 提供异步运行时。

use super::*;
use peri_agent::agent::events::{Stage, StageStatus};

fn make_tracer(
    rate: f64,
) -> (
    LangfuseTracer,
    std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
) {
    // FakeLangfuseSession::new() 已返回 Arc<Self>，无需再包一层
    let session = crate::langfuse::fake_session::FakeLangfuseSession::new("sess_smoke");
    let config = crate::langfuse::config::LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: rate,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
    };
    let t = LangfuseTracer::new(session.clone(), "sess_smoke".to_string(), config);
    (t, session)
}

// ── 烟雾测试：完整 turn 序列 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_smoke_complete_turn_sequence() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");

    // Stage: Receive
    t.on_stage_start(Stage::Receive, "turn_1");
    let recv_handle = t.stages.on_stage_start(
        Stage::Receive,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end(&recv_handle, StageStatus::Done);

    // Stage: Reason + LLM
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "claude-4.7", "anthropic", "hello", None, None);
    let reason_handle = t.stages.on_stage_start(
        Stage::Reason,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end(&reason_handle, StageStatus::Done);

    let _handle = t.on_turn_end(None);
    // 等待 flush async 任务完成（FakeLangfuseSession 的 flush 是同步的，但 spawn 需要运行）
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "应有至少一个事件");
}

// ── 采样率测试 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_sampling_rate_0_emits_nothing() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "m", "p", "o", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(
        events.is_empty(),
        "采样率 0 应不上报任何事件，实际有 {} 个",
        events.len()
    );
}

#[tokio::test]
async fn test_sampling_rate_1_emits_events() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "采样率 1.0 应有事件");
}

// ── ErrorSpan 测试 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_error_span_emitted_for_error_turn() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    let _handle = t.on_turn_end(Some("TurnError"));
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_trace = events
        .iter()
        .any(|e| matches!(e, langfuse_client::IngestionEvent::TraceCreate { .. }));
    let has_error_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("ErrorTurn")
        } else {
            false
        }
    });
    assert!(has_trace, "错误 turn 应补发 TraceCreate");
    assert!(has_error_span, "错误 turn 应发 ErrorSpan");
}

// ── TextChunk 累积测试 ─────────────────────────────────────────────────────

#[test]
fn test_on_text_chunk_accumulates() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_text_chunk("Hello ");
    t.on_text_chunk("World");
    assert_eq!(t.final_answer, "Hello World");
}

// ── LLM Generation 事件测试 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_llm_generation_emits_events() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "gpt-4", "openai", "response", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen_count = events
        .iter()
        .filter(|e| matches!(e, langfuse_client::IngestionEvent::GenerationCreate { .. }))
        .count();
    assert!(gen_count > 0, "应有至少一个 GenerationCreate 事件");
}

#[tokio::test]
async fn test_llm_retry_accumulates_metadata() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_retrying(1, 3, 500, "timeout");
    t.on_llm_retrying(2, 3, 1000, "timeout");
    t.on_llm_end(0, "gpt-4", "openai", "response", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    // 验证 GenerationCreate 包含重试 metadata（字段名为 retry_count）
    let has_retry_meta = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
            body.metadata
                .as_ref()
                .map(|m| m.get("retry_count").is_some())
                .unwrap_or(false)
        } else {
            false
        }
    });
    assert!(
        has_retry_meta,
        "GenerationCreate 应包含重试 metadata (retry_count)"
    );
}

// ── Middleware 事件测试 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_middleware_start_and_end() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_middleware_start(
        "auth",
        peri_agent::agent::events::MiddlewareHook::BeforeAgent,
    );
    let mw_handle = t.middleware.on_start(
        "auth",
        peri_agent::agent::events::MiddlewareHook::BeforeAgent,
    );
    // 微小延迟确保 duration > 0（MiddlewareSpan 条件上报）
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    t.on_middleware_end(&mw_handle, StageStatus::Done, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_mw_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("mw-auth")
        } else {
            false
        }
    });
    assert!(has_mw_span, "应有 mw-auth SpanCreate 事件");
}

// ── Compact 事件测试 ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compact_lifecycle() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_compact_start(
        peri_agent::agent::events::CompactStrategy::Micro,
        peri_agent::agent::events::CompactTrigger::Auto,
    );
    // 微小延迟确保 duration > 0（Compact 条件上报）
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    t.on_compact_end("summary text", 3, 2, 5, false, "");
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_compact_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(has_compact_span, "应有 compact SpanCreate 事件");

    // v2 条件上报：compact 改为延迟创建，不再发 SpanUpdate
    let has_compact_update = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanUpdate { body, .. } = e {
            body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(!has_compact_update, "v2 条件上报不应发 compact SpanUpdate");
}

/// 回归测试：当 `stages.active_handle()` 返回 None 但 subagent stack 非空时，
/// `on_tool_start` 的 parent_id 应 fallback 到 subagent 的 observation_id，
/// 而非主 agent 的 agent_observation_id。
///
/// BUG 1 修复：这是 belts-and-suspenders 安全网，应对 biased select 重排
/// 外仍可能出现的时序问题。配合 forwarder 重排后，正常流程中此 fallback 不应触发。
///
/// BUG 3 注意：subagent 活跃时工具路由到 subagent 的 tool_batch。
#[test]
fn test_on_tool_start_fallback_to_subagent_when_stage_not_started() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_fallback_test");

    // 手动压入 subagent 上下文（模拟 SubAgent 已启动但 StageStarted 尚未到达）
    t.begin_subagent(&serde_json::json!({"agent": "explore", "description": "test"}));

    // 确认 subagent 栈非空
    assert_eq!(t.subagent.depth(), 1, "subagent 栈应有 1 层");

    // 获取预期的 fallback parent_id（subagent 的 observation_id）
    let expected_parent = t.subagent.current_agent_id(&t.agent_observation_id);
    assert_ne!(
        expected_parent, t.agent_observation_id,
        "subagent observation_id 应不同于主 agent observation_id"
    );

    // 在没有 stage 的情况下调用 on_tool_start（active_handle() 返回 None）
    // parent_id 应 fallback 到 subagent 的 observation_id
    t.on_tool_start(
        "tc_fallback",
        "Read",
        &serde_json::json!({"path": "test.txt"}),
    );
    t.on_tool_end("tc_fallback", "file content", false);

    // BUG 3: 工具已路由到 subagent 的 tool_batch，需从中 flush
    let flushes = t.subagent.flush_all_subagent_tool_batches();
    assert_eq!(flushes.len(), 1, "应有 1 个 subagent tool_batch flush");
    let flush = &flushes[0];
    assert_eq!(
        flush.parent_observation_id, expected_parent,
        "parent_observation_id 应 fallback 到 subagent 的 observation_id，而非主 agent"
    );
    assert_ne!(
        flush.parent_observation_id, t.agent_observation_id,
        "parent_observation_id 不应回落到主 agent 的 agent_observation_id"
    );
}

// ── BUG 2: bg subagent 栈时序测试 ─────────────────────────────────────────

/// 模拟 bg subagent 场景：on_tool_end 在 subagent 启动前到达，
/// 此时 has_started=false，不应弹栈。
#[test]
fn test_bg_subagent_deferred_pop_preserves_stack() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_bg_test");

    // 模拟 Agent 工具调用开始（压入 subagent 栈，has_started=false）
    t.on_tool_start(
        "tc_bg",
        "Agent",
        &serde_json::json!({"subagent_name": "bg_agent"}),
    );
    assert_eq!(t.subagent.depth(), 1, "Agent 工具应压入 subagent 栈");

    // 确认 has_started 为 false（尚未收到 StageStarted）
    assert!(
        !t.subagent.top_has_started(),
        "尚未收到 subagent 事件时 has_started 应为 false"
    );

    // Agent 工具结束：因为 has_started=false（bg 场景），不应弹栈
    t.on_tool_end("tc_bg", "bg agent spawned, will run later", false);
    assert_eq!(
        t.subagent.depth(),
        1,
        "bg subagent：on_tool_end 时 has_started=false，不应弹栈"
    );
}

/// 模拟 fork subagent 场景：on_tool_end 在 subagent 事件之后到达，
/// 此时 has_started=true，应正常弹栈。
#[test]
fn test_fork_subagent_pops_on_tool_end() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_fork_test");

    // 模拟 fork Agent 工具调用开始
    t.on_tool_start(
        "tc_fork",
        "Agent",
        &serde_json::json!({"subagent_name": "fork_agent"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 模拟 subagent 已启动（StageStarted 到达 → mark_top_started）
    t.subagent.mark_top_started();
    assert!(
        t.subagent.top_has_started(),
        "mark_top_started 后 has_started 应为 true"
    );

    // Agent 工具结束：has_started=true，应正常弹栈
    t.on_tool_end("tc_fork", "fork agent completed", false);
    assert_eq!(
        t.subagent.depth(),
        0,
        "fork subagent：on_tool_end 时 has_started=true，应弹栈"
    );
}

/// 模拟 ActiveHandle 调用链：bg subagent 的 StageStarted 到达后
/// 应标记 has_started=true，恢复子 agent 活跃状态。
#[test]
fn test_bg_subagent_stage_started_marks_started() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_bg_stage");

    // 1. 创建 bg subagent（压栈，has_started=false）
    t.on_tool_start(
        "tc_bg",
        "Agent",
        &serde_json::json!({"subagent_name": "bg_agent"}),
    );
    assert_eq!(t.subagent.depth(), 1);
    assert!(!t.subagent.top_has_started());

    // 2. bg subagent 的 on_tool_end 到达（不弹栈）
    t.on_tool_end("tc_bg", "spawned", false);
    assert_eq!(t.subagent.depth(), 1, "bg 场景不应弹栈");

    // 3. bg subagent 的 StageStarted 到达 → mark_started
    t.on_stage_start(Stage::Act, "turn_bg_stage");
    assert!(
        t.subagent.top_has_started(),
        "StageStarted 后 has_started 应为 true"
    );

    // 4. 现在栈顶的 has_started=true，如果再有 agent tool end 会正常弹栈
    assert_eq!(t.subagent.depth(), 1);
}

// ── BUG 3: subagent 工具路由到正确的 ToolBatch ──────────────────────────

/// 验证 subagent 活跃时，Agent 工具写入主 ToolBatch（Fix A），普通工具写入 subagent 的 ToolBatch。
#[test]
fn test_subagent_tool_routed_to_subagent_tool_batch() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_sub_route");

    // 1. Agent 工具启动：先写入主 batch，再压入 subagent 栈
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "explore"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 1b. 结束 Agent 工具：Fix B 路由到主 batch，top_has_started=false → bg 路径不弹栈
    t.on_tool_end("tc_agent", "subagent dispatched", false);
    assert_eq!(
        t.subagent.depth(),
        1,
        "bg subagent 不应在工具结束时弹栈（未启动）"
    );

    // 2. Agent 工具应写入主 agent 的 tool_batch
    let main_flush = t.tool_batch.flush();
    assert_eq!(
        main_flush.tools.len(),
        1,
        "Agent 工具应在主 agent 的 tool_batch"
    );
    assert_eq!(main_flush.tools[0].name, "Agent");

    // 3. subagent 内的普通工具：应写入 subagent 的 tool_batch（栈非空时路由到 Sub）
    t.on_tool_start("tc_read", "Read", &serde_json::json!({"path": "test.txt"}));
    t.on_tool_end("tc_read", "file content", false);

    // 4. 主 agent 的 tool_batch 只有 Agent 工具（已 flush），不应有 subagent 工具
    //    验证后 subagent 仍活跃（未 flush sub batch）
    let main_flush2 = t.tool_batch.flush();
    assert!(
        main_flush2.tools.is_empty(),
        "subagent 活跃时，普通工具不应写入主 agent 的 tool_batch"
    );
    assert_eq!(t.subagent.depth(), 1, "subagent 栈应仍活跃");
}

/// 验证栈空时，工具仍写入主 ToolBatch（向后兼容）。
#[test]
fn test_main_agent_tool_not_routed_to_subagent_batch() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_main_only");

    // 栈空时，工具应写入主 agent 的 tool_batch
    t.on_tool_start("tc_read", "Read", &serde_json::json!({"path": "test.txt"}));
    t.on_tool_end("tc_read", "file content", false);

    // 主 agent 的 tool_batch 应有该工具
    let main_flush = t.tool_batch.flush();
    assert_eq!(
        main_flush.tools.len(),
        1,
        "栈空时，工具应写入主 agent 的 tool_batch"
    );
    assert_eq!(main_flush.tools[0].name, "Read");
}

/// 验证 fork subagent：on_tool_end 时 flush subagent tool_batch 后再弹栈。
/// Agent 工具现在写入主 batch（Fix A），subagent 工具在 sub batch 中被 flush。
#[test]
fn test_fork_subagent_flushes_tool_batch_before_pop() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_fork_flush");

    // 1. Agent 工具启动 → 写入主 batch + 压栈
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "fork"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 2. 模拟 subagent 已启动
    t.subagent.mark_top_started();
    assert!(t.subagent.top_has_started());

    // 3. Agent 工具结束：flush sub batch → 弹栈
    t.on_tool_end("tc_agent", "fork completed", false);
    assert_eq!(t.subagent.depth(), 0, "fork 场景应弹栈");

    // 4. 弹栈后主 tool_batch 包含 Agent 工具（已完成，尚未 flush）
    let main_flush = t.tool_batch.flush();
    assert_eq!(main_flush.tools.len(), 1, "主 tool_batch 应含 Agent 工具");
    assert_eq!(main_flush.tools[0].name, "Agent");
}

/// 验证 bg subagent：turn_end 时所有 subagent tool_batch 被 flush。
/// Agent 工具现在在主 batch（Fix A），subagent 工具在 sub batch。
#[test]
fn test_bg_subagent_tool_batch_flushed_on_turn_end() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_bg_flush");

    // 1. 创建 bg subagent（Agent 工具写入主 batch）
    t.on_tool_start(
        "tc_bg",
        "Agent",
        &serde_json::json!({"subagent_name": "bg"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 2. bg subagent 内工具（写入 sub batch）
    t.on_tool_start("tc_bash", "Bash", &serde_json::json!({"cmd": "ls"}));
    t.on_tool_end("tc_bash", "file list", false);

    // 3. bg subagent 未启动，Agent 工具结束时不应弹栈
    assert!(!t.subagent.top_has_started());
    t.on_tool_end("tc_bg", "spawned", false);
    assert_eq!(t.subagent.depth(), 1, "bg 场景不应弹栈");

    // 4. turn_end：flush_all_subagent_tool_batches 应工作
    // 手动测试 flush_all 方法
    let flushes = t.subagent.flush_all_subagent_tool_batches();
    let total_tools: usize = flushes.iter().map(|f| f.tools.len()).sum();
    assert_eq!(
        total_tools, 1,
        "subagent tool_batch 只应包含 Bash 工具（Agent 在主 batch）"
    );
    assert_eq!(flushes[0].tools[0].name, "Bash");

    // 5. 验证 Agent 工具在主 tool_batch 中
    let main_flush = t.tool_batch.flush();
    assert_eq!(main_flush.tools.len(), 1);
    assert_eq!(main_flush.tools[0].name, "Agent");
}

/// Fix A+B+C regression: subagent tools flush with subagent act span as parent.
/// 本测试直接调用 `t.stages.on_stage_start` + `t.subagent.mark_top_started()`
/// 来模拟子 agent stage 创建流程（与生产路径 `t.on_stage_start` 等效）。
#[test]
fn test_subagent_tool_batch_parent_is_subagent_act_span() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_parent_fix");

    // Start main agent's Act stage (for active_handle to return main span)
    let main_act = t.stages.on_stage_start(
        Stage::Act,
        &t.trace_id,
        "turn_parent_fix",
        &t.agent_observation_id,
    );
    let main_act_span_id = main_act.span_id.clone();

    // Agent tool start → main batch + begin_subagent
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "test"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // Simulate subagent Act stage start (changes active_handle to subagent's span)
    let sub_act = t.stages.on_stage_start(
        Stage::Act,
        &t.trace_id,
        "turn_parent_fix",
        &t.agent_observation_id,
    );
    let sub_act_span_id = sub_act.span_id.clone();
    t.subagent.mark_top_started();

    // Subagent tool → routes to sub batch with sub act span as parent
    t.on_tool_start("tc_grep", "Grep", &serde_json::json!({"pattern": "test"}));
    t.on_tool_end("tc_grep", "found matches", false);

    // Flush subagent batch and verify parent
    let sub_flushes = t.subagent.flush_all_subagent_tool_batches();
    assert_eq!(sub_flushes.len(), 1);
    let sub_flush = &sub_flushes[0];
    assert_eq!(
        sub_flush.tools.len(),
        1,
        "subagent flush should have 1 tool (Grep)"
    );
    assert_eq!(sub_flush.tools[0].name, "Grep");
    // Key assertion: parent is subagent's act span, not main's
    assert_eq!(
        sub_flush.parent_observation_id, sub_act_span_id,
        "subagent tool batch parent should be subagent's act span, not main's"
    );
    assert_ne!(
        sub_flush.parent_observation_id, main_act_span_id,
        "subagent tool batch parent should NOT be main agent's act span"
    );

    // End Agent tool (moves to completed_tools for flush visibility)
    // top_has_started() is true → fork path: flushes sub batch (already flushed, no-op) + pops stack
    t.on_tool_end("tc_agent", "subagent done", false);

    // Main batch contains Agent tool
    let main_flush = t.tool_batch.flush();
    assert_eq!(main_flush.tools.len(), 1);
    assert_eq!(main_flush.tools[0].name, "Agent");
    assert_eq!(
        main_flush.parent_observation_id, main_act_span_id,
        "main tool batch parent should be main agent's act span"
    );
}

/// Fix C reverted: on_stage_end(Act) 只 flush 主 batch，不 flush 子 batch。
/// 子 batch 的 flush 由 on_tool_end("Agent") fork 路径负责（top_has_started 守卫）。
#[test]
fn test_on_stage_end_act_does_not_flush_subagent_batch() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_stage_flush");

    let _main_act = t.stages.on_stage_start(
        Stage::Act,
        &t.trace_id,
        "turn_stage_flush",
        &t.agent_observation_id,
    );

    // Agent → push subagent
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "test"}),
    );

    // Mock subagent Act stage (mark_started is done via on_stage_start in real flow)
    let sub_act = t.stages.on_stage_start(
        Stage::Act,
        &t.trace_id,
        "turn_stage_flush",
        &t.agent_observation_id,
    );
    t.subagent.mark_top_started();

    // Subagent tool
    t.on_tool_start("tc_bash", "Bash", &serde_json::json!({"cmd": "ls"}));
    t.on_tool_end("tc_bash", "ok", false);

    // Act stage end: 只 flush 主 batch（但有活跃子 agent，跳过）
    t.on_stage_end(&sub_act, StageStatus::Done);

    // 子 batch 应仍包含 tools（Bash 在子 batch，未被 on_stage_end 影响）
    let sub_flushes = t.subagent.flush_all_subagent_tool_batches();
    let total_tools: usize = sub_flushes.iter().map(|f| f.tools.len()).sum();
    assert_eq!(total_tools, 1);
    assert_eq!(sub_flushes[0].tools[0].name, "Bash");

    // End Agent tool（Fix B 路由到主 batch）
    t.on_tool_end("tc_agent", "subagent done", false);

    // 子 batch 已空（Agent tool 的 fork 路径 flush 了子 batch）
    let sub_flushes2 = t.subagent.flush_all_subagent_tool_batches();
    assert_eq!(sub_flushes2.iter().map(|f| f.tools.len()).sum::<usize>(), 0);

    // Main batch — Agent tool 应该在主 batch 中（Fix A 写入主 batch，
    // Fix B on_tool_end 路由到主 batch，on_stage_end 因 subagent.is_empty()=false 未 flush）
    let main_flush = t.tool_batch.flush();
    assert_eq!(main_flush.tools.len(), 1);
    assert_eq!(main_flush.tools[0].name, "Agent");
    // batch span 应在（主 batch 未被 on_stage_end 提前 flush）
    assert!(
        main_flush.batch.is_some(),
        "batch span should exist — stage end skipped flush"
    );
}

/// 回归测试：Receive stage span 的 input 应包含 mq_counts 排空数据。
/// bug: stages.on_stage_end() 在 span body 构造前清空 active → mq_counts 丢失，
/// 导致 Receive span 的 input 始终为 None。
/// 修复：在 stages.on_stage_end() 之前捕获 mq_counts，填入 span body 的 input 字段。
#[test]
fn test_receive_stage_span_includes_mq_counts_in_input() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_mq");

    // 1. 开始 Receive 阶段 → mq_counts 初始化为 (0,0,0)
    t.on_stage_start(Stage::Receive, "turn_mq");

    // 2. 模拟 MQ 排空：1 条 prompt + 2 条 defer + 0 条 info = 共 3 条
    t.on_mq_drained(1, 2, 0);

    // 3. 确保 stage 持续 ≥ 2ms，避免 duration_ms==0 导致 span 被条件跳过
    std::thread::sleep(std::time::Duration::from_millis(2));

    // 4. 获取 active handle（on_stage_start 返回的是忽略的，从 stages 拿实际 handle）
    let handle = t
        .stages
        .active_handle()
        .expect("Receive stage 应为 active")
        .clone();

    // 5. 结束 Receive 阶段
    t.on_stage_end(&handle, StageStatus::Done);

    // 6. 验证：发出的 SpanCreate 事件中 input 应包含 mq_counts
    let events = session.events_snapshot();
    let span_creates: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-receive") {
                    Some(body)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        span_creates.len(),
        1,
        "应有恰好 1 个 stage-receive SpanCreate 事件"
    );

    let span = &span_creates[0];
    let input = span
        .input
        .as_ref()
        .expect("Receive span 的 input 不应为 None");
    let mq = input
        .get("messages_drained")
        .expect("input 应包含 messages_drained 字段");

    assert_eq!(mq["prompt"], serde_json::json!(1), "prompt 计数应为 1");
    assert_eq!(mq["defer"], serde_json::json!(2), "defer 计数应为 2");
    assert_eq!(mq["info"], serde_json::json!(0), "info 计数应为 0");
    assert_eq!(mq["total"], serde_json::json!(3), "total 应为 1+2+0 = 3");
}

/// 非 Receive 阶段的 span input 应保持 None，不应误填充。
#[test]
fn test_non_receive_stage_span_input_is_none() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_reason");

    // Reason 阶段
    t.on_stage_start(Stage::Reason, "turn_reason");
    // 确保 stage 持续 ≥ 2ms，避免 duration_ms==0 导致 span 被条件跳过
    std::thread::sleep(std::time::Duration::from_millis(2));
    let handle = t
        .stages
        .active_handle()
        .expect("Reason stage 应为 active")
        .clone();
    t.on_stage_end(&handle, StageStatus::Done);

    let events = session.events_snapshot();
    let reason_spans: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-reason") {
                    Some(body)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        reason_spans.len(),
        1,
        "应有恰好 1 个 stage-reason SpanCreate"
    );
    assert!(
        reason_spans[0].input.is_none(),
        "非 Receive 阶段的 input 应为 None，不应误填入 mq_counts"
    );
}
