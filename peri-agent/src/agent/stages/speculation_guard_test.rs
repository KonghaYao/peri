//! speculation_guard 单元测试
//!
//! 覆盖：推测深挖触发 L1/L2 分级提醒、SubAgent 信号排除、
//! 用户 Prompt 重置、AskUserQuestion 后不再提醒、工具错误路径阈值。
//! 均使用 stub LLM / stub 工具，不弹真实 AskUserQuestion 窗。

use super::*;
use crate::agent::react::{Reasoning, ToolCall};
use crate::agent::stages::{
    run_react_loop, LoopResult, RuntimeServices, SharedToolMap, StageContext,
};
use crate::messages::MessageContent;
use crate::session::queue::{MessageQueue, MessageSource};
use crate::session::store::FrozenContext;
use crate::session::Session;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 构造主 agent 上下文（注入 session_id 主 agent 信号）
fn make_main_agent_context() -> (Arc<Session>, StageContext) {
    let cwd: Arc<str> = Arc::from("/tmp/spec-guard");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let session_ctx = Arc::new(RwLock::new(HashMap::from([(
        "session_id".to_string(),
        "test-session".to_string(),
    )])));
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_session_context(session_ctx)
        .build();
    (session, ctx)
}

/// 断言 transcript + queue 残留消息的组合文本
fn combined_text(ctx: &StageContext) -> String {
    let mut text: String = ctx
        .session
        .transcript
        .read()
        .visible_messages()
        .into_iter()
        .map(|m| m.content().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let rest = ctx.session.queue.drain_all();
    for msg in rest {
        text.push('\n');
        text.push_str(&msg.message.content().to_string());
    }
    text
}

/// 简单探测工具（成功或失败）
struct ProbeTool {
    fail: bool,
}

#[async_trait::async_trait]
impl crate::tools::BaseTool for ProbeTool {
    fn name(&self) -> &str {
        "probe_tool"
    }

    fn description(&self) -> &str {
        "deterministic probe tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if self.fail {
            Err("probe failed".into())
        } else {
            Ok("probe ok".to_string())
        }
    }
}

/// 可编程工具注册表（成功 probe + AskUserQuestion stub）
fn make_tools() -> SharedToolMap {
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "probe_tool".to_string(),
        Arc::new(ProbeTool { fail: false }) as Arc<dyn crate::tools::BaseTool>,
    );
    map.insert(
        "AskUserQuestion".to_string(),
        Arc::new(ProbeTool { fail: false }) as Arc<dyn crate::tools::BaseTool>,
    );
    Arc::new(RwLock::new(map))
}

fn fail_tools() -> SharedToolMap {
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "probe_tool".to_string(),
        Arc::new(ProbeTool { fail: true }) as Arc<dyn crate::tools::BaseTool>,
    );
    Arc::new(RwLock::new(map))
}

fn tool_reasoning(thought: &str) -> Reasoning {
    Reasoning::with_tools(
        thought,
        vec![ToolCall::new("call-1", "probe_tool", serde_json::json!({}))],
    )
}

