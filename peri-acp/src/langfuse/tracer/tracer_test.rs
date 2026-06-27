use std::{sync::Arc, time::Duration};

use langfuse_client::{BackpressurePolicy, Batcher, BatcherConfig, LangfuseClient};

use super::*;

fn make_tracer() -> LangfuseTracer {
    let client = LangfuseClient::new("pk-test", "sk-test", "http://127.0.0.1:1", 0);
    let config = BatcherConfig {
        max_events: 1000,
        flush_interval: Duration::from_secs(600),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Arc::new(Batcher::new(client, config));
    let session = Arc::new(LangfuseSession {
        client: Arc::new(LangfuseClient::new("pk", "sk", "http://127.0.0.1:1", 0)),
        batcher,
    });
    LangfuseTracer::new(session, "test-session".to_string())
}

fn agent_tool_input(subagent_type: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "subagent_type": subagent_type,
        "prompt": prompt,
        "description": "test task"
    })
}

// === Test 1: Agent 工具 on_tool_start 压栈 ===
#[tokio::test]
async fn test_agent_tool_start_pushes_subagent_stack() {
    let mut tracer = make_tracer();
    let main_agent_id = tracer.agent_observation_id.clone();

    assert!(tracer.subagent_stack.is_empty());
    assert_eq!(tracer.current_agent_id(), main_agent_id);

    let input = agent_tool_input("code-reviewer", "review this code");
    tracer.on_tool_start("tc-1", "Agent", &input);

    assert_eq!(tracer.subagent_stack.len(), 1);
    let subagent_obs_id = tracer.subagent_stack[0].observation_id.clone();
    assert_ne!(subagent_obs_id, main_agent_id);
    assert_eq!(tracer.current_agent_id(), subagent_obs_id);
    assert_eq!(tracer.subagent_stack[0].agent_id, "code-reviewer");
}

// === Test 2: 非 Agent 工具不影响栈 ===
#[tokio::test]
async fn test_non_agent_tool_does_not_push_subagent_stack() {
    let mut tracer = make_tracer();
    let input = serde_json::json!({"file_path": "/tmp/test.rs"});
    tracer.on_tool_start("tc-1", "Read", &input);
    assert!(tracer.subagent_stack.is_empty());
}

// === Test 3: Agent 工具 on_tool_end 弹出栈 ===
#[tokio::test]
async fn test_agent_tool_end_pops_subagent_stack() {
    let mut tracer = make_tracer();
    tracer.on_tool_start("tc-1", "Agent", &agent_tool_input("explorer", "find files"));
    assert_eq!(tracer.subagent_stack.len(), 1);
    tracer.on_tool_end("tc-1", "found 3 files", false);
    assert!(tracer.subagent_stack.is_empty());
}

// === Test 4: 非 Agent 工具 on_tool_end 不弹栈 ===
#[tokio::test]
async fn test_non_agent_tool_end_does_not_pop_subagent_stack() {
    let mut tracer = make_tracer();
    tracer.on_tool_start("tc-agent", "Agent", &agent_tool_input("plan", "plan this"));
    assert_eq!(tracer.subagent_stack.len(), 1);

    let input = serde_json::json!({"pattern": "*.rs"});
    tracer.on_tool_start("tc-glob", "Glob", &input);
    tracer.on_tool_end("tc-glob", "file1.rs", false);

    assert_eq!(tracer.subagent_stack.len(), 1);

    tracer.on_tool_end("tc-agent", "plan done", false);
    assert!(tracer.subagent_stack.is_empty());
}

// === Test 5: SubAgent 生命周期中内部事件路由正确 ===
#[tokio::test]
async fn test_subagent_internal_events_use_subagent_context() {
    let mut tracer = make_tracer();
    let main_agent_id = tracer.agent_observation_id.clone();

    tracer.on_tool_start(
        "tc-1",
        "Agent",
        &agent_tool_input("code-reviewer", "review"),
    );
    let subagent_obs_id = tracer.current_agent_id();
    assert_ne!(subagent_obs_id, main_agent_id);

    // SubAgent 内部 LLM 调用：parent 应为 subagent obs
    tracer.on_llm_start(0, &[], &[]);
    assert_eq!(tracer.current_agent_id(), subagent_obs_id);

    // SubAgent 内部工具调用：使用 subagent 的 tools context
    tracer.on_tool_start(
        "tc-inner",
        "Read",
        &serde_json::json!({"file_path": "x.rs"}),
    );
    assert_eq!(tracer.subagent_stack[0].pending_tools.len(), 1);

    tracer.on_tool_end("tc-inner", "content", false);
    assert!(tracer.subagent_stack[0].tools_batch_end_time.is_some());

    tracer.on_tool_end("tc-1", "review done", false);
    assert!(tracer.subagent_stack.is_empty());
    assert_eq!(tracer.current_agent_id(), main_agent_id);
}

