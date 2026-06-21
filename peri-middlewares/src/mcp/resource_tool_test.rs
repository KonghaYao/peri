    fn empty_pool() -> Arc<McpClientPool> {
        Arc::new(McpClientPool::new_empty())
    }

    #[test]
    fn test_name_returns_mcp_read_resource() {
        let tool = McpResourceTool::new(empty_pool());
        assert_eq!(tool.name(), "mcp_read_resource");
    }

    #[test]
    fn test_parameters_schema() {
        let tool = McpResourceTool::new(empty_pool());
        let params = tool.parameters();
        assert!(params
            .get("properties")
            .unwrap()
            .get("server_name")
            .is_some());
        assert!(params.get("properties").unwrap().get("uri").is_some());
        let required = params.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("server_name")));
        assert!(required.iter().any(|r| r.as_str() == Some("uri")));
    }

    #[test]
    fn test_description_empty_pool() {
        let tool = McpResourceTool::new(empty_pool());
        let desc = tool.description();
        assert!(desc.contains("No resources currently available"));
    }

    #[tokio::test]
    async fn test_invoke_missing_server_name() {
        let tool = McpResourceTool::new(empty_pool());
        let result = tool
            .invoke(serde_json::json!({"uri": "file:///test"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("server_name"));
    }

    #[tokio::test]
    async fn test_invoke_missing_uri() {
        let tool = McpResourceTool::new(empty_pool());
        let result = tool
            .invoke(serde_json::json!({"server_name": "test"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("uri"));
    }

    #[tokio::test]
    async fn test_invoke_server_not_found() {
        let tool = McpResourceTool::new(empty_pool());
        let result = tool
            .invoke(serde_json::json!({"server_name": "nonexistent", "uri": "test://x"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未找到"));
    }
