use async_trait::async_trait;
use peri_agent::prelude::*;

// ── 辅助 ──────────────────────────────────────────────────────────────────────

/// 记录调用顺序的测试中间件
struct OrderTracker {
    name: String,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl OrderTracker {
    fn new(name: &str, log: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        Self {
            name: name.to_string(),
            log,
        }
    }
}

#[async_trait]
impl Middleware for OrderTracker {
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

    async fn after_agent(
        &self,
        _state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}.after_agent", self.name));
        Ok(output.clone())
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_middleware_order() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(OrderTracker::new("first", log.clone())));
    chain.add(Box::new(OrderTracker::new("second", log.clone())));

    let mut state = AgentState::new("/test");

    chain.run_before_agent(&mut state).await.unwrap();

    let calls = log.lock().unwrap().clone();
    assert_eq!(calls, vec!["first.before_agent", "second.before_agent"]);
}

#[tokio::test]
async fn test_noop_middleware() {
    let noop = NoopMiddleware::new("test_noop");
    assert_eq!(<NoopMiddleware as Middleware>::name(&noop), "test_noop");

    let mut state = AgentState::new("/test");
    let tool_call = ToolCall::new("id1", "my_tool", serde_json::json!({"key": "val"}));

    // before_tool 应透传 tool_call
    let result = <NoopMiddleware as Middleware>::before_tool(&noop, &mut state, &tool_call)
        .await
        .unwrap();
    assert_eq!(result.name, "my_tool");
}

#[tokio::test]
async fn test_logging_middleware() {
    let mw = LoggingMiddleware::new();
    let mut state = AgentState::new("/workspace");

    // 不应 panic
    <LoggingMiddleware as Middleware>::before_agent(&mw, &mut state)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_chain_before_tool_modifies_call() {
    struct PrefixMiddleware;

    #[async_trait::async_trait]
    impl Middleware for PrefixMiddleware {
        fn name(&self) -> &str {
            "prefix"
        }

        async fn before_tool(
            &self,
            _state: &mut dyn MiddlewareState,
            tool_call: &ToolCall,
        ) -> AgentResult<ToolCall> {
            Ok(ToolCall::new(
                format!("prefixed_{}", tool_call.id),
                &tool_call.name,
                tool_call.input.clone(),
            ))
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PrefixMiddleware));

    let mut state = AgentState::new("/test");
    let call = ToolCall::new("orig", "tool", serde_json::json!({}));
    let modified = chain.run_before_tool(&mut state, call).await.unwrap();

    assert_eq!(modified.id, "prefixed_orig");
}
