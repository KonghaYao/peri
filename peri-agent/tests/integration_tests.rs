use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_agent::prelude::*;

// ── 辅助 ──────────────────────────────────────────────────────────────────────

struct CounterTool {
    count: Arc<Mutex<usize>>,
}

impl CounterTool {
    fn new(count: Arc<Mutex<usize>>) -> Self {
        Self { count }
    }
}

#[async_trait]
impl BaseTool for CounterTool {
    fn name(&self) -> &str {
        "counter"
    }
    fn description(&self) -> &str {
        "Increments a counter"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut c = self.count.lock().unwrap();
        *c += 1;
        Ok(format!("count = {}", *c))
    }
}

struct CallRecorder {
    calls: Arc<Mutex<Vec<String>>>,
}

impl CallRecorder {
    fn new(calls: Arc<Mutex<Vec<String>>>) -> Self {
        Self { calls }
    }
}

#[async_trait]
impl Middleware<AgentState> for CallRecorder {
    fn name(&self) -> &str {
        "recorder"
    }

    async fn before_tool(
        &self,
        _state: &mut AgentState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("before:{}", tool_call.name));
        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut AgentState,
        tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("after:{}", tool_call.name));
        Ok(())
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_full_react_loop() {
    let llm = MockLLM::new(vec![
        Reasoning::with_tools(
            "step 1",
            vec![ToolCall::new("c1", "counter", serde_json::json!({}))],
        ),
        Reasoning::with_tools(
            "step 2",
            vec![ToolCall::new("c2", "counter", serde_json::json!({}))],
        ),
        Reasoning::with_answer("done", "Final: counted twice"),
    ]);

    let count = Arc::new(Mutex::new(0usize));
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));

    let agent = ReActAgent::new(llm)
        .register_tool(Box::new(CounterTool::new(count.clone())))
        .add_middleware(Box::new(CallRecorder::new(calls.clone())));

    let mut state = AgentState::new("/workspace");
    let output = agent
        .execute(AgentInput::text("count twice"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(*count.lock().unwrap(), 2);
    assert_eq!(output.tool_calls.len(), 2);
    assert_eq!(output.text, "Final: counted twice");

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            "before:counter",
            "after:counter",
            "before:counter",
            "after:counter"
        ]
    );
}

#[tokio::test]
async fn test_multiple_middlewares() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    struct Tagger {
        tag: String,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Middleware<AgentState> for Tagger {
        fn name(&self) -> &str {
            &self.tag
        }

        async fn before_agent(&self, _state: &mut AgentState) -> AgentResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:before", self.tag));
            Ok(())
        }

        async fn after_agent(
            &self,
            _state: &mut AgentState,
            output: &AgentOutput,
        ) -> AgentResult<AgentOutput> {
            self.log.lock().unwrap().push(format!("{}:after", self.tag));
            Ok(output.clone())
        }
    }

    let agent = ReActAgent::new(MockLLM::always_answer("ok"))
        .add_middleware(Box::new(Tagger {
            tag: "A".into(),
            log: log.clone(),
        }))
        .add_middleware(Box::new(Tagger {
            tag: "B".into(),
            log: log.clone(),
        }))
        .add_middleware(Box::new(Tagger {
            tag: "C".into(),
            log: log.clone(),
        }));

    let mut state = AgentState::new("/test");
    agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["A:before", "B:before", "C:before", "A:after", "B:after", "C:after"]
    );
}
