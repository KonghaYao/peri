//! Tests for chain

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;
use crate::{
    agent::state::AgentState,
    error::{AgentError, AgentResult},
    messages::{BaseMessage, ContentBlock, MessageId},
    middleware::{
        r#trait::{Middleware, NoopMiddleware},
        state::MiddlewareState,
    },
};

/// 记录调用顺序的中间件
struct OrderRecorder {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl OrderRecorder {
    fn new(name: &str, log: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            name: name.to_string(),
            log,
        }
    }
}

#[async_trait]
impl Middleware for OrderRecorder {
    fn name(&self) -> &str {
        &self.name
    }

    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}.before_agent", self.name));
        Ok(())
    }

    async fn before_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}.before_tool", self.name));
        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        _tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}.after_tool", self.name));
        Ok(())
    }
}

/// 修改 ToolCall 的中间件（用于验证 before_tool 链式传播）
struct InputModifier {
    suffix: String,
}

#[async_trait]
impl Middleware for InputModifier {
    fn name(&self) -> &str {
        "InputModifier"
    }

    async fn before_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let mut modified = tool_call.clone();
        let new_name = format!("{}{}", tool_call.name, self.suffix);
        modified.name = new_name;
        Ok(modified)
    }
}

/// 总是返回错误的中间件（用于验证短路行为）
struct FailMiddleware;

#[async_trait]
impl Middleware for FailMiddleware {
    fn name(&self) -> &str {
        "FailMiddleware"
    }

    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Err(AgentError::MiddlewareError {
            middleware: "FailMiddleware".to_string(),
            reason: "intentional failure".to_string(),
        })
    }
}

#[tokio::test]
async fn test_multiple_middlewares_sequential_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(OrderRecorder::new("A", Arc::clone(&log))));
    chain.add(Box::new(OrderRecorder::new("B", Arc::clone(&log))));
    chain.add(Box::new(OrderRecorder::new("C", Arc::clone(&log))));

    let mut state = AgentState::new("/tmp");
    chain.run_before_agent(&mut state).await.unwrap();

    let calls = log.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec!["A.before_agent", "B.before_agent", "C.before_agent"]
    );
}

#[tokio::test]
async fn test_error_short_circuits_chain() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(OrderRecorder::new("A", Arc::clone(&log))));
    chain.add(Box::new(FailMiddleware));
    chain.add(Box::new(OrderRecorder::new("B", Arc::clone(&log))));

    let mut state = AgentState::new("/tmp");
    let result = chain.run_before_agent(&mut state).await;

    assert!(result.is_err(), "应该返回错误");
    // B.before_agent 不应被执行
    let calls = log.lock().unwrap().clone();
    assert_eq!(calls, vec!["A.before_agent"]);
}

#[tokio::test]
async fn test_before_tool_modification_propagates() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(InputModifier {
        suffix: "_modified".to_string(),
    }));

    let mut state = AgentState::new("/tmp");
    let original = ToolCall::new("id1", "my_tool", serde_json::json!({}));
    let result = chain.run_before_tool(&mut state, original).await.unwrap();

    assert_eq!(result.name, "my_tool_modified");
}

#[tokio::test]
async fn test_before_tool_chained_modifications() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(InputModifier {
        suffix: "_a".to_string(),
    }));
    chain.add(Box::new(InputModifier {
        suffix: "_b".to_string(),
    }));

    let mut state = AgentState::new("/tmp");
    let original = ToolCall::new("id1", "tool", serde_json::json!({}));
    let result = chain.run_before_tool(&mut state, original).await.unwrap();

    assert_eq!(result.name, "tool_a_b");
}

#[tokio::test]
async fn test_empty_chain_runs_ok() {
    let chain = MiddlewareChain::new();
    let mut state = AgentState::new("/tmp");
    chain.run_before_agent(&mut state).await.unwrap();

    let original = ToolCall::new("id", "tool", serde_json::json!({}));
    let result = chain
        .run_before_tool(&mut state, original.clone())
        .await
        .unwrap();
    assert_eq!(result.name, original.name);
}

#[tokio::test]
async fn test_after_tool_sequential_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(OrderRecorder::new("A", Arc::clone(&log))));
    chain.add(Box::new(OrderRecorder::new("B", Arc::clone(&log))));

    let mut state = AgentState::new("/tmp");
    let call = ToolCall::new("id", "tool", serde_json::json!({}));
    let result = ToolResult {
        tool_call_id: "id".to_string(),
        tool_name: "tool".to_string(),
        output: "ok".to_string(),
        is_error: false,
    };
    chain
        .run_after_tool(&mut state, &call, &result)
        .await
        .unwrap();

    let calls = log.lock().unwrap().clone();
    assert_eq!(calls, vec!["A.after_tool", "B.after_tool"]);
}