/// stub LLM：前 `tool_rounds` 次调用返回推测 thought + tool_call，之后 final answer
struct SpeculativeToolLLM {
    tool_rounds: usize,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for SpeculativeToolLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.tool_rounds {
            Ok(tool_reasoning("可能有问题，也许还需要检查"))
        } else {
            Ok(Reasoning::with_answer("", "done"))
        }
    }

    fn model_name(&self) -> String {
        "spec-guard-speculative".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// stub LLM：干净 thought + 工具调用（用于工具错误路径）
struct CleanToolLLM {
    tool_rounds: usize,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for CleanToolLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.tool_rounds {
            Ok(tool_reasoning("处理文件内容并检查输出"))
        } else {
            Ok(Reasoning::with_answer("", "done"))
        }
    }

    fn model_name(&self) -> String {
        "spec-guard-clean".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// stub LLM：前 3 轮推测 tool，第 4 轮 AskUserQuestion，之后继续推测 tool
struct AskUserThenDigLLM {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for AskUserThenDigLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 3 {
            Ok(Reasoning::with_tools(
                "可能需要问用户",
                vec![ToolCall::new(
                    "call-ask",
                    "AskUserQuestion",
                    serde_json::json!({}),
                )],
            ))
        } else {
            Ok(tool_reasoning("可能有问题，也许还需要检查"))
        }
    }

    fn model_name(&self) -> String {
        "spec-guard-ask-user".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// stub LLM：指定调用次数时向 queue 注入用户 Prompt，其余轮推测 tool
struct PromptInjectLLM {
    inject_at: usize,
    queue: MessageQueue,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for PromptInjectLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == self.inject_at {
            self.queue.push(QueuedMessage::prompt(
                MessageSource::UserInput,
                BaseMessage::human(MessageContent::text("user interjected")),
            ));
        }
        Ok(tool_reasoning("可能有问题，也许还需要检查"))
    }

    fn model_name(&self) -> String {
        "spec-guard-prompt-inject".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

// ─── 单元测试：纯函数 ────────────────────────────────────────────────────────

#[test]
fn test_contains_speculation_matches_words() {
    assert!(contains_speculation("可能有问题"));
    assert!(contains_speculation("也许需要重试"));
    assert!(contains_speculation("大概是权限问题"));
    assert!(contains_speculation("PROBABLY a race"));
    assert!(contains_speculation("maybe the env"));
    assert!(contains_speculation("推测是缓存"));
    assert!(contains_speculation("猜测与剪贴板有关"));
    assert!(!contains_speculation("处理文件内容并检查输出"));
    assert!(!contains_speculation(""));
}

#[test]
fn test_window_all_hit_requires_full_window() {
    let mut w: VecDeque<bool> = VecDeque::new();
    assert!(!window_all_hit(&w), "空窗口不应命中");
    push_window(&mut w, SPECULATION_WINDOW, true);
    assert!(!window_all_hit(&w), "单轮不应命中");
    push_window(&mut w, SPECULATION_WINDOW, true);
    assert!(window_all_hit(&w), "满窗口全 true 应命中");
    push_window(&mut w, SPECULATION_WINDOW, false);
    assert!(
        !window_all_hit(&w),
        "新轮非命中应替换最旧（[true,false] 不命中）"
    );
    push_window(&mut w, SPECULATION_WINDOW, true);
    assert!(
        !window_all_hit(&w),
        "窗口滑动保留旧值语义（[false,true] 不命中）"
    );
    push_window(&mut w, SPECULATION_WINDOW, true);
    assert!(
        window_all_hit(&w),
        "窗口滑动后满窗全 true 命中（[true,true]）"
    );
}

// ─── e2e：分级提醒 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_l1_l2_escalation_with_speculative_thought() {
    // 推测词命中只影响措辞不影响阈值 → N1=6 → L1@6、L2@10；工具成功（errors 窗口不参与）
    let (_session, ctx) = make_main_agent_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = StageContext {
        runtime: RuntimeServices {
            llm: Arc::new(SpeculativeToolLLM {
                tool_rounds: 14,
                calls: Arc::clone(&calls),
            }),
            tools: make_tools(),
            ..ctx.runtime
        },
        ..ctx
    };

    let result = run_react_loop(ctx.clone(), 20).await;
    assert!(matches!(result, LoopResult::Completed));

    let text = combined_text(&ctx);
    assert!(
        text.contains("推测深挖无新证据"),
        "应出现 L1 提醒, got: {}",
        text
    );
    assert!(
        text.contains("应停止静态追查"),
        "应出现 L2 提醒, got: {}",
        text
    );
    assert!(
        text.contains("已连续 6 轮推测深挖"),
        "L1 应带实际轮数 6, got: {}",
        text
    );
}

#[tokio::test]
async fn test_no_trigger_without_main_agent_signal() {
    // SubAgent 信号（无 session_id）→ 哨兵不启用 → 深挖无提醒
    let cwd: Arc<str> = Arc::from("/tmp/spec-guard-subagent");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(SpeculativeToolLLM {
            tool_rounds: 10,
            calls: Arc::clone(&calls),
        }))
        .with_tools(make_tools())
        .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));

    let result = run_react_loop(ctx.clone(), 20).await;
    assert!(matches!(result, LoopResult::Completed));

    let text = combined_text(&ctx);
    assert!(
        !text.contains("推测深挖无新证据"),
        "SubAgent（无 session_id）不应触发提醒, got: {}",
        text
    );
    assert!(
        !text.contains("必须 AskUserQuestion"),
        "SubAgent（无 session_id）不应触发 L2, got: {}",
        text
    );
    assert!(
        !text.contains("应停止静态追查"),
        "SubAgent（无 session_id）不应触发 L2, got: {}",
        text
    );
}

