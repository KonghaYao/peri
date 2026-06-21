    fn new_tools() -> (CronRegisterTool, CronListTool, CronRemoveTool) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(tx)));
        (
            CronRegisterTool::new(scheduler.clone()),
            CronListTool::new(scheduler.clone()),
            CronRemoveTool::new(scheduler),
        )
    }

    #[tokio::test]
    async fn test_register_rejects_empty_prompt() {
        let (reg, _, _) = new_tools();
        let result = reg
            .invoke(serde_json::json!({"expression": "* * * * *", "prompt": ""}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        assert!(result.is_err(), "空 prompt 应被拒绝");
    }

    #[tokio::test]
    async fn test_register_rejects_whitespace_prompt() {
        let (reg, _, _) = new_tools();
        let result = reg
            .invoke(serde_json::json!({"expression": "* * * * *", "prompt": "   "}), peri_agent::tools::ToolContext::new(&[], "."))
            .await;
        assert!(result.is_err(), "纯空白 prompt 应被拒绝");
    }

    #[tokio::test]
    async fn test_register_success() {
        let (reg, list, _) = new_tools();
        let result = reg
            .invoke(serde_json::json!({"expression": "* * * * *", "prompt": "test task"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(result.contains("已注册"));

        let list_result = list.invoke(serde_json::json!({}), peri_agent::tools::ToolContext::new(&[], ".")).await.unwrap();
        assert!(list_result.contains("test task"));
    }
