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
    t.on_llm_end(0, "claude-4.7", "anthropic", "hello", None);
    let reason_handle = t.stages.on_stage_start(
        Stage::Reason,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end(&reason_handle, StageStatus::Done);

    // Stage: End
    t.on_stage_start(Stage::End, "turn_1");
    let end_handle =
        t.stages
            .on_stage_start(Stage::End, &t.trace_id, "turn_1", &t.agent_observation_id);
    t.on_stage_end(&end_handle, StageStatus::Done);

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
    t.on_llm_end(0, "m", "p", "o", None);
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
    t.on_llm_end(0, "gpt-4", "openai", "response", None);
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
    t.on_llm_end(0, "gpt-4", "openai", "response", None);
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

/// 验证 subagent 活跃时，工具写入 subagent 的 ToolBatch 而非主 agent 的。
#[test]
fn test_subagent_tool_routed_to_subagent_tool_batch() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_sub_route");

    // 1. Agent 工具启动：压入 subagent 栈，Agent 工具写入 subagent 的 tool_batch
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "explore"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 2. 主 agent 的 tool_batch 应为空（Agent 工具已路由到 subagent 的）
    let main_flush = t.tool_batch.flush();
    assert!(
        main_flush.tools.is_empty(),
        "主 agent 的 tool_batch 应为空，Agent 工具已路由到 subagent 的 tool_batch"
    );

    // 3. subagent 内的普通工具：应写入 subagent 的 tool_batch
    t.on_tool_start("tc_read", "Read", &serde_json::json!({"path": "test.txt"}));
    t.on_tool_end("tc_read", "file content", false);

    // 4. 主 agent 的 tool_batch 仍应为空
    let main_flush2 = t.tool_batch.flush();
    assert!(
        main_flush2.tools.is_empty(),
        "subagent 活跃时，普通工具不应写入主 agent 的 tool_batch"
    );
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
#[test]
fn test_fork_subagent_flushes_tool_batch_before_pop() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_fork_flush");

    // 1. Agent 工具启动 → 压栈 + 写入 subagent 的 tool_batch
    t.on_tool_start(
        "tc_agent",
        "Agent",
        &serde_json::json!({"subagent_name": "fork"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 2. 模拟 subagent 已启动
    t.subagent.mark_top_started();
    assert!(t.subagent.top_has_started());

    // 3. Agent 工具结束：flush → 弹栈
    t.on_tool_end("tc_agent", "fork completed", false);
    assert_eq!(t.subagent.depth(), 0, "fork 场景应弹栈");

    // 4. 弹栈后主 tool_batch 仍为空（工具在 subagent 的 tool_batch 中被 flush 掉了）
    let main_flush = t.tool_batch.flush();
    assert!(
        main_flush.tools.is_empty(),
        "fork 后主 tool_batch 不应有 subagent 的工具"
    );
}

/// 验证 bg subagent：turn_end 时所有 subagent tool_batch 被 flush。
#[test]
fn test_bg_subagent_tool_batch_flushed_on_turn_end() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_bg_flush");

    // 1. 创建 bg subagent（Agent 工具写入 subagent 的 tool_batch）
    t.on_tool_start(
        "tc_bg",
        "Agent",
        &serde_json::json!({"subagent_name": "bg"}),
    );
    assert_eq!(t.subagent.depth(), 1);

    // 2. bg subagent 内工具
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
    assert_eq!(total_tools, 2, "bg subagent 的 tool_batch 应包含 2 个工具");
}
