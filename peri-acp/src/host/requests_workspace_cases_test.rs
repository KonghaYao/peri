use crate::host::requests::tests::MockTransport;
use crate::host::SessionState;
use crate::provider::LlmProvider;
use async_trait::async_trait;

use peri_acp_types::workspace::SessionBinding;
use peri_middlewares::permission::shared_mode::PermissionMode;
use peri_resources::sessions::SqliteThreadStore;
use serde_json::json;
use serde_json::Value;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[path = "requests_workspace_assembly_test.rs"]
mod assembly_cases;

#[path = "requests_workspace_storage_v2_test.rs"]
mod storage_v2_cases;

#[path = "requests_workspace_tasks_test.rs"]
mod task_cases;

#[tokio::test]
async fn worktree_binding_hot_cold_resume_ignore_caller_cwd_and_keep_saved_facts() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("main");
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&repo).unwrap();
    let run_git = |args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init".as_ref(), "--quiet".as_ref()]);
    run_git(&[
        "-c".as_ref(),
        "user.name=Test".as_ref(),
        "-c".as_ref(),
        "user.email=test@example.invalid".as_ref(),
        "commit".as_ref(),
        "--allow-empty".as_ref(),
        "-m".as_ref(),
        "initial".as_ref(),
        "--quiet".as_ref(),
    ]);
    run_git(&[
        "worktree".as_ref(),
        "add".as_ref(),
        "--detach".as_ref(),
        linked.as_os_str(),
    ]);
    let sub = repo.join("src");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("CLAUDE.md"), "SOURCE_WORKTREE_FROZEN_SENTINEL").unwrap();
    let provider_config =
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model");
    let peri_config = crate::host::requests::tests::make_peri_config_with_provider(provider_config);
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = crate::host::requests::tests::make_server_config(
        peri_config.clone(),
        provider.clone(),
        &tmp,
    )
    .await;
    let second =
        crate::host::requests::tests::make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": sub}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let original_cwd = std::fs::canonicalize(&sub).unwrap();
    assert_eq!(sessions[&id].cwd, original_cwd.to_str().unwrap());
    assert_eq!(
        created["_meta"]["peri.sessionWorkspaceV1"]["workspace"]["cwd"],
        original_cwd.to_str().unwrap()
    );
    let frozen = crate::host::requests::tests::frozen_snapshot_bytes(&cfg, &id).await;
    crate::host::requests::tests::append_human_message(&cfg, &id, "visible project session").await;
    for method in ["session/load", "session/resume"] {
        crate::host::requests::handle_request(
            method,
            &json!({"sessionId": id, "cwd": linked}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        assert_eq!(sessions[&id].cwd, original_cwd.to_str().unwrap());
        assert_eq!(
            crate::host::requests::tests::frozen_snapshot_bytes(&cfg, &id).await,
            frozen
        );
    }
    let project = crate::host::requests::handle_request(
        "peri/session_context",
        &json!({"cwd": linked}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let list = crate::host::requests::handle_request("session/list", &json!({"_meta":{"peri.sessionWorkspaceV1":{"scope":{"kind":"project","value":project["workspace"]["project_id"]}}}}), &cfg, &mut sessions, &transport).await.unwrap();
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    let exact = crate::host::requests::handle_request(
        "session/list",
        &json!({"cwd": linked}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(exact["sessions"].as_array().unwrap().is_empty());
    let forked = crate::host::requests::handle_request(
        "session/fork",
        &json!({"sessionId": id, "cwd": linked}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fork_id = forked["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(sessions[&fork_id].cwd, original_cwd.to_str().unwrap());
    assert_eq!(
        cfg.session_resources
            .load_session_binding(&fork_id)
            .await
            .unwrap(),
        cfg.session_resources
            .load_session_binding(&id)
            .await
            .unwrap()
    );
    assert_eq!(
        crate::host::requests::tests::frozen_snapshot_bytes(&cfg, &fork_id).await,
        frozen
    );
    assert_eq!(
        crate::host::requests::tests::own_payloads(&cfg, &fork_id)
            .await
            .len(),
        1
    );
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": fork_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    // 先排空原宿主的任务，再在冷宿主恢复同一会话。
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let mut cold = HashMap::new();
    let loaded = crate::host::requests::handle_request(
        "session/load",
        &json!({"sessionId": id, "cwd": sub}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
    assert!(loaded["_meta"]["peri.sessionWorkspaceV1"]["read_only"].is_null());
    assert_eq!(cold[&id].cwd, original_cwd.to_str().unwrap());
    std::fs::write(sub.join("CLAUDE.md"), "MUTATED_AFTER_CLOSE").unwrap();
    crate::host::requests::handle_request(
        "session/resume",
        &json!({"sessionId": id}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(cold[&id].cwd, original_cwd.to_str().unwrap());
    assert_eq!(
        crate::host::requests::tests::frozen_snapshot_bytes(&second, &id).await,
        frozen
    );
    assert!(cold[&id]
        .frozen
        .as_ref()
        .unwrap()
        .v2_frozen()
        .environment_prompt
        .as_deref()
        .expect("restored frozen environment")
        .contains(original_cwd.to_str().unwrap()));
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn worktree_missing_directory_keeps_history_but_rejects_execution() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    crate::host::requests::tests::append_human_message(&cfg, id, "saved history").await;
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    std::fs::remove_dir(&cwd).unwrap();
    let history = crate::host::requests::handle_request(
        "peri/session_history",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(history["payloads"].as_array().unwrap().len(), 1);
    assert!(sessions.is_empty());
    for method in ["session/load", "session/resume"] {
        assert!(crate::host::requests::handle_request(
            method,
            &json!({"sessionId": id}),
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .is_err());
        assert!(sessions.is_empty());
        assert!(cfg.session_manager.get_session(id).is_none());
    }
}

/// 断言「资源取自 linked worktree 目标目录」⇒ 读的是 builtin `workspace` 资源面，
/// 依赖注入**默认态**（`PERI_MCP_BUILTIN` 非 `off`）。该 env 是进程级全局且由同进程
/// 开关组用例在 `#[serial]` 临界区内改写（TEST-HERMETIC-001）⇒ 读侧必须同键互斥。
#[tokio::test]
#[serial]
async fn worktree_new_resources_use_the_target_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::write(startup.join("CLAUDE.md"), "STARTUP_FROZEN_CONFIGURATION").unwrap();
    std::fs::write(target.join("CLAUDE.md"), "TARGET_FROZEN_CONFIGURATION").unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg =
        crate::host::requests::tests::make_server_config(config.clone(), provider, &tmp).await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let state = &sessions[id];
    let environment = state.environment.as_ref().unwrap();
    assert!(state
        .frozen
        .as_ref()
        .unwrap()
        .v2_frozen()
        .claude_md
        .contains("TARGET_FROZEN_CONFIGURATION"));
    assert!(!state
        .frozen
        .as_ref()
        .unwrap()
        .v2_frozen()
        .claude_md
        .contains("STARTUP_FROZEN_CONFIGURATION"));
    assert_eq!(
        environment.cfg.session_manager.get_session(id).unwrap().cwd,
        state.cwd
    );
    let pool = environment
        .cfg
        .mcp_pool
        .clone()
        .expect("bare 会话仍提供 builtin workspace")
        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .unwrap_or_else(|_| panic!("workspace MCP pool"));
    let workspace = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(client) = pool.client("workspace") {
                if client.peer.is_some() && !client.resources.is_empty() {
                    break client;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bare workspace 必须完成初始化");
    assert!(workspace.resources.iter().any(|resource| {
        resource.uri == peri_acp_types::workspace_resources::INSTRUCTION_MAIN_URI
    }));
    let (main, _) = pool.read_builtin_workspace_instructions().await.unwrap();
    let main = main.expect("target workspace instruction resource must be readable");
    assert!(main.contains("TARGET_FROZEN_CONFIGURATION"));
    assert!(!main.contains("STARTUP_FROZEN_CONFIGURATION"));
    assert!(environment.cfg.hook_groups.is_empty());
    assert_eq!(cfg.session_manager.get_session(id).unwrap().cwd, state.cwd);
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

/// A resource that keeps closing until its external cleanup is acknowledged.
struct RetryShutdownPool {
    settled: AtomicBool,
    called: AtomicBool,
}

#[async_trait]
impl peri_acp_types::ports::McpPoolPort for RetryShutdownPool {
    async fn verify_shared_environment_close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        self.called.store(true, Ordering::Release);
        if self.settled.load(Ordering::Acquire) {
            peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 1,
                failed_services: 0,
            }
        } else {
            peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
                settled_services: 0,
                unfinished_services: 1,
                failed_services: 0,
            }
        }
    }

    fn snapshot(&self) -> Value {
        json!({})
    }
}

#[tokio::test]
async fn worktree_scheduled_approval_uses_session_permission_and_rejects_closed_owner() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    sessions[id]
        .environment
        .as_ref()
        .unwrap()
        .cfg
        .permission_mode
        .store(PermissionMode::Default);
    assert_eq!(cfg.permission_mode.load(), PermissionMode::Bypass);
    let sessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let permission = crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, id)
        .await
        .unwrap();
    assert_eq!(permission.load(), PermissionMode::Default);
    assert!(
        !crate::host::prompt::approve_scheduled_trigger(
            permission.as_ref(),
            None,
            peri_acp_types::interaction::TrustedInteractionOwner::host(
                crate::host::SCHEDULED_TRIGGER_NAMESPACE,
                1,
            ),
            "task",
            "run",
        )
        .await
    );
    sessions.lock().await.get_mut(id).unwrap().closing = true;
    assert!(
        crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, id)
            .await
            .is_err()
    );
    assert!(
        crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, "missing")
            .await
            .is_err()
    );
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut *sessions.lock().await,
        &transport,
    )
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn close_premarked_for_admission_persists_intent_before_settlement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    sessions.get_mut(&id).unwrap().closing = true;
    assert!(!cfg.session_resources.is_session_closing(&id).await.unwrap());

    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(!sessions.contains_key(&id));
    assert!(!cfg.session_resources.is_session_closing(&id).await.unwrap());
}

include!("requests_workspace_cases_test_part2_test.rs");
