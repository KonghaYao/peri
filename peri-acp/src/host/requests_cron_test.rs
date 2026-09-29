//! Deployment → session/new → MCP → session scheduler → continuation regression.
use super::*;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_mcp_cron::CronSchedulerPortHandle;
use peri_middlewares::mcp::McpClientPool;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use std::time::Duration;

#[derive(Default)]
struct CronModel(parking_lot::Mutex<Vec<String>>);

#[async_trait]
impl Model for CronModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_streaming: true,
            ..Default::default()
        }
    }
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.0.lock().push(
            request
                .messages
                .iter()
                .map(|m| m.text_content().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let response = ModelResponse::new(
            ModelMessage::assistant_text("scheduled done"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

async fn deployment(tmp: &tempfile::TempDir, drive_cron_tick: bool) -> AcpServerConfig {
    let config = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cwd = tmp.path().canonicalize().unwrap();
    crate::host::assemble::assemble_server_config(crate::host::assemble::HostAssemblyInput {
        provider,
        peri_config: Arc::new(parking_lot::RwLock::new(config)),
        config_source: Arc::new(
            crate::provider::ConfigSource::load_at(&cwd, tmp.path().join("settings.json")).unwrap(),
        ),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_resources: peri_agent::resources::open_session_resources_with(Some(
            tmp.path().join("threads.db"),
        ))
        .await
        .unwrap(),
        session_store_shutdown: None,
        cwd: cwd.to_string_lossy().into_owned(),
        bare: false,
        drive_cron_tick,
        workspace_input: None,
        prepared_plugins: None,
    })
    .await
}

async fn cron_tool(
    env: &crate::host::workspace::SessionEnvironment,
    cwd: &str,
    name: &str,
    args: Value,
) -> String {
    let pool = env
        .cfg
        .mcp_pool
        .clone()
        .unwrap()
        .downcast_arc::<McpClientPool>()
        .unwrap_or_else(|_| panic!("MCP pool"));
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(client) = pool.get_client("cron") {
                if client.peer.is_some() && !client.tools.is_empty() {
                    break client;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cron tools ready");
    let tool = client.tools.iter().find(|t| t.name == name).unwrap();
    peri_middlewares::mcp::tool_bridge::McpToolBridge::new("cron", tool, client.clone())
        .invoke(args, ToolContext::new(&[], cwd))
        .await
        .unwrap()
}

fn scheduler(
    env: &crate::host::workspace::SessionEnvironment,
) -> Arc<parking_lot::Mutex<peri_mcp_cron::CronScheduler>> {
    env.cfg
        .cron_scheduler
        .clone()
        .unwrap()
        .downcast_arc::<CronSchedulerPortHandle>()
        .unwrap_or_else(|_| panic!("scheduler"))
        .0
        .clone()
}

async fn new_session(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cwd: &str,
) -> String {
    handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        cfg,
        sessions,
        transport,
    )
    .await
    .unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
#[serial]
async fn cron_deployment_registration_and_tick_are_session_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = Arc::new(deployment(&tmp, true).await);
    assert!(
        cfg.mcp_pool.is_none(),
        "deployment must defer MCP to session ownership"
    );
    let (cron_tx, mut cron_rx) = tokio::sync::mpsc::unbounded_channel();
    cfg.session_manager.bind_cron_continuation(cron_tx);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    let a = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    let b = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    let env_a = sessions[&a].environment.clone().unwrap();
    let env_b = sessions[&b].environment.clone().unwrap();
    let registered = cron_tool(
        &env_a,
        &cwd,
        "cron_register",
        json!({"expression":"0 0 1 1 *", "prompt":"session-a-marker"}),
    )
    .await;
    assert!(cron_tool(&env_a, &cwd, "cron_list", json!({}))
        .await
        .contains("session-a-marker"));
    assert!(!cron_tool(&env_b, &cwd, "cron_list", json!({}))
        .await
        .contains("session-a-marker"));
    let visible = env_a.cfg.cron_scheduler.as_ref().unwrap().list_tasks();
    assert_eq!(
        visible.len(),
        1,
        "MCP registration must reach the scheduler used by the session bridge: {registered}"
    );
    let task_a = visible[0].id.clone();
    assert!(env_b
        .cfg
        .cron_scheduler
        .as_ref()
        .unwrap()
        .list_tasks()
        .is_empty());
    let sched_a = scheduler(&env_a);
    let sched_b = scheduler(&env_b);
    assert!(!Arc::ptr_eq(&sched_a, &sched_b));
    assert!(sched_a.lock().force_next_fire_to_past(&task_a));
    let trigger = tokio::time::timeout(Duration::from_secs(4), cron_rx.recv())
        .await
        .expect("deployment tick must reach session bridge")
        .unwrap();
    assert_eq!(trigger.session_id, a);
    assert_eq!(trigger.trigger.task_id, task_a);
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), cron_rx.recv())
            .await
            .is_err(),
        "one due task must produce exactly one continuation, without cross-session broadcast"
    );

    // Observe the actual continuation through the production scheduler and prompt path.
    let model = Arc::new(CronModel::default());
    let fingerprint = crate::session::agent_pool::fingerprint(&env_a.cfg.provider.read().clone());
    sessions
        .get_mut(&a)
        .unwrap()
        .agent_pool
        .subagent_llm_cache
        .insert(fingerprint, model.clone());
    let shared = Arc::new(tokio::sync::Mutex::new(sessions));
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let driver = tokio::spawn(crate::host::run_cron_continuation_scheduler(
        cron_rx,
        crate::host::CronContinuationContext {
            sessions: shared.clone(),
            prompt_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cfg: cfg.clone(),
            transport: transport.clone(),
            cont_tx: Arc::new(cont_tx),
            task_spawner: cfg.host_task_spawner.clone(),
            shutdown: shutdown.clone(),
        },
    ));
    assert!(sched_a.lock().force_next_fire_to_past(&task_a));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if shared.lock().await[&a].history.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scheduled continuation must finish and commit history");
    assert_eq!(model.0.lock().len(), 1);
    assert!(model.0.lock()[0].contains("session-a-marker"));
    assert!(shared.lock().await[&b].history.is_empty());

    // Deleting a registered task prevents later ticks; closing A stops its generation.
    cron_tool(&env_a, &cwd, "cron_remove", json!({"id": task_a})).await;
    assert!(env_a
        .cfg
        .cron_scheduler
        .as_ref()
        .unwrap()
        .list_tasks()
        .is_empty());
    assert!(!sched_a.lock().force_next_fire_to_past(&task_a));
    cron_tool(
        &env_a,
        &cwd,
        "cron_register",
        json!({"expression":"0 0 1 1 *", "prompt":"closed-marker"}),
    )
    .await;
    let closed_task = env_a.cfg.cron_scheduler.as_ref().unwrap().list_tasks()[0]
        .id
        .clone();
    let mut observer = sched_a.lock().subscribe();
    handle_request(
        "session/close",
        &json!({"sessionId":a}),
        &cfg,
        &mut *shared.lock().await,
        &transport,
    )
    .await
    .unwrap();
    assert!(sched_a.lock().force_next_fire_to_past(&closed_task));
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), observer.recv())
            .await
            .is_err(),
        "closed session must stop its only tick driver"
    );
    assert_eq!(model.0.lock().len(), 1);
    // B remains usable after A closes.
    cron_tool(
        &env_b,
        &cwd,
        "cron_register",
        json!({"expression":"0 0 1 1 *", "prompt":"session-b-marker"}),
    )
    .await;
    assert_eq!(
        env_b
            .cfg
            .cron_scheduler
            .as_ref()
            .unwrap()
            .list_tasks()
            .len(),
        1
    );
    handle_request(
        "session/close",
        &json!({"sessionId":b}),
        &cfg,
        &mut *shared.lock().await,
        &transport,
    )
    .await
    .unwrap();
    shutdown.cancel();
    driver.await.unwrap();
}

#[tokio::test]
#[serial]
async fn cron_deployment_without_tick_retains_manual_schedule_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = deployment(&tmp, false).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    cfg.session_manager.bind_cron_continuation(tx);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    let id = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    let env = sessions[&id].environment.clone().unwrap();
    cron_tool(
        &env,
        &cwd,
        "cron_register",
        json!({"expression":"0 0 1 1 *", "prompt":"manual-marker"}),
    )
    .await;
    let visible = env.cfg.cron_scheduler.as_ref().unwrap().list_tasks();
    assert_eq!(visible.len(), 1);
    assert!(scheduler(&env)
        .lock()
        .force_next_fire_to_past(&visible[0].id));
    assert!(tokio::time::timeout(Duration::from_millis(1200), rx.recv())
        .await
        .is_err());
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}
