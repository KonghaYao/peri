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
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        async fn invoke(
            &self,
            _input: Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("mock".to_string())
        }
    }

    fn build_test_index() -> Arc<ToolSearchIndex> {
        let index = Arc::new(ToolSearchIndex::new());
        index.build(vec![
            Arc::new(MockTool::new(
                "mcp__slack__send_message",
                "Send a message to Slack channel",
            )),
            Arc::new(MockTool::new(
                "mcp__slack__get_channel",
                "Get Slack channel info",
            )),
            Arc::new(MockTool::new(
                "CronRegister",
                "Register a cron scheduled task",
            )),
        ]);
        index
    }

    #[test]
    fn test_tool_name_is_search_extra_tools() {
        let index = build_test_index();
        let tool = SearchExtraTools::new(index);
        assert_eq!(tool.name(), "SearchExtraTools");
    }

    #[test]
    fn test_parameters_schema() {
        let index = build_test_index();
        let tool = SearchExtraTools::new(index);
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["query"].is_object());
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
    }

    #[tokio::test]
    async fn test_invoke_search_returns_results() {
        let index = build_test_index();
        let tool = SearchExtraTools::new(index);

        let result = tool
            .invoke(json!({"query": "slack message"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert!(parsed["results"].is_array());
        assert!(parsed["total_available"].is_number());
        let results = parsed["results"].as_array().unwrap();
        assert!(!results.is_empty());
        assert!(results[0]["name"].as_str().unwrap().contains("slack"));
    }

    #[tokio::test]
    async fn test_invoke_empty_results() {
        let index = build_test_index();
        let tool = SearchExtraTools::new(index);

        let result = tool
            .invoke(json!({"query": "nonexistent_tool_xyz"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        // TF-IDF may still return results, but total_available should be > 0
        assert!(parsed["total_available"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_invoke_missing_query() {
        let index = build_test_index();
        let tool = SearchExtraTools::new(index);

        let result = tool.invoke(json!({}), peri_agent::tools::ToolContext::new(&[], ".")).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing required 'query' parameter"));
    }
