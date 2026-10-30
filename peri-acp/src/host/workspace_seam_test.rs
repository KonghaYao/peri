//! ACP session task state remains local to ACP. Workspace Bash state is owned
//! by the MCP capability and verified by its Tasks wire tests.

use std::sync::Arc;

use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};

use super::workspace::SessionEnvironment;
use crate::host::assemble::{assemble_server_config, HostAssemblyInput};
use crate::provider::{ConfigSource, LlmProvider, PeriConfig, ProviderConfig, ProviderModels};

// ── 夹具 ─────────────────────────────────────────────────────────────────────

/// 生产同构的 host cfg + 规范化 cwd（`SessionEnvironment::assemble` 的
/// `same_directory` 分支要求 `canonicalize(startup_cwd) == cwd`）。
struct SeamFixture {
    cfg: crate::host::AcpServerConfig,
    cwd: String,
    _tmp: tempfile::TempDir,
}

async fn seam_fixture() -> SeamFixture {
    let tmp = tempfile::TempDir::new().unwrap();
    // macOS 上 `TempDir` 的原始路径（/var/...）与规范化路径（/private/var/...）
    // 不同：装配面的同目录判定用 `canonicalize(startup_cwd) == cwd`，故 cwd 取规范形态。
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let peri_config = make_peri_config_with_provider(make_provider_config());
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let config_source = Arc::new(
        ConfigSource::load_at(
            std::path::Path::new(&cwd),
            tmp.path().join("test_config.json"),
        )
        .expect("夹具配置源"),
    );

    let cfg = assemble_server_config(HostAssemblyInput {
        provider,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        config_source,
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_resources,
        workspace_id: None,
        session_store_shutdown: None,
        cwd: cwd.clone(),
        // bare：跳过插件/settings hooks，会话只建 workspace 池。
        // 本文件验证激活前的输入注入与关闭，初始化受 activation 闸门保护。
        bare: true,
        drive_cron_tick: false,
        workspace_input: None,
        workspace_bash_default_run_in_background: false,
        // 本夹具直接构造顶层装配：资源面输入同样保持未接线（会话路径才装载）。
        workspace_resources: None,
        // 同理：workflow 运行时槽位是会话级装配输入，本夹具无会话上下文。
        workflow_input: None,
        // 无会话上下文（测试夹具）：A24 关闭集为空集。
        builtin_closed: Default::default(),
        // 宿主技能面关闭位与关闭集同源：本夹具无会话上下文，恒为假。
        skills_face_closed: false,
        plugin_face_closed: false,
        prepared_plugins: None,
        session_mcp_servers: None,
    })
    .await;

    SeamFixture {
        cfg,
        cwd,
        _tmp: tmp,
    }
}

struct WorkflowExecutor;