#[tokio::test]
async fn test_no_trigger_with_ask_discipline_disabled() {
    // with_ask_discipline(false) 显式关闭 → 深挖无提醒
    let cwd: Arc<str> = Arc::from("/tmp/spec-guard-disabled");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let session_ctx = Arc::new(RwLock::new(HashMap::from([(
        "session_id".to_string(),
        "test-session".to_string(),
    )])));
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_session_context(session_ctx)
        .with_ask_discipline(false)
        .with_llm(Arc::new(SpeculativeToolLLM {
            tool_rounds: 10,
            calls: Arc::clone(&calls),
        }))
        .with_tools(make_tools())
        .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));

    let result = run_react_loop(ctx.clone(), 20).await;
    assert!(matches!(result, LoopResult::Completed));

    let text = combined_text(&ctx);
    assert!(
        !text.contains("推测深挖无新证据"),
        "ask_discipline=false 不应触发提醒, got: {}",
        text
    );
}

#[tokio::test]
async fn test_no_trigger_after_ask_user_question() {
    // D 条件：已 AskUserQuestion → 之后深挖不提醒
    let (_session, ctx) = make_main_agent_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = StageContext {
        runtime: RuntimeServices {
            llm: Arc::new(AskUserThenDigLLM {
                calls: Arc::clone(&calls),
            }),
            tools: make_tools(),
            ..ctx.runtime
        },
        ..ctx
    };

    let result = run_react_loop(ctx.clone(), 20).await;
    // stub LLM 永不返回 final answer（一直在深挖）→ 循环跑满 max_iterations
    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(20))
    ));

    let text = combined_text(&ctx);
    assert!(
        !text.contains("推测深挖无新证据"),
        "AskUserQuestion 后不应再提醒, got: {}",
        text
    );
    assert!(
        !text.contains("必须 AskUserQuestion"),
        "AskUserQuestion 后不应出现 L2, got: {}",
        text
    );
    assert!(
        !text.contains("应停止静态追查"),
        "AskUserQuestion 后不应出现 L2, got: {}",
        text
    );
}

#[tokio::test]
async fn test_reset_on_user_prompt_interjection() {
    // 用户中途插入 Prompt → 计数清零重新累计：L1 触发 2 次（reset 前后各 1），
    // 且无 L2（若无 reset，轮数会持续累计到 N1+4 触发 L2）。
    // 注意：make_tools 的 AskUserQuestion stub 与 probe_tool 同名会触发
    // ambiguous 解析失败（连续失败提醒中断 observe 计数），此处用 probe-only 表。
    let tools = Arc::new(RwLock::new(std::collections::BTreeMap::from([(
        "probe_tool".to_string(),
        Arc::new(ProbeTool { fail: false }) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let (_session, ctx) = make_main_agent_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let queue = ctx.session.queue.clone();
    let ctx = StageContext {
        runtime: RuntimeServices {
            llm: Arc::new(PromptInjectLLM {
                inject_at: 7, // 第 8 次 LLM 调用（iter8）注入用户 Prompt；iter2-7 触发首次 L1
                queue,
                calls: Arc::clone(&calls),
            }),
            tools,
            ..ctx.runtime
        },
        ..ctx
    };

    let result = run_react_loop(ctx.clone(), 16).await;
    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(16))
    ));

    let text = combined_text(&ctx);
    assert_eq!(
        text.matches("推测深挖无新证据").count(),
        2,
        "用户 Prompt 应重置计数并重新触发 L1, got: {}",
        text
    );
    assert!(
        !text.contains("应停止静态追查"),
        "reset 后轮数不足以触发 L2, got: {}",
        text
    );
}

#[tokio::test]
async fn test_l1_at_default_threshold_on_tool_errors() {
    // thought 干净 + 工具连续失败 → errors 窗口命中 → N1=6 → L1@6、L2@10
    let (_session, ctx) = make_main_agent_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("investigate")),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = StageContext {
        runtime: RuntimeServices {
            llm: Arc::new(CleanToolLLM {
                tool_rounds: 14,
                calls: Arc::clone(&calls),
            }),
            tools: fail_tools(),
            ..ctx.runtime
        },
        ..ctx
    };

    let result = run_react_loop(ctx.clone(), 20).await;
    assert!(matches!(result, LoopResult::Completed));

    let text = combined_text(&ctx);
    assert!(
        text.contains("已连续 6 轮工具调用无进展"),
        "工具错误路径应在默认阈值 6 触发 L1, got: {}",
        text
    );
    assert!(
        text.contains("应停止静态追查"),
        "应出现 L2 提醒, got: {}",
        text
    );
}