// === Test 6: 嵌套 SubAgent ===
#[tokio::test]
async fn test_nested_subagent_stack_depth() {
    let mut tracer = make_tracer();

    tracer.on_tool_start("tc-a", "Agent", &agent_tool_input("planner", "plan"));
    assert_eq!(tracer.subagent_stack.len(), 1);
    let planner_obs_id = tracer.current_agent_id();

    tracer.on_tool_start("tc-b", "Agent", &agent_tool_input("explorer", "find"));
    assert_eq!(tracer.subagent_stack.len(), 2);
    let explorer_obs_id = tracer.current_agent_id();
    assert_ne!(explorer_obs_id, planner_obs_id);

    tracer.on_llm_start(0, &[], &[]);
    assert_eq!(tracer.current_agent_id(), explorer_obs_id);

    tracer.on_tool_end("tc-b", "found files", false);
    assert_eq!(tracer.subagent_stack.len(), 1);
    assert_eq!(tracer.current_agent_id(), planner_obs_id);

    tracer.on_tool_end("tc-a", "plan done", false);
    assert!(tracer.subagent_stack.is_empty());
}

// === Test 7: 未知 tool_call_id 的 on_tool_end 不 panic ===
#[tokio::test]
async fn test_on_tool_end_unknown_tool_call_id_no_panic() {
    let mut tracer = make_tracer();
    tracer.on_tool_end("nonexistent", "output", false);
    // Should return early without panicking
}

// === Test 8: Fork 类型识别 ===
#[test]
fn test_fork_subagent_identity() {
    assert_eq!(
        LangfuseTracer::subagent_identity(
            &serde_json::json!({"prompt": "do something", "fork": true})
        ),
        "fork"
    );
    assert_eq!(
        LangfuseTracer::subagent_identity(
            &serde_json::json!({"subagent_type": "code-reviewer", "fork": true, "prompt": "x"})
        ),
        "code-reviewer"
    );
    assert_eq!(
        LangfuseTracer::subagent_identity(&serde_json::json!({"prompt": "x"})),
        "fork"
    );
}

// === Test 9: 并发 SubAgent 独立上下文 ===
#[tokio::test]
async fn test_concurrent_subagents_independent_context() {
    let mut tracer = make_tracer();
    let main_id = tracer.agent_observation_id.clone();

    // 启动第一个 subagent
    tracer.on_tool_start("tc-1", "Agent", &agent_tool_input("explorer", "find files"));
    assert_eq!(tracer.subagent_stack.len(), 1);
    let sub1_id = tracer.current_agent_id();

    // 启动第二个 subagent（嵌套场景，flat model 下是兄弟关系）
    tracer.on_tool_start(
        "tc-2",
        "Agent",
        &agent_tool_input("code-reviewer", "review"),
    );
    assert_eq!(tracer.subagent_stack.len(), 2);
    let sub2_id = tracer.current_agent_id();

    // 两个 subagent 彼此不同，也不同于 main agent
    assert_ne!(sub1_id, main_id);
    assert_ne!(sub2_id, main_id);
    assert_ne!(sub1_id, sub2_id);

    // 第二个 subagent 结束后回到第一个
    tracer.on_tool_end("tc-2", "review done", false);
    assert_eq!(tracer.subagent_stack.len(), 1);
    assert_eq!(tracer.current_agent_id(), sub1_id);

    // 第一个 subagent 结束后回到 main agent
    tracer.on_tool_end("tc-1", "found 5 files", false);
    assert!(tracer.subagent_stack.is_empty());
    assert_eq!(tracer.current_agent_id(), main_id);
}

// === Test 10: Compact 生命周期——full compact 正常完成 ===
#[tokio::test]
async fn test_compact_full_lifecycle() {
    let mut tracer = make_tracer();

    // Compact 开始前 compact_span 为空
    assert!(tracer.compact_span.is_none());

    tracer.on_compact_start();
    assert!(tracer.compact_span.is_some());

    tracer.on_compact_end("这是摘要内容", 3, 2, 0, false, "");
    // 完成后 compact_span 被清空
    assert!(tracer.compact_span.is_none());
}

// === Test 11: Compact 生命周期——错误路径 ===
#[tokio::test]
async fn test_compact_error_lifecycle() {
    let mut tracer = make_tracer();

    tracer.on_compact_start();
    assert!(tracer.compact_span.is_some());

    tracer.on_compact_end("", 0, 0, 0, true, "LLM 调用超时");
    assert!(tracer.compact_span.is_none());
}

// === Test 12: Compact 生命周期——micro compact ===
#[tokio::test]
async fn test_compact_micro_lifecycle() {
    let mut tracer = make_tracer();

    // micro compact 不使用 on_compact_start（由 CompactCompleted 直接携带 micro_cleared）
    // 但 on_compact_end 仍能正确识别
    tracer.on_compact_end("", 0, 0, 8, false, "");
    assert!(tracer.compact_span.is_none());
}