#[async_trait::async_trait]
impl peri_acp_types::workflow::AgentExecutor for WorkflowExecutor {
    async fn execute(
        &self,
        _params: peri_acp_types::workflow::AgentRunParams,
    ) -> peri_acp_types::workflow::AgentRunResult {
        peri_acp_types::workflow::AgentRunResult::Ok {
            output: serde_json::json!("dispatched"),
            usage: peri_acp_types::workflow::Usage { output_tokens: 1 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

#[tokio::test]
#[serial_test::serial(mcp_env)]
async fn workflow_consumer_waits_for_instance_registration_then_dispatches() {
    let mut fixture = seam_fixture().await;
    fixture.cfg.workspace_assembly.as_mut().unwrap().bare = false;
    let sid = "workflow-delayed-init";
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, sid)
        .await
        .unwrap()
        .unwrap();
    environment
        .cfg
        .session_manager
        .ensure_session_with_task_manager(sid, &fixture.cwd, Some(environment.task_manager()));
    let pool = environment.cfg.mcp_pool.as_ref().unwrap();
    assert!(!pool.has_instance("workflow"));
    let executor: Arc<dyn peri_acp_types::workflow::AgentExecutor> = Arc::new(WorkflowExecutor);
    let (tx, _rx) = tokio::sync::broadcast::channel(8);
    let runtime = Arc::new(peri_mcp_workflow::WorkflowRuntime::new(
        executor.clone(),
        &fixture.cwd,
        tx,
        None,
    ));
    environment.bind_workflow_runtime(&runtime).unwrap();
    let start = environment.start_workflow_consumer(executor.clone());
    tokio::pin!(start);
    // 激活闸门保证连接尚未开始：旧实现会立即返回 false，确定性复现竞态。
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut start)
            .await
            .is_err(),
        "初始化中不得把暂缺句柄当成能力缺席"
    );
    environment.activate();
    assert!(start.await.unwrap());
    let original_projection = environment.workflow_projection_client_for_test().unwrap();
    assert!(environment.start_workflow_consumer(executor).await.unwrap());
    assert!(Arc::ptr_eq(
        &original_projection,
        &environment.workflow_projection_client_for_test().unwrap(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !runtime.consumer_ready() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // 就绪后必须进入参数校验，不再让两种用户调用都卡在接线错误。
    for arguments in [
        serde_json::json!({"name": "factory-dispatch-audit"}),
        serde_json::json!({"maxAgents": 1}),
    ] {
        let error = pool
            .call_instance_tool("workflow", "Workflow", arguments, None)
            .await
            .unwrap_err();
        assert!(
            error.contains("missing 'script' or 'scriptPath' field"),
            "{error}"
        );
    }
    // 真 MCP 订阅、claim 和回填；只替换模型执行边界。
    let params = serde_json::from_value(serde_json::json!({
        "runId": "delayed-run", "agentId": 1, "prompt": "dispatch"
    }))
    .unwrap();
    runtime.tasks().publish("delayed-task".into(), params);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let page = pool
                .read_instance_resource("workflow", "workflow://tasks")
                .await
                .unwrap();
            let page: serde_json::Value = serde_json::from_str(&page).unwrap();
            if page["activeTasks"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let record = runtime.tasks().get("delayed-run", "delayed-task").unwrap();
    assert_eq!(
        record.state,
        peri_mcp_workflow::WorkflowTaskState::Completed
    );
    assert!(environment.shutdown().await);
}

#[tokio::test]
async fn workflow_consumer_skips_absent_instance_after_bare_initialization() {
    let fixture = seam_fixture().await;
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, "workflow-bare")
        .await
        .unwrap()
        .unwrap();
    let executor: Arc<dyn peri_acp_types::workflow::AgentExecutor> = Arc::new(WorkflowExecutor);
    let (tx, _rx) = tokio::sync::broadcast::channel(8);
    let runtime = Arc::new(peri_mcp_workflow::WorkflowRuntime::new(
        executor.clone(),
        &fixture.cwd,
        tx,
        None,
    ));
    environment.bind_workflow_runtime(&runtime).unwrap();
    environment.activate();
    assert!(!environment.start_workflow_consumer(executor).await.unwrap());
    assert!(!runtime.consumer_bound());
    assert!(environment.shutdown().await);
}

struct WorkflowSetupPool {
    phase: &'static str,
    present: bool,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for WorkflowSetupPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn has_instance(&self, _instance: &str) -> bool {
        self.present
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"initPhase": self.phase})
    }

    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        peri_acp_types::ports::McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }
}

#[tokio::test]
async fn workflow_consumer_reports_setup_failures_without_binding_consumer() {
    let fixture = seam_fixture().await;
    let mut environment =
        SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, "workflow-failure")
            .await
            .unwrap()
            .unwrap();
    for (phase, present, expected) in [
        ("unknown", false, "unknown MCP initialization state"),
        ("ready", true, "runtime slot is not bound"),
        ("pending", false, "timed out"),
    ] {
        Arc::get_mut(&mut environment).unwrap().cfg.mcp_pool =
            Some(Arc::new(WorkflowSetupPool { phase, present }));
        let error = environment
            .start_workflow_consumer(Arc::new(WorkflowExecutor))
            .await
            .unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    // 初始化失败是终态；缺少 workflow 实例时只跳过接线，不影响会话可用性。
    Arc::get_mut(&mut environment).unwrap().cfg.mcp_pool = Some(Arc::new(WorkflowSetupPool {
        phase: "failed",
        present: false,
    }));
    assert!(!environment
        .start_workflow_consumer(Arc::new(WorkflowExecutor))
        .await
        .unwrap());
    assert!(environment.shutdown().await);
}

fn make_provider_config() -> ProviderConfig {
    ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider];
    peri_config
}

#[tokio::test]
async fn session_task_manager_is_local_to_the_acp_session() {
    let fixture = seam_fixture().await;
    let session_id = "session-task-owner";
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, session_id)
        .await
        .expect("assembly succeeds")
        .expect("workspace environment exists");
    let manager = environment.task_manager();
    environment
        .cfg
        .session_manager
        .ensure_session_with_task_manager(session_id, &fixture.cwd, Some(manager.clone()));
    let stored = &environment
        .cfg
        .session_manager
        .get_session(session_id)
        .expect("session registered")
        .task_manager;
    assert!(Arc::ptr_eq(stored, &manager));
    assert!(environment.shutdown().await);
}
