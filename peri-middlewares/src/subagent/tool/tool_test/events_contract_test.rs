use super::*;

// ─── C2/C3：v2 SubagentStart/Stop 生产 emit 契约测试 ─────────────────────────
//
// 每条生产路径（fork 同步 / define 同步 / bg 非 fork / bg fork）各一测试：
// Start/Stop 恰好一次、字段配对、child_agent_id 为 UUID v7（= child_thread_id）。
// 捕获通道：child EventBus → forwarder observe 分支 → mock LangfuseBridgeLike
// （v1 mapper 转发已被过滤，见 peri-agent subagent_event_forwarder 测试）。

/// 记录 bridge（观测 v2 Start/Stop；必须同时装到父 host 与工具上）。
fn make_bridge() -> Arc<RecordingBridge> {
    Arc::new(RecordingBridge {
        observes: Arc::new(std::sync::Mutex::new(Vec::new())),
    })
}

/// 构造注入父身份的 SubAgentTool（parent_agent_id 已 set → emit 生效）。
fn make_tool_with_bridge(bridge: &Arc<RecordingBridge>) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
        "/tmp".to_string(),
    )
    .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
    .with_langfuse_bridge(Arc::clone(bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>)
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

/// 安装父会话：真实门面 + 已建立会话（父 id 即会话 id）+ root owner + canonical cwd。
///
/// child 保存要求父会话存在、调用 cwd 与父会话 cwd 一致、owner 存活；三者一次建好。
/// 夹具本体随返回值存活（drop 即释放 owner），调用方必须持有到 invoke 结束。
async fn install_parent_session(dir: &std::path::Path, invocation_id: &str) -> HostFixture {
    HostFixture::open_in(dir, invocation_id).await
}

/// 带父身份 + 记录 bridge 的绑定工具（durable host 提供资源/父会话/端口）。
async fn make_durable_tool(
    host: &HostFixture,
    dir: &std::path::Path,
) -> (SubAgentTool, Arc<RecordingBridge>) {
    let bridge = make_bridge();
    let t = host.bind(with_agent_face(make_tool_with_bridge(&bridge), dir).await);
    (t, bridge)
}

/// 父 host 携带记录 bridge 的 durable 宿主（host() 以父 session host 为准）。
async fn durable_host_with_bridge(
    dir: &std::path::Path,
    invocation_id: &str,
    bridge: &Arc<RecordingBridge>,
) -> HostFixture {
    let bridge = Arc::clone(bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>;
    HostFixture::open_in_with_host(dir, invocation_id, move |host| {
        host.langfuse_bridge = Some(bridge);
    })
    .await
}

/// S1/T1：fork 同步路径（execute_fork.rs）—— Start/Stop 恰好一次，
/// 且 child_agent_id == child_thread_id（C1 身份统一契约）
#[tokio::test]
async fn test_fork_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    let bridge = make_bridge();
    let host = durable_host_with_bridge(dir.path(), "fixture-events-fork", &bridge).await;
    // 会话资源门面存在时 invoke 返回携带 child_thread_id，用于身份对齐断言
    let t = host.bind(with_agent_face(make_tool_with_bridge(&bridge), dir.path()).await);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "cwd": host.cwd.clone(),
                "prompt": "fork task"
            }),
            host.context(&[]),
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
    let bridge = make_bridge();
    let host = durable_host_with_bridge(dir.path(), "fixture-events-define", &bridge).await;
    let t = host.bind(with_agent_face(make_tool_with_bridge(&bridge), dir.path()).await);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": host.cwd.clone(),
                "prompt": "do it"
            }),
            host.context(&[]),
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
    let bridge = make_bridge();
    let bridge_for_host = Arc::clone(&bridge);
    let host = HostFixture::open_in_with_background(
        dir.path(),
        "fixture-events-bg",
        Arc::clone(&registry),
        bg_tx,
    )
    .await;
    // 后台路径的 bridge 同样必须装在父 host 上；后台通道用 host 装配版本。
    let host = {
        let mut sub_host = host.parent_session_host().unwrap_or_default();
        sub_host.langfuse_bridge =
            Some(Arc::clone(&bridge_for_host) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>);
        host.with_rebuilt_host(sub_host)
    };
    let t = host.bind(with_agent_face(make_tool_with_bridge(&bridge), dir.path()).await);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "run_in_background": true,
                "cwd": host.cwd.clone(),
                "prompt": "bg task"
            }),
            host.context(&[]),
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
    let bridge = make_bridge();
    let bridge_for_host = Arc::clone(&bridge);
    let host =
        HostFixture::open_with_background("fixture-events-bg-fork", Arc::clone(&registry), bg_tx)
            .await;
    let host = {
        let mut sub_host = host.parent_session_host().unwrap_or_default();
        sub_host.langfuse_bridge =
            Some(Arc::clone(&bridge_for_host) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>);
        host.with_rebuilt_host(sub_host)
    };
    let t = host.bind(make_tool_with_bridge(&bridge).with_parent_messages(parent_messages));
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "bg fork task"
            }),
            host.context(&[]),
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