// === Test 13: LlmRetrying 累积 → generation metadata ===
#[tokio::test]
async fn test_llm_retry_accumulates_to_metadata() {
    let mut tracer = make_tracer();

    // 第一轮 LLM 调用，无重试
    tracer.on_llm_start(0, &[], &[]);
    assert!(tracer.retry_attempts.is_empty());
    tracer.on_llm_end(0, "gpt-4o", "OpenAI", "result", None);
    assert!(tracer.retry_attempts.is_empty());
    assert!(tracer.active_step.is_none());

    // 第二轮 LLM 调用，有 2 次重试
    tracer.on_llm_start(1, &[], &[]);
    assert_eq!(tracer.active_step, Some(1));
    assert!(tracer.retry_attempts.is_empty());

    tracer.on_llm_retrying(1, 3, 200, "timeout");
    tracer.on_llm_retrying(2, 3, 400, "rate_limit");
    assert_eq!(tracer.retry_attempts.len(), 2);
    assert_eq!(tracer.retry_attempts[0].attempt, 1);
    assert_eq!(tracer.retry_attempts[1].error, "rate_limit");

    // on_llm_end 后重试记录被清空，active_step 重置
    tracer.on_llm_end(1, "gpt-4o", "OpenAI", "result after retry", None);
    assert!(tracer.retry_attempts.is_empty());
    assert!(tracer.active_step.is_none());
}

// === Test 14: on_llm_start 清空上一轮的 retry_attempts ===
#[tokio::test]
async fn test_llm_start_clears_retry_attempts() {
    let mut tracer = make_tracer();

    tracer.on_llm_start(0, &[], &[]);
    tracer.on_llm_retrying(1, 3, 100, "error");
    assert_eq!(tracer.retry_attempts.len(), 1);

    // 新一轮 LLM 调用清空 retry_attempts
    tracer.on_llm_start(1, &[], &[]);
    assert!(tracer.retry_attempts.is_empty());
    assert_eq!(tracer.active_step, Some(1));
}

// === Test 15: on_compact_end 无 compact span 时静默返回 ===
#[tokio::test]
async fn test_compact_end_without_start_no_panic() {
    let mut tracer = make_tracer();
    tracer.on_compact_end("summary", 1, 0, 0, false, "");
    // 不应 panic
}

// === Test 16: on_llm_request_payload 缓存 raw_body 到 generation_data ===
#[tokio::test]
async fn test_on_llm_request_payload_caches_raw_body() {
    let mut tracer = make_tracer();
    tracer.on_llm_start(7, &[], &[]);
    let body = serde_json::json!({"messages":[{"role":"system","content":"x"}]});
    tracer.on_llm_request_payload(7, Arc::new(body.clone()));
    let cached = tracer
        .generation_data
        .get(&7)
        .expect("step 7 应有 generation_data 缓存");
    assert_eq!(cached.raw_body, Some(body));
}

// === Test 17: on_llm_request_payload 在 on_llm_start 之前到达时静默 no-op ===
#[tokio::test]
async fn test_on_llm_request_payload_without_start_is_noop() {
    let mut tracer = make_tracer();
    // 未先调 on_llm_start，generation_data 为空 → 不应 panic
    tracer.on_llm_request_payload(42, Arc::new(serde_json::json!({"x": 1})));
    assert!(tracer.generation_data.is_empty());
}

// === Test 18: on_llm_end 优先用 raw_body 作为 Generation input（Provider-native 格式） ===
#[tokio::test]
async fn test_on_llm_end_prefers_raw_body_over_messages_tools() {
    let mut tracer = make_tracer();
    tracer.on_llm_start(3, &[], &[]);
    // 关键：raw_body 含 Provider-native 完整请求体（含 system / 工具格式）
    let raw = serde_json::json!({
        "model": "claude-sonnet-4",
        "system": "REAL_SYSTEM_PROMPT",
        "messages": [{"role":"user","content":"hi"}],
        "tools": [{"name":"X","description":"d","input_schema":{}}]
    });
    tracer.on_llm_request_payload(3, Arc::new(raw.clone()));
    // 取出 cached 验证 raw_body 已写入（on_llm_end 会消费 generation_data）
    tracer.on_llm_end(3, "claude-sonnet-4", "Anthropic", "answer", None);
    // generation_data 应被 on_llm_end 移除
    assert!(
        tracer.generation_data.is_empty(),
        "on_llm_end 应消费 generation_data[step]"
    );
    // 事件已入队 batcher（无法直接读回 input 字段，但消费成功即说明 raw_body 路径未报错）
}

// === Test 19: on_llm_end 在 raw_body 缺失时 fallback 到 messages+tools 序列化 ===
#[tokio::test]
async fn test_on_llm_end_fallback_to_messages_when_no_payload() {
    let mut tracer = make_tracer();
    // 未调 on_llm_request_payload → raw_body 缺失
    tracer.on_llm_start(2, &[], &[]);
    tracer.on_llm_end(2, "gpt-4o", "OpenAI", "result", None);
    // 旧路径应仍然工作（不 panic），generation_data 被消费
    assert!(tracer.generation_data.is_empty());
}
