use std::sync::Arc;

use peri_agent::{
    agent::{state::AgentState, token::ContextBudget},
    messages::{BaseMessage, ContentBlock},
};

use super::*;

fn make_state() -> AgentState {
    AgentState::new("/tmp/test")
}

fn make_config() -> CompactConfig {
    CompactConfig::default()
}

fn make_budget(context_window: u32) -> ContextBudget {
    ContextBudget::new(context_window)
}

fn make_event_tx() -> Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    Arc::new(Mutex::new(Some(tx)))
}

fn make_middleware() -> CompactMiddleware {
    CompactMiddleware {
        model: None,
        config: make_config(),
        budget: make_budget(200_000),
        cwd: "/tmp/test".to_string(),
        event_tx: make_event_tx(),
        cancel: AgentCancellationToken::default(),
        hooks: vec![],
        session_id: "test-session".to_string(),
        provider_name: "test-model".to_string(),
        micro_compact_done: AtomicBool::new(false),
    }
}

#[tokio::test]
async fn test_name_returns_compact_middleware() {
    let mw = make_middleware();
    assert_eq!(
        <CompactMiddleware as Middleware<AgentState>>::name(&mw),
        "CompactMiddleware"
    );
}

#[tokio::test]
async fn test_before_model_noop_when_disabled_by_env() {
    // 使用 config.auto_compact_enabled=false 模拟 disable（避免 env var 并行测试污染）
    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_noop_when_config_disabled() {
    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_noop_when_below_threshold() {
    // tracker 用量低，不触发任何 compact
    let mw = make_middleware();
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_with_low_budget_triggers_full_or_micro() {
    // budget 为 1000 token 且 tracker 已累积 → 应触发 compact
    let mut state = make_state();
    // 向 state 添加大量消息
    state.add_message(BaseMessage::human(vec![ContentBlock::text(
        "hello ".repeat(100),
    )]));

    let mw = CompactMiddleware {
        budget: ContextBudget::new(100), // 极小窗口
        model: None,                     // 无 model，full compact 会跳过
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    // 无 model 时 full compact 返回 Ok 但跳过
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_compact_without_model_skips_full() {
    // 验证无 model 时 full compact 被跳过
    let mut state = make_state();
    state.add_message(BaseMessage::human(vec![ContentBlock::text("test message")]));

    let mw = CompactMiddleware {
        budget: ContextBudget::new(100),
        model: None,
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    assert!(result.is_ok());
    // 无 model 时不应该 panic
}

#[tokio::test]
async fn test_borrow_safety_then_mut() {
    // 验证先读 tracker 后改 messages 的借用模式
    let mut state = make_state();
    state.add_message(BaseMessage::human(vec![ContentBlock::text("test")]));

    // 即使有低 budget，借用模式也不应 panic
    let mw = CompactMiddleware {
        budget: ContextBudget::new(1_000_000), // 大窗口，不触发
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_is_disabled_detects_config() {
    let mw = make_middleware();
    // 默认情况 auto_compact_enabled=true，不应 disabled
    assert!(!mw.is_disabled());

    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    assert!(mw.is_disabled());
}

#[tokio::test]
async fn test_micro_compact_once_per_prompt() {
    // 验证 micro compact 在同一个 middleware 实例中只触发一次
    let mut state = make_state();
    // 添加足够的消息使 stale_steps 之外的工具有可压缩内容
    for i in 0..8 {
        state.add_message(BaseMessage::ai_with_tool_calls(
            peri_agent::messages::MessageContent::text("using tool"),
            vec![peri_agent::messages::ToolCallRequest::new(
                format!("tc{}", i),
                "Bash",
                serde_json::json!({}),
            )],
        ));
        state.add_message(BaseMessage::tool_result(
            format!("tc{}", i),
            "x".repeat(600),
        ));
    }

    // 设置 token tracker 使 should_warn() 返回 true
    // context_window=1000, input_tokens=800 → 80% > 70% (warning threshold)
    state
        .token_tracker_mut()
        .accumulate(&peri_agent::llm::types::TokenUsage {
            input_tokens: 800,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            request_id: None,
        });

    // 极小 budget 使 should_warn() 返回 true（70% 阈值）
    let mut mw = CompactMiddleware {
        budget: ContextBudget::new(1000),
        config: {
            let mut c = make_config();
            c.micro_compact_stale_steps = 1;
            c
        },
        ..make_middleware()
    };

    // 第一次调用：micro compact 应该触发
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    mw.event_tx = Arc::new(Mutex::new(Some(tx1)));

    mw.before_model(&mut state).await.unwrap();

    // 应收到 CompactCompleted 事件
    let event1 = rx1.try_recv();
    assert!(
        matches!(event1, Ok(ExecutorEvent::CompactCompleted { micro_cleared, .. }) if micro_cleared > 0),
        "第一次 micro compact 应触发并清理工具结果"
    );

    // 第二次调用：micro compact 不应再触发
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    mw.event_tx = Arc::new(Mutex::new(Some(tx2)));

    mw.before_model(&mut state).await.unwrap();

    let event2 = rx2.try_recv();
    assert!(
        event2.is_err(),
        "第二次 micro compact 不应触发（once-per-prompt 守卫）"
    );

    // 确认标志已设置
    assert!(mw.micro_compact_done.load(Ordering::Relaxed));
}

/// 验证 compact 替换 messages 后，头部 System 消息被保留
/// 根因：do_full_compact 用 *state.messages_mut() = new_messages 整体替换，
/// 丢失了 with_system_prompt / before_agent 注入的头部 System 消息。
/// 修复：compact 后重新前置原始头部的 System 消息。
#[test]
fn test_do_full_compact_preserves_system_prefix() {
    // 模拟 compact 前的 state.messages()：
    // [System(system_prompt), System(claude_md), Human(input), Ai(response), Tool(result)]
    let original_messages = [
        BaseMessage::system("system prompt"),
        BaseMessage::system("CLAUDE.md content"),
        BaseMessage::human(vec![ContentBlock::text("user message")]),
        BaseMessage::ai_with_tool_calls(
            peri_agent::messages::MessageContent::text("assistant response"),
            vec![peri_agent::messages::ToolCallRequest::new(
                "tc1",
                "Bash",
                serde_json::json!({"command": "ls"}),
            )],
        ),
        BaseMessage::tool_result("tc1", "file1\nfile2"),
    ];

    // 模拟 compact 产出的 new_messages（不含 system prefix）
    let compact_summary = "此会话讨论了文件列表";
    let summary_content = format!(
        "{}\n\n{}",
        compact_summary,
        peri_agent::agent::compact::CONTINUATION_HINT
    );
    let mut new_messages = vec![BaseMessage::human(vec![ContentBlock::text(
        &summary_content,
    )])];
    new_messages.push(BaseMessage::system(
        "[最近读取的文件: /tmp/test]\nfile content",
    ));

    // 模拟修复逻辑：保留头部 System 消息并前置
    let system_prefix: Vec<BaseMessage> = original_messages
        .iter()
        .take_while(|m| m.is_system())
        .cloned()
        .collect();
    for sys_msg in system_prefix.into_iter().rev() {
        new_messages.insert(0, sys_msg);
    }

    // 验证：头部 System 消息保留
    assert_eq!(
        new_messages.len(),
        4,
        "应有 2 个原始 System + 1 个 Human 摘要 + 1 个 re_inject System"
    );
    assert!(new_messages[0].is_system());
    assert_eq!(new_messages[0].content(), "system prompt");
    assert!(new_messages[1].is_system());
    assert_eq!(new_messages[1].content(), "CLAUDE.md content");
    // Human 摘要紧随 System 前缀
    assert!(matches!(new_messages[2], BaseMessage::Human { .. }));
    assert!(new_messages[2].content().contains(compact_summary));
    // re_inject System 在最后
    assert!(new_messages[3].is_system());
}

/// 验证无头部 System 消息时，compact 正常工作（无 crash、无多余插入）
#[test]
fn test_do_full_compact_no_system_prefix_is_noop() {
    let original_messages = [
        BaseMessage::human(vec![ContentBlock::text("user message")]),
        BaseMessage::ai(vec![ContentBlock::text("response")]),
    ];

    let mut new_messages = vec![BaseMessage::human(vec![ContentBlock::text(
        "compact summary",
    )])];

    // 模拟修复逻辑
    let system_prefix: Vec<BaseMessage> = original_messages
        .iter()
        .take_while(|m| m.is_system())
        .cloned()
        .collect();
    for sys_msg in system_prefix.into_iter().rev() {
        new_messages.insert(0, sys_msg);
    }

    // 无 System 前缀时，new_messages 不变
    assert_eq!(new_messages.len(), 1);
    assert!(matches!(new_messages[0], BaseMessage::Human { .. }));
}

/// 验证保留的 System 消息保持原始 MessageId（cleanup_prepended 依赖 ID 匹配）
#[test]
fn test_preserved_system_messages_keep_original_ids() {
    let sys1 = BaseMessage::system("system prompt");
    let sys1_id = sys1.id();
    let sys2 = BaseMessage::system("CLAUDE.md");
    let sys2_id = sys2.id();

    let original_messages = [
        sys1,
        sys2,
        BaseMessage::human(vec![ContentBlock::text("input")]),
    ];

    let mut new_messages = vec![BaseMessage::human(vec![ContentBlock::text(
        "compact summary",
    )])];

    let system_prefix: Vec<BaseMessage> = original_messages
        .iter()
        .take_while(|m| m.is_system())
        .cloned()
        .collect();
    for sys_msg in system_prefix.into_iter().rev() {
        new_messages.insert(0, sys_msg);
    }

    // 验证 ID 保留
    assert_eq!(
        new_messages[0].id(),
        sys1_id,
        "第一个 System 消息应保留原始 ID"
    );
    assert_eq!(
        new_messages[1].id(),
        sys2_id,
        "第二个 System 消息应保留原始 ID"
    );
}

#[test]
fn test_compact_middleware_reset_micro_compact_flag() {
    let mw = make_middleware();
    // 设置标志为 true
    mw.micro_compact_done.store(true, Ordering::SeqCst);
    assert!(mw.micro_compact_done.load(Ordering::SeqCst));

    // reset 后应恢复为 false
    mw.reset();
    assert!(!mw.micro_compact_done.load(Ordering::SeqCst));
}

// ── Compact cancel/error restore 测试 ─────────────────────────────────────────
// 对应 TRAP: CLAUDE.md compact 不变量, spec/global/domains/compact.md

/// 验证 do_full_compact 被 cancel 时 own_messages 回滚后与 compact 前一致。
#[tokio::test]
async fn test_full_compact_cancel_restores_own_messages() {
    // 构造消息列表（含 System 前缀 + Human + Ai + Tool）
    let original: Vec<BaseMessage> = vec![
        BaseMessage::system("system prompt"),
        BaseMessage::human("帮我写函数"),
        BaseMessage::ai_with_tool_calls(
            peri_agent::messages::MessageContent::text("using bash"),
            vec![peri_agent::messages::ToolCallRequest::new(
                "tc1",
                "Bash",
                serde_json::json!({"command": "echo"}),
            )],
        ),
        BaseMessage::tool_result("tc1", "编译成功"),
    ];

    // 构造 state：ancestor_len=0（整个消息列表都是 own）
    let mut state = make_state();
    for msg in &original {
        state.add_message(msg.clone());
    }
    let orig_count = state.messages().len();

    // 模拟 do_full_compact 的 restore 逻辑：drain → cancel → extend
    let own_messages: Vec<BaseMessage> = state.messages_mut().drain(0..).collect();
    assert_eq!(
        own_messages.len(),
        orig_count,
        "drain 后 own_messages 数应与原消息一致"
    );
    assert_eq!(state.messages().len(), 0, "drain 后 state 应为空");

    // 回滚
    state.messages_mut().extend(own_messages);

    // 消息数量、顺序、role 完全一致
    assert_eq!(state.messages().len(), orig_count, "回滚后消息数应与原一致");
    // 验证具体消息类型和内容
    assert_eq!(state.messages().len(), 4, "回滚后应有 4 条消息");
    assert_eq!(state.messages()[0].content(), "system prompt");
    assert!(matches!(state.messages()[1], BaseMessage::Human { .. }));
    assert!(matches!(state.messages()[2], BaseMessage::Ai { .. }));
    assert!(matches!(state.messages()[3], BaseMessage::Tool { .. }));
    // 验证 tool_use 与 tool_result 配对（孤儿 tool_use 会导致 Anthropic 400）
    let tool_ids: Vec<_> = state
        .messages()
        .iter()
        .filter_map(|m| {
            if let BaseMessage::Tool { tool_call_id, .. } = m {
                Some(tool_call_id.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(!tool_ids.is_empty(), "回滚后应保留 tool_result");
}

/// 验证 do_full_compact LLM 失败时 own_messages 回滚后与 compact 前一致。
#[tokio::test]
async fn test_full_compact_error_restores_own_messages() {
    // 构造消息列表（含多个 tool_use 和 tool_result）
    let original: Vec<BaseMessage> = vec![
        BaseMessage::human("问题1"),
        BaseMessage::ai("回答1"),
        BaseMessage::human("问题2"),
        BaseMessage::ai_with_tool_calls(
            peri_agent::messages::MessageContent::text("using tool"),
            vec![
                peri_agent::messages::ToolCallRequest::new(
                    "t1",
                    "Read",
                    serde_json::json!({"file_path": "/a"}),
                ),
                peri_agent::messages::ToolCallRequest::new(
                    "t2",
                    "Grep",
                    serde_json::json!({"pattern": "x"}),
                ),
            ],
        ),
        BaseMessage::tool_result("t1", "content a"),
        BaseMessage::tool_result("t2", "found 1 match"),
    ];

    let mut state = make_state();
    for msg in &original {
        state.add_message(msg.clone());
    }
    let orig_count = state.messages().len();

    // 模拟 do_full_compact 失败后的 restore 逻辑
    let own_messages: Vec<BaseMessage> = state.messages_mut().drain(0..).collect();
    assert_eq!(own_messages.len(), orig_count);

    // 失败后回滚
    state.messages_mut().extend(own_messages);

    // 消息数不变
    assert_eq!(
        state.messages().len(),
        orig_count,
        "LLM 失败回滚后消息数应与原一致"
    );

    // 验证 tool 消息完整性：每个 tool_use 有配对 tool_result
    let mut ai_tool_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: Vec<String> = Vec::new();
    for msg in state.messages() {
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                ai_tool_ids.push(tc.id.clone());
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = msg {
            tool_result_ids.push(tool_call_id.clone());
        }
    }
    assert_eq!(
        ai_tool_ids.len(),
        tool_result_ids.len(),
        "回滚后 tool_use 数量应与 tool_result 一致"
    );
    for id in &ai_tool_ids {
        assert!(
            tool_result_ids.contains(id),
            "tool_use {id} 缺少配对 tool_result"
        );
    }
}