/// 批量工具调用：一个中间件批准、下一个中间件拒绝（混合结果）
#[tokio::test]
async fn test_before_tools_batch_mixed_approval() {
    // 第一个中间件：所有工具加 _a 后缀
    struct SuffixA;
    #[async_trait]
    impl Middleware for SuffixA {
        fn name(&self) -> &str {
            "SuffixA"
        }
        async fn before_tool(
            &self,
            _state: &mut dyn MiddlewareState,
            tc: &ToolCall,
        ) -> AgentResult<ToolCall> {
            let mut m = tc.clone();
            m.name = format!("{}{}", tc.name, "_a");
            Ok(m)
        }
    }

    // 第二个中间件：第二个工具调用返回 ToolRejected，第一个和第三个放行
    struct RejectSecond;
    #[async_trait]
    impl Middleware for RejectSecond {
        fn name(&self) -> &str {
            "RejectSecond"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn MiddlewareState,
            calls: &[ToolCall],
        ) -> Vec<AgentResult<ToolCall>> {
            calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if i == 1 {
                        Err(AgentError::ToolRejected {
                            tool: c.name.clone(),
                            reason: "拒绝第二个".to_string(),
                        })
                    } else {
                        Ok(c.clone())
                    }
                })
                .collect()
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(SuffixA));
    chain.add(Box::new(RejectSecond));
    let mut state = AgentState::new("/tmp");

    let calls = vec![
        ToolCall::new("id1", "tool1", serde_json::json!({})),
        ToolCall::new("id2", "tool2", serde_json::json!({})),
        ToolCall::new("id3", "tool3", serde_json::json!({})),
    ];
    let results = chain.run_before_tools_batch(&mut state, calls).await;

    assert_eq!(results.len(), 3);
    // 第一个：通过，名称被 SuffixA 修改为 tool1_a
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap().name, "tool1_a");
    // 第二个：被 RejectSecond 拒绝
    assert!(matches!(&results[1], Err(AgentError::ToolRejected { tool, .. }) if tool == "tool2_a"));
    // 第三个：通过
    assert!(results[2].is_ok());
    assert_eq!(results[2].as_ref().unwrap().name, "tool3_a");
}

