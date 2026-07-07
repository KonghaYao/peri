    fn make_registry() -> (
        BackgroundTaskRegistry,
        tokio::sync::mpsc::UnboundedReceiver<BackgroundTaskResult>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (BackgroundTaskRegistry::new(tx), rx)
    }

    fn make_task(id: &str) -> BackgroundTask {
        let handle = tokio::runtime::Handle::current().spawn(async {});
        BackgroundTask {
            id: id.to_string(),
            agent_name: "test-agent".to_string(),
            prompt_summary: "test task".to_string(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Abort(handle.abort_handle()),
            pid: None,
            output_preview: None,
        }
    }

    #[tokio::test]
    async fn test_register_and_active_count() {
        let (registry, _rx) = make_registry();
        assert_eq!(registry.active_count(), 0);

        registry.register(make_task("bg-1")).unwrap();
        assert_eq!(registry.active_count(), 1);
    }

    #[tokio::test]
    async fn test_max_concurrent_limit() {
        let (registry, _rx) = make_registry();

        registry.register(make_task("bg-1")).unwrap();
        registry.register(make_task("bg-2")).unwrap();
        registry.register(make_task("bg-3")).unwrap();

        let result = registry.register(make_task("bg-4"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum 3"));
    }

    #[tokio::test]
    async fn test_complete_sends_notification() {
        let (registry, mut rx) = make_registry();

        registry.register(make_task("bg-1")).unwrap();
        assert_eq!(registry.active_count(), 1);

        let result = BackgroundTaskResult {
            task_id: "bg-1".to_string(),
            agent_name: "test-agent".to_string(),
            prompt_summary: "test".to_string(),
            success: true,
            output: "done".to_string(),
            tool_calls_count: 2,
            duration_ms: 100,
            child_thread_id: None,
        };

        registry.complete("bg-1", result);

        // 已完成任务应被立即清理，list_tasks 不再返回
        let tasks = registry.list_tasks();
        assert_eq!(tasks.len(), 0, "completed tasks should be cleaned up immediately");
        assert_eq!(registry.active_count(), 0);

        // 通知应已发送
        let received = rx.try_recv().unwrap();
        assert_eq!(received.task_id, "bg-1");
        assert!(received.success);
    }

    #[tokio::test]
    async fn test_cancel_removes_task() {
        let (registry, _rx) = make_registry();

        registry.register(make_task("bg-1")).unwrap();
        registry.register(make_task("bg-2")).unwrap();

        registry.cancel("bg-1").unwrap();
        let tasks = registry.list_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].0, "bg-2");

        // 取消不存在的任务返回 Err
        let result = registry.cancel("nonexistent");
        assert!(result.is_err());
    }

    /// Cancel 传播到执行中的 Background 任务：阻塞的 JoinHandle 被 abort 后任务终止。
    /// 验证 abort_handle.abort() 真正触发了 JoinHandle 的取消，而非仅从 registry 移除条目。
    #[tokio::test]
    async fn test_cancel_propagates_to_running_task() {
        let (registry, _rx) = make_registry();

        // 构造一个会长时间阻塞的 JoinHandle（等待 oneshot，永不 resolve）
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            // 阻塞等待永不触发的 oneshot，模拟执行中的 SubAgent
            let _ = rx.await;
        });

        let task = BackgroundTask {
            id: "bg-running".to_string(),
            agent_name: "blocking-agent".to_string(),
            prompt_summary: "blocking test".to_string(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Abort(handle.abort_handle()),
            pid: None,
            output_preview: None,
        };

        registry.register(task).unwrap();
        assert_eq!(registry.active_count(), 1);

        // 取消任务：应 abort JoinHandle 并从 registry 移除
        registry.cancel("bg-running").unwrap();

        // 验证 registry 中已清理
        let tasks = registry.list_tasks();
        assert!(
            tasks.is_empty(),
            "cancel 后任务应从 registry 移除，实际: {}",
            tasks.len()
        );
        assert_eq!(
            registry.active_count(),
            0,
            "cancel 后 active_count 应为 0"
        );

        // 清理：让 oneshot sender 释放，避免 JoinHandle 泄漏
        drop(tx);
    }

    // ── 新增：per-kind 上限测试 ──

    #[tokio::test]
    async fn test_count_by_kind_works() {
        let (registry, _rx) = make_registry();

        let mut shell_task = make_task("bg-shell-1");
        shell_task.kind = BgTaskKind::Shell;
        shell_task.id = "bg-shell-1".to_string();
        registry.register(shell_task).unwrap();

        let mut agent_task = make_task("bg-agent-1");
        agent_task.kind = BgTaskKind::Agent;
        agent_task.id = "bg-agent-1".to_string();
        registry.register(agent_task).unwrap();

        assert_eq!(registry.count_by_kind(BgTaskKind::Shell), 1);
        assert_eq!(registry.count_by_kind(BgTaskKind::Agent), 1);
        assert_eq!(registry.count_by_kind(BgTaskKind::Workflow), 0);
    }

    #[tokio::test]
    async fn test_register_with_kind_shell_limit() {
        let (registry, _rx) = make_registry();

        for i in 0..5 {
            let mut task = make_task(&format!("bg-shell-{}", i));
            task.kind = BgTaskKind::Shell;
            registry.register_with_kind(task).unwrap();
        }

        // 第 6 个应被拒绝
        let mut task = make_task("bg-shell-over");
        task.kind = BgTaskKind::Shell;
        let result = registry.register_with_kind(task);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("已达 shell 并发上限"));
    }

    #[tokio::test]
    async fn test_register_with_kind_agent_limit() {
        let (registry, _rx) = make_registry();

        for i in 0..3 {
            let mut task = make_task(&format!("bg-agent-{}", i));
            task.kind = BgTaskKind::Agent;
            registry.register_with_kind(task).unwrap();
        }

        let mut task = make_task("bg-agent-over");
        task.kind = BgTaskKind::Agent;
        let result = registry.register_with_kind(task);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("已达 agent 并发上限"));
    }

    #[tokio::test]
    async fn test_list_tasks_full_returns_info() {
        let (registry, _rx) = make_registry();

        let mut task = make_task("bg-agent-1");
        task.kind = BgTaskKind::Agent;
        registry.register(task).unwrap();

        let tasks = registry.list_tasks_full();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "bg-agent-1");
        assert_eq!(tasks[0].kind, BgTaskKind::Agent);
    }
