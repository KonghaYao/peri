use super::*;

// ─── C2/C3：v2 SubagentStart/Stop 生产 emit 契约测试 ─────────────────────────
//
// 每条生产路径（fork 同步 / define 同步 / bg 非 fork / bg fork）各一测试：
// Start/Stop 恰好一次、字段配对、child_agent_id 为 UUID v7（= child_thread_id）。
// 捕获通道：child EventBus → forwarder observe 分支 → mock LangfuseBridgeLike
// （v1 mapper 转发已被过滤，见 peri-agent subagent_event_forwarder 测试）。

/// 构造注入父身份的 SubAgentTool + 记录 bridge（parent_agent_id 已 set → emit 生效）
fn make_tool_with_bridge() -> (SubAgentTool, Arc<RecordingBridge>) {
    let bridge = Arc::new(RecordingBridge {
        observes: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
    .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
    .with_langfuse_bridge(Arc::clone(&bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>);
    (t, bridge)
}

/// 轮询等待 bridge 收到 Start 与 Stop 各至少一次（forwarder 异步消费，
/// 内容事件可能先到，不能只按数量等待）
async fn wait_for_observe_start_stop(
    bridge: &Arc<RecordingBridge>,
    timeout_ms: u64,
) -> Vec<ObserveEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let evs = bridge.observes.lock().unwrap().clone();
        let has_start = evs
            .iter()
            .any(|e| matches!(e, ObserveEvent::SubagentStart { .. }));
        let has_stop = evs
            .iter()
            .any(|e| matches!(e, ObserveEvent::SubagentStop { .. }));
        if has_start && has_stop {
            return evs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 v2 SubagentStart/Stop 超时（{}ms）：当前事件：{:?}",
                timeout_ms, evs
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// 从事件流中取出 Start 事件的 child_agent_id
fn start_child_agent_id(evs: &[ObserveEvent]) -> peri_acp_types::identity::AgentId {
    evs.iter()
        .find_map(|e| match e {
            ObserveEvent::SubagentStart { child_agent_id, .. } => Some(*child_agent_id),
            _ => None,
        })
        .expect("事件流中应有 SubagentStart")
}

/// S1/T1：fork 同步路径（execute_fork.rs）—— Start/Stop 恰好一次，
/// 且 child_agent_id == child_thread_id（C1 身份统一契约）
#[tokio::test]
async fn test_fork_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    // thread_store 存在时 invoke 返回携带 child_thread_id，用于身份对齐断言
    let (t, bridge) = make_tool_with_bridge();
    let t = t.with_thread_store(Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    )) as Arc<dyn peri_agent::thread::ThreadStore>);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "fork task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "fork 应成功: {:?}", result.err());
    let result = result.unwrap();
    let child_thread_id = result
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("fork 返回值应包含 child_thread_id")
        .to_string();

    let evs = wait_for_observe_start_stop(&bridge, 3000).await;
    assert_start_stop_pair(&evs, "fork", false);
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        child_thread_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S4/T4：define 同步路径（define.rs）—— Start/Stop 恰好一次，
/// 且 child_agent_id == child_thread_id（C1 身份统一契约）
#[tokio::test]
async fn test_define_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let (t, bridge) = make_tool_with_bridge();
    let t = t.with_thread_store(Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    )) as Arc<dyn peri_agent::thread::ThreadStore>);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "do it"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "define 应成功: {:?}", result.err());
    let result = result.unwrap();
    let child_thread_id = result
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("define 返回值应包含 child_thread_id")
        .to_string();

    let evs = wait_for_observe_start_stop(&bridge, 3000).await;
    assert_start_stop_pair(&evs, "test-agent", false);
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        child_thread_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S2/T2：bg 非 fork 路径（execute_bg.rs）—— Start/Stop 恰好一次（is_background=true），
/// 且 child_agent_id == v1 SubagentStarted.instance_id（C1 身份统一契约）
#[tokio::test]
async fn test_background_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (t, bridge) = make_tool_with_bridge();
    let t = t
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "bg task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "bg 应启动成功: {:?}", result.err());

    let evs = wait_for_observe_start_stop(&bridge, 5000).await;
    assert_start_stop_pair(&evs, "test-agent", true);

    // v1 SubagentStarted.instance_id（= child_thread_id）与 v2 Start.child_agent_id 对齐
    let instance_id = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("应收到 SubagentStarted")
        .expect("通道不应关闭");
    let instance_id = match instance_id {
        ExecutorEvent::SubagentStarted { instance_id, .. } => instance_id,
        other => panic!("应为 SubagentStarted，实际 {:?}", other),
    };
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        instance_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S3/T3：bg fork 路径（spawner.rs spawn_background_fork）—— Start/Stop 恰好一次，
/// 且 child_agent_id == v1 SubagentStarted.instance_id（C1 身份统一契约）
#[tokio::test]
async fn test_bg_fork_path_emits_v2_start_stop_exactly_once() {
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> =
        Arc::new(RwLock::new(vec![BaseMessage::human("ctx for bg fork")]));
    let (t, bridge) = make_tool_with_bridge();
    let t = t
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "bg fork task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "bg fork 应启动成功: {:?}", result.err());

    let evs = wait_for_observe_start_stop(&bridge, 5000).await;
    assert_start_stop_pair(&evs, "fork", true);

    // v1 SubagentStarted.instance_id（= child_thread_id）与 v2 Start.child_agent_id 对齐
    let instance_id = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("应收到 SubagentStarted")
        .expect("通道不应关闭");
    let instance_id = match instance_id {
        ExecutorEvent::SubagentStarted { instance_id, .. } => instance_id,
        other => panic!("应为 SubagentStarted，实际 {:?}", other),
    };
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        instance_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}