/// 批量工具调用：所有中间件使用默认逐条实现，结果应与逐个调用一致
#[tokio::test]
async fn test_before_tools_batch_equivalent_to_individual() {
    struct SuffixX;
    #[async_trait]
    impl Middleware for SuffixX {
        fn name(&self) -> &str {
            "SuffixX"
        }
        async fn before_tool(
            &self,
            _state: &mut dyn MiddlewareState,
            tc: &ToolCall,
        ) -> AgentResult<ToolCall> {
            let mut m = tc.clone();
            m.name = format!("{}{}", tc.name, "_x");
            Ok(m)
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(SuffixX));
    let mut state = AgentState::new("/tmp");

    let calls = vec![
        ToolCall::new("id1", "t1", serde_json::json!({})),
        ToolCall::new("id2", "t2", serde_json::json!({})),
    ];

    let batch_results = chain
        .run_before_tools_batch(&mut state, calls.clone())
        .await;
    assert_eq!(batch_results.len(), 2);
    assert_eq!(batch_results[0].as_ref().unwrap().name, "t1_x");
    assert_eq!(batch_results[1].as_ref().unwrap().name, "t2_x");
}

// ── before_model / after_model 测试 ──

#[tokio::test]
async fn test_before_model_sequential_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();

    struct BeforeModelRecorder {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for BeforeModelRecorder {
        fn name(&self) -> &str {
            &self.name
        }
        async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.before_model", self.name));
            Ok(())
        }
    }

    chain.add(Box::new(BeforeModelRecorder {
        name: "A".into(),
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(BeforeModelRecorder {
        name: "B".into(),
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(BeforeModelRecorder {
        name: "C".into(),
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");
    chain.run_before_model(&mut state).await.unwrap();

    assert_eq!(
        log.lock().unwrap().clone(),
        vec!["A.before_model", "B.before_model", "C.before_model"]
    );
}

#[tokio::test]
async fn test_before_model_error_short_circuits() {
    struct FailBeforeModel;
    #[async_trait]
    impl Middleware for FailBeforeModel {
        fn name(&self) -> &str {
            "FailBeforeModel"
        }
        async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
            Err(AgentError::MiddlewareError {
                middleware: "FailBeforeModel".to_string(),
                reason: "intentional failure".to_string(),
            })
        }
    }

    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();

    struct Recorder {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for Recorder {
        fn name(&self) -> &str {
            &self.name
        }
        async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.before_model", self.name));
            Ok(())
        }
    }

    chain.add(Box::new(Recorder {
        name: "A".into(),
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(FailBeforeModel));
    chain.add(Box::new(Recorder {
        name: "B".into(),
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");
    let result = chain.run_before_model(&mut state).await;

    assert!(result.is_err());
    assert_eq!(log.lock().unwrap().clone(), vec!["A.before_model"]);
}

#[tokio::test]
async fn test_after_model_sequential_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();

    struct AfterModelRecorder {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for AfterModelRecorder {
        fn name(&self) -> &str {
            &self.name
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            _reasoning: &Reasoning,
        ) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.after_model", self.name));
            Ok(())
        }
    }

    chain.add(Box::new(AfterModelRecorder {
        name: "A".into(),
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(AfterModelRecorder {
        name: "B".into(),
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");
    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();

    assert_eq!(
        log.lock().unwrap().clone(),
        vec!["A.after_model", "B.after_model"]
    );
}

#[tokio::test]
async fn test_after_model_error_short_circuits() {
    struct FailAfterModel;
    #[async_trait]
    impl Middleware for FailAfterModel {
        fn name(&self) -> &str {
            "FailAfterModel"
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            _reasoning: &Reasoning,
        ) -> AgentResult<()> {
            Err(AgentError::MiddlewareError {
                middleware: "FailAfterModel".to_string(),
                reason: "intentional failure".to_string(),
            })
        }
    }

    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();

    struct Recorder {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for Recorder {
        fn name(&self) -> &str {
            &self.name
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            _reasoning: &Reasoning,
        ) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.after_model", self.name));
            Ok(())
        }
    }

    chain.add(Box::new(Recorder {
        name: "A".into(),
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(FailAfterModel));
    chain.add(Box::new(Recorder {
        name: "B".into(),
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");
    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    let result = chain.run_after_model(&mut state, &reasoning).await;

    assert!(result.is_err());
    assert_eq!(log.lock().unwrap().clone(), vec!["A.after_model"]);
}

#[tokio::test]
async fn test_before_model_empty_chain_ok() {
    let chain = MiddlewareChain::new();
    let mut state = AgentState::new("/tmp");
    assert!(chain.run_before_model(&mut state).await.is_ok());
}

#[tokio::test]
async fn test_after_model_empty_chain_ok() {
    let chain = MiddlewareChain::new();
    let mut state = AgentState::new("/tmp");
    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    assert!(chain.run_after_model(&mut state, &reasoning).await.is_ok());
}

#[tokio::test]
async fn test_new_hooks_default_noop() {
    // NoopMiddleware 的 before_model/after_model 默认实现不应报错
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(NoopMiddleware::new("noop")));
    let mut state = AgentState::new("/tmp");

    chain.run_before_model(&mut state).await.unwrap();

    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();
}

/// 验证 before_model 和 after_model 在同一链中独立执行：
/// A 覆盖两个钩子，B 只覆盖 before_model，C 只覆盖 after_model。
/// run_before_model 应触发 A+B 但跳过 C；
/// run_after_model 应触发 A+C 但跳过 B。
#[tokio::test]
async fn test_mixed_before_and_after_model_in_same_chain() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    // A 覆盖两个钩子
    struct BothHooks {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for BothHooks {
        fn name(&self) -> &str {
            "A"
        }
        async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
            self.log.lock().unwrap().push("A.before_model".into());
            Ok(())
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            _r: &Reasoning,
        ) -> AgentResult<()> {
            self.log.lock().unwrap().push("A.after_model".into());
            Ok(())
        }
    }

    // B 只覆盖 before_model
    struct BeforeOnly {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for BeforeOnly {
        fn name(&self) -> &str {
            "B"
        }
        async fn before_model(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
            self.log.lock().unwrap().push("B.before_model".into());
            Ok(())
        }
    }

    // C 只覆盖 after_model
    struct AfterOnly {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for AfterOnly {
        fn name(&self) -> &str {
            "C"
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            _r: &Reasoning,
        ) -> AgentResult<()> {
            self.log.lock().unwrap().push("C.after_model".into());
            Ok(())
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(BothHooks {
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(BeforeOnly {
        log: Arc::clone(&log),
    }));
    chain.add(Box::new(AfterOnly {
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");

    // run_before_model: A + B 执行，C 不执行
    log.lock().unwrap().clear();
    chain.run_before_model(&mut state).await.unwrap();
    assert_eq!(
        log.lock().unwrap().clone(),
        vec!["A.before_model", "B.before_model"]
    );

    // run_after_model: A + C 执行，B 不执行
    log.lock().unwrap().clear();
    let reasoning = Reasoning {
        thought: "test".into(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();
    assert_eq!(
        log.lock().unwrap().clone(),
        vec!["A.after_model", "C.after_model"]
    );
}

/// before_model 修改 state（如添加消息），随后 after_model 应能读取该修改。
#[tokio::test]
async fn test_state_mutation_visible_across_hooks() {
    let marker_id = Arc::new(Mutex::new(None::<MessageId>));

    struct Writer {
        marker_id: Arc<Mutex<Option<MessageId>>>,
    }
    #[async_trait]
    impl Middleware for Writer {
        fn name(&self) -> &str {
            "Writer"
        }
        async fn before_model(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
            let msg =
                BaseMessage::system(vec![ContentBlock::text("marker written by before_model")]);
            let id = msg.id();
            state.add_message(msg);
            *self.marker_id.lock().unwrap() = Some(id);
            Ok(())
        }
    }

    struct Reader {
        marker_id: Arc<Mutex<Option<MessageId>>>,
    }
    #[async_trait]
    impl Middleware for Reader {
        fn name(&self) -> &str {
            "Reader"
        }
        async fn after_model(
            &self,
            state: &mut dyn MiddlewareState,
            _r: &Reasoning,
        ) -> AgentResult<()> {
            let expected_id = self.marker_id.lock().unwrap().unwrap();
            let found = state.messages().iter().any(|m| m.id() == expected_id);
            assert!(found, "after_model 应能看到 before_model 写入的消息");
            Ok(())
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(Writer {
        marker_id: Arc::clone(&marker_id),
    }));
    chain.add(Box::new(Reader {
        marker_id: Arc::clone(&marker_id),
    }));

    let mut state = AgentState::new("/tmp");
    chain.run_before_model(&mut state).await.unwrap();

    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();
}

/// 验证 after_model 可接收含工具调用的 Reasoning（非空 vec![]）。
#[tokio::test]
async fn test_after_model_with_tool_calls() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    struct Inspector {
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Middleware for Inspector {
        fn name(&self) -> &str {
            "Inspector"
        }
        async fn after_model(
            &self,
            _state: &mut dyn MiddlewareState,
            r: &Reasoning,
        ) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("tool_count={}", r.tool_calls.len()));
            self.log
                .lock()
                .unwrap()
                .push(format!("has_answer={}", r.final_answer.is_some()));
            Ok(())
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(Inspector {
        log: Arc::clone(&log),
    }));

    let mut state = AgentState::new("/tmp");
    let reasoning = Reasoning {
        thought: "need to search".into(),
        final_answer: Some("final answer".into()),
        tool_calls: vec![
            ToolCall::new("tc1", "test_read".to_string(), serde_json::json!({})),
            ToolCall::new("tc2", "test_write".to_string(), serde_json::json!({})),
        ],
        source_message: None,
        usage: None,
        request_id: None,
        model: "test-model".into(),
        streamed: false,
        stop_reason: peri_model::StopReason::ToolUse,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();

    let captured = log.lock().unwrap().clone();
    assert!(captured.contains(&"tool_count=2".to_string()));
    assert!(captured.contains(&"has_answer=true".to_string()));
}

/// 验证仅覆盖旧钩子（before_tool、after_tool 等）的中间件
/// 在新钩子被调用时不报错（默认空实现）。
#[tokio::test]
async fn test_unrelated_middleware_ignores_new_hooks() {
    // OrderRecorder 仅覆盖 name()、before_tool()、after_tool()
    // 其 before_model/after_model 使用默认空实现
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(OrderRecorder::new("A", Arc::clone(&log))));
    chain.add(Box::new(OrderRecorder::new("B", Arc::clone(&log))));

    let mut state = AgentState::new("/tmp");
    // 不应报错
    chain.run_before_model(&mut state).await.unwrap();

    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();

    // 确认没有日志写入（OrderRecorder 未覆盖新钩子）
    assert!(log.lock().unwrap().is_empty());
}
