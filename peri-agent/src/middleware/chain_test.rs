use super::*;
use crate::agent::state::AgentState;
use crate::middleware::r#trait::Middleware;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

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

    async fn before_agent(&self, _state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}.before_agent", self.name));
        Ok(())
    }

    async fn before_tool(
        &self,
        _state: &mut dyn hook_state::BeforeToolState,
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
        _state: &mut dyn hook_state::AfterToolState,
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
        _state: &mut dyn hook_state::BeforeToolState,
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

#[derive(Clone, Copy)]
enum ToolHookFailure {
    Middleware,
    Interrupted,
}

struct ToolHookRecorder {
    name: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
    failure: Option<ToolHookFailure>,
}

impl ToolHookRecorder {
    fn record(&self) -> AgentResult<()> {
        self.log.lock().unwrap().push(self.name);
        match self.failure {
            None => Ok(()),
            Some(ToolHookFailure::Interrupted) => Err(AgentError::Interrupted),
            Some(ToolHookFailure::Middleware) => Err(AgentError::MiddlewareError {
                middleware: self.name.into(),
                reason: "tool hook failed".into(),
            }),
        }
    }
}

#[async_trait]
impl Middleware for ToolHookRecorder {
    fn name(&self) -> &str {
        self.name
    }

    async fn before_tool(
        &self,
        _state: &mut dyn hook_state::BeforeToolState,
        call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        self.record()?;
        Ok(call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut dyn hook_state::AfterToolState,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        self.record()
    }
}

fn tool_failure_chain(
    failure: ToolHookFailure,
) -> (MiddlewareChain, Arc<Mutex<Vec<&'static str>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    for (name, failure) in [("First", None), ("Failing", Some(failure)), ("Last", None)] {
        chain.add(Box::new(ToolHookRecorder {
            name,
            failure,
            log: Arc::clone(&log),
        }));
    }
    (chain, log)
}

fn assert_tool_hook_error(error: AgentError, failure: ToolHookFailure) {
    match failure {
        ToolHookFailure::Interrupted => assert!(matches!(error, AgentError::Interrupted)),
        ToolHookFailure::Middleware => assert!(matches!(
            error,
            AgentError::MiddlewareError { middleware, reason }
                if middleware == "Failing" && reason == "tool hook failed"
        )),
    }
}

#[tokio::test]
async fn before_tool_failure_short_circuits_without_reclassifying_error() {
    for failure in [ToolHookFailure::Middleware, ToolHookFailure::Interrupted] {
        let (chain, log) = tool_failure_chain(failure);
        let mut state = AgentState::new("/tmp");
        let call = ToolCall::new("id", "tool", serde_json::json!({}));
        let error = chain.run_before_tool(&mut state, call).await.unwrap_err();
        assert_tool_hook_error(error, failure);
        assert_eq!(*log.lock().unwrap(), ["First", "Failing"]);
    }
}

#[tokio::test]
async fn after_tool_failure_short_circuits_without_reclassifying_error() {
    for failure in [ToolHookFailure::Middleware, ToolHookFailure::Interrupted] {
        let (chain, log) = tool_failure_chain(failure);
        let mut state = AgentState::new("/tmp");
        let call = ToolCall::new("id", "tool", serde_json::json!({}));
        let result = ToolResult {
            tool_call_id: "id".into(),
            tool_name: "tool".into(),
            output: "ok".into(),
            is_error: false,
            execution: None,
            effective_error_code: None,
            subagent_failure: None,
        };
        let error = chain
            .run_after_tool(&mut state, &call, &result)
            .await
            .unwrap_err();
        assert_tool_hook_error(error, failure);
        assert_eq!(*log.lock().unwrap(), ["First", "Failing"]);
    }
}

#[async_trait]
impl Middleware for FailMiddleware {
    fn name(&self) -> &str {
        "FailMiddleware"
    }

    async fn before_agent(&self, _state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
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
        execution: None,
        effective_error_code: None,
        subagent_failure: None,
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
            _state: &mut dyn hook_state::BeforeToolState,
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
            _state: &mut dyn hook_state::BeforeToolState,
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
            _state: &mut dyn hook_state::BeforeToolState,
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

#[tokio::test]
async fn test_before_tools_batch_short_result_fails_closed() {
    struct ShortBatch;
    #[async_trait]
    impl Middleware for ShortBatch {
        fn name(&self) -> &str {
            "ShortBatch"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<AgentResult<ToolCall>> {
            calls.iter().take(1).cloned().map(Ok).collect()
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ShortBatch));
    let mut state = AgentState::new("/tmp");
    let calls = vec![
        ToolCall::new("id1", "tool1", serde_json::json!({})),
        ToolCall::new("id2", "tool2", serde_json::json!({})),
    ];

    let results = chain.run_before_tools_batch(&mut state, calls).await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| matches!(
        result,
        Err(AgentError::MiddlewareError { middleware, reason })
            if middleware == "ShortBatch"
                && reason == "before_tools_batch returned 1 results for 2 calls"
    )));
}

#[tokio::test]
async fn test_before_tools_batch_long_result_preserves_rejection_and_stops_chain() {
    struct RejectFirst;
    #[async_trait]
    impl Middleware for RejectFirst {
        fn name(&self) -> &str {
            "RejectFirst"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<AgentResult<ToolCall>> {
            calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    if index == 0 {
                        Err(AgentError::ToolRejected {
                            tool: call.name.clone(),
                            reason: "denied".to_string(),
                        })
                    } else {
                        Ok(call.clone())
                    }
                })
                .collect()
        }
    }

    struct ExtraResult;
    #[async_trait]
    impl Middleware for ExtraResult {
        fn name(&self) -> &str {
            "ExtraResult"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<AgentResult<ToolCall>> {
            let mut results: Vec<_> = calls.iter().cloned().map(Ok).collect();
            results.push(Ok(calls[0].clone()));
            results
        }
    }

    struct LaterApprover(Arc<Mutex<Vec<String>>>);
    #[async_trait]
    impl Middleware for LaterApprover {
        fn name(&self) -> &str {
            "LaterApprover"
        }
        async fn before_tool(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            call: &ToolCall,
        ) -> AgentResult<ToolCall> {
            self.0.lock().unwrap().push(call.id.clone());
            Ok(call.clone())
        }
    }

    let later_calls = Arc::new(Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(RejectFirst));
    chain.add(Box::new(ExtraResult));
    chain.add(Box::new(LaterApprover(Arc::clone(&later_calls))));
    let mut state = AgentState::new("/tmp");
    let calls = vec![
        ToolCall::new("id1", "tool1", serde_json::json!({})),
        ToolCall::new("id2", "tool2", serde_json::json!({})),
    ];

    let results = chain.run_before_tools_batch(&mut state, calls).await;

    assert!(matches!(
        &results[0],
        Err(AgentError::ToolRejected { tool, reason })
            if tool == "tool1" && reason == "denied"
    ));
    assert!(matches!(
        &results[1],
        Err(AgentError::MiddlewareError { middleware, reason })
            if middleware == "ExtraResult"
                && reason == "before_tools_batch returned 2 results for 1 calls"
    ));
    assert!(later_calls.lock().unwrap().is_empty());
}
