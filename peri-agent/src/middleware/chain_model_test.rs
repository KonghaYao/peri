use super::*;
use crate::agent::state::AgentState;
use crate::messages::{BaseMessage, ContentBlock, MessageId};
use crate::middleware::r#trait::{Middleware, NoopMiddleware};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

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
        async fn before_model(
            &self,
            _state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
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
        async fn before_model(
            &self,
            _state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
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
        async fn before_model(
            &self,
            _state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
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
            _state: &mut dyn hook_state::StateView,
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
        stream_interruption: None,
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
            _state: &mut dyn hook_state::StateView,
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
            _state: &mut dyn hook_state::StateView,
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
        stream_interruption: None,
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
        stream_interruption: None,
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
        stream_interruption: None,
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
        async fn before_model(
            &self,
            _state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
            self.log.lock().unwrap().push("A.before_model".into());
            Ok(())
        }
        async fn after_model(
            &self,
            _state: &mut dyn hook_state::StateView,
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
        async fn before_model(
            &self,
            _state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
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
            _state: &mut dyn hook_state::StateView,
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
        stream_interruption: None,
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
        async fn before_model(
            &self,
            state: &mut dyn hook_state::BeforeModelState,
        ) -> AgentResult<()> {
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
            state: &mut dyn hook_state::StateView,
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
        stream_interruption: None,
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
            _state: &mut dyn hook_state::StateView,
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
        stream_interruption: None,
        stop_reason: peri_model::StopReason::ToolUse,
    };
    chain.run_after_model(&mut state, &reasoning).await.unwrap();

    let captured = log.lock().unwrap().clone();
    assert!(captured.contains(&"tool_count=2".to_string()));
    assert!(captured.contains(&"has_answer=true".to_string()));
}
