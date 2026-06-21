    struct MockTool {
        name_str: String,
        desc_str: String,
    }

    impl MockTool {
        fn new(name: &str, desc: &str) -> Self {
            Self {
                name_str: name.to_string(),
                desc_str: desc.to_string(),
            }
        }
    }

    #[async_trait]
    impl BaseTool for MockTool {
        fn name(&self) -> &str {
            &self.name_str
        }
        fn description(&self) -> &str {
            &self.desc_str
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("mock".to_string())
        }
    }

    fn build_test_components() -> (
        Arc<ToolSearchIndex>,
        Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    ) {
        let index = Arc::new(ToolSearchIndex::new());
        index.build(vec![
            Arc::new(MockTool::new("CronRegister", "Register a cron task")),
            Arc::new(MockTool::new("mcp__slack__send", "Send Slack message")),
        ]);

        let mut shared = HashMap::new();
        shared.insert(
            "CronRegister".to_string(),
            Arc::new(MockTool::new("CronRegister", "Register a cron task")) as Arc<dyn BaseTool>,
        );
        shared.insert(
            "mcp__slack__send".to_string(),
            Arc::new(MockTool::new("mcp__slack__send", "Send Slack message")) as Arc<dyn BaseTool>,
        );

        (index, Arc::new(RwLock::new(shared)))
    }

    #[test]
    fn test_collect_tools_returns_meta_tools() {
        let (index, shared) = build_test_components();
        let mw = ToolSearchMiddleware::new(index, shared);
        let tools = <ToolSearchMiddleware as Middleware<
            peri_agent::agent::state::AgentState,
        >>::collect_tools(&mw, "/tmp");

        assert!(tools.len() >= 3, "expected at least 3 tools (meta + deferred)");
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"SearchExtraTools"));
        assert!(names.contains(&"ExecuteExtraTool"));
        assert!(names.contains(&"artifact"), "expected artifact tool");
    }

    #[tokio::test]
    async fn test_before_agent_injects_system_prompt() {
        let (index, shared) = build_test_components();
        let mw = ToolSearchMiddleware::new(index, shared);

        let mut state = peri_agent::agent::state::AgentState::new("/tmp");
        mw.before_agent(&mut state).await.unwrap();

        let messages = state.messages();
        assert!(!messages.is_empty(), "before_agent 应注入 system 消息");
        let first = messages.first().unwrap();
        assert!(
            matches!(first, BaseMessage::System { .. }),
            "第一条消息应为 System"
        );
        assert!(
            first.content().contains("CronRegister"),
            "system 消息应包含延迟工具列表"
        );
    }

    #[tokio::test]
    async fn test_second_before_agent_injects_same_cached_prompt() {
        let (index, shared) = build_test_components();
        let mw = ToolSearchMiddleware::new(index, shared);

        let mut state1 = peri_agent::agent::state::AgentState::new("/tmp");
        mw.before_agent(&mut state1).await.unwrap();
        let first_content = state1.messages()[0].content().to_string();

        let mut state2 = peri_agent::agent::state::AgentState::new("/tmp");
        mw.before_agent(&mut state2).await.unwrap();
        assert_eq!(
            state2.messages().len(),
            1,
            "每轮都应注入 system 消息（System 消息被过滤后需重新注入）"
        );
        assert_eq!(
            state2.messages()[0].content(),
            first_content,
            "第二轮注入的内容应与首轮完全一致（缓存）"
        );
    }
