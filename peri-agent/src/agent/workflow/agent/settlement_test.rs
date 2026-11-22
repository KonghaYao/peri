//! workflow child 在 `execute` 退出路径上的工作意图结算。
//!
//! 回归：child 以 `active + working` 入队初始指令，run 结束后必须收敛为终态 +
//! 不自动续跑；否则父会话每次 `session/load` 都会把它选为恢复候选。

use std::sync::Arc;

use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::{AgentStatus, ThreadMeta, WorkState};
use peri_acp_types::workflow::{AgentExecutor, AgentRunParams, AgentRunResult};

use crate::agent::workflow::agent::WorkflowAgentContext;

const PARENT: &str = "workflow-parent";
const CWD: &str = "/workflow-settle";

struct NoopFactory;

#[async_trait::async_trait]
impl crate::agent::workflow::WorkflowAgentFactory for NoopFactory {
    async fn resolve_agent_definition(
        &self,
        agent_type: &str,
        _cwd: &str,
    ) -> Result<crate::agent::workflow::WorkflowAgentDefinition, String> {
        Err(format!("unexpected agent type: {agent_type}"))
    }

    fn build_tools(
        &self,
        _cwd: &str,
        _disabled: &std::collections::HashSet<String>,
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
        _mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Vec<Box<dyn crate::tools::BaseTool>> {
        Vec::new()
    }

    fn build_middlewares(
        &self,
        _ctx: &WorkflowAgentContext,
        _model_name: &str,
        _skill_names: &[String],
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    ) -> Vec<Box<dyn crate::middleware::r#trait::Middleware>> {
        Vec::new()
    }

    fn build_tool_resolver(&self) -> Arc<dyn crate::tools::ToolInvocationResolver> {
        Arc::new(crate::tools::DirectToolInvocationResolver)
    }
}

struct DoneModel;

#[async_trait::async_trait]
impl peri_model::Model for DoneModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        let response = peri_model::ModelResponse::try_new(
            peri_model::ModelMessage::assistant_text("done"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(peri_model::ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

struct FailingModel;

#[async_trait::async_trait]
impl peri_model::Model for FailingModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        Err(peri_model::ModelError::http_status(
            400,
            "fixture",
            None::<&str>,
        ))
    }
}

/// 在模型请求内停住，直到调用方 drop execute future。
struct GatedModel {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl peri_model::Model for GatedModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        self.entered.notify_one();
        self.release.notified().await;
        let response = peri_model::ModelResponse::try_new(
            peri_model::ModelMessage::assistant_text("done"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(peri_model::ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

fn params() -> AgentRunParams {
    AgentRunParams {
        run_id: "workflow-settle-run".into(),
        agent_id: 1,
        prompt: "task".into(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    }
}

fn workflow_context(
    cwd: String,
    resources: Arc<dyn SessionResources>,
    model: Arc<dyn peri_model::Model>,
) -> WorkflowAgentContext {
    WorkflowAgentContext {
        cwd,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        mcp_skill_registry: None,
        session_id: Some(PARENT.into()),
        session_resources: Some(resources),
        compact_config: None,
        cancel: None,
        system_prompt: Some("workflow system".into()),
        external_instructions: None,
        environment_prompt: None,
        legacy_embedded_instructions: false,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        progress_tx: None,
        subagent_ctx_builder: None,
        agent_prompt_builder: Arc::new(|_, _, _, _, _| String::new()),
        model_factory: Arc::new(move |_, _, _| crate::agent::workflow::WorkflowModel {
            model: Arc::clone(&model),
            model_name: "workflow-test".into(),
            tier: None,
        }),
        middleware_factory: Arc::new(NoopFactory),
        system_prompt_fallback: Arc::new(|_, _, _, _| "fallback".into()),
        forwarder_launcher: Arc::new(|mut handles, _session_id, _on_event| {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(_) = handles.render_rx.recv() => {},
                        Some(_) = handles.state_rx.recv() => {},
                        Ok(_) = handles.observe_rx.recv() => {},
                        else => break,
                    }
                }
            })
        }),
        publish_hook: None,
        langfuse_hooks: None,
        langfuse_event_handler: None,
        meta_harness_disabled: Default::default(),
    }
}

fn child_meta(
    resources: &crate::session::test_resources::mock::MockSessionResources,
) -> ThreadMeta {
    resources
        .threads()
        .into_iter()
        .find(|meta| meta.parent_thread_id.as_deref() == Some(PARENT) && meta.hidden)
        .expect("workflow child must be persisted")
}

async fn wait_for_terminal_child(
    resources: &crate::session::test_resources::mock::MockSessionResources,
) -> ThreadMeta {
    for _ in 0..200 {
        let meta = child_meta(resources);
        if meta.agent_status.is_terminal() {
            return meta;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("workflow child was not settled after execute future was dropped");
}

#[tokio::test]
async fn completed_run_settles_child_to_done_and_idle() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    resources.register_bound_session(PARENT, CWD);
    let ctx = workflow_context(
        CWD.into(),
        resources.clone() as Arc<dyn SessionResources>,
        Arc::new(DoneModel),
    );

    let result = crate::agent::workflow::WorkflowAgentExecutor::new(ctx)
        .execute(params())
        .await;
    assert!(matches!(result, AgentRunResult::Ok { .. }), "{result:?}");

    let meta = child_meta(&resources);
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
}

/// 重试复用同一 `(run_id, agent_id)`：本次 attempt 在进入循环前失败时，
/// 宿主不得读到上一轮解析的迭代用量。
#[tokio::test]
async fn retry_without_a_loop_does_not_reuse_previous_execution_limits() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    resources.register_bound_session(PARENT, CWD);
    let ctx = workflow_context(
        CWD.into(),
        resources.clone() as Arc<dyn SessionResources>,
        Arc::new(DoneModel),
    );
    let executor = crate::agent::workflow::WorkflowAgentExecutor::new(ctx);

    let first = executor.execute(params()).await;
    assert!(matches!(first, AgentRunResult::Ok { .. }), "{first:?}");
    assert!(
        executor
            .execution_limits(&params().run_id, params().agent_id)
            .is_some(),
        "首次执行应记录解析后的上限与用量"
    );

    let mut retry = params();
    retry.agent_type = Some("missing-agent-type".into());
    let second = executor.execute(retry).await;
    assert!(matches!(second, AgentRunResult::Dead { .. }), "{second:?}");
    assert!(
        executor
            .execution_limits(&params().run_id, params().agent_id)
            .is_none(),
        "尚未进入循环的重试不得继承上一轮用量"
    );
}

#[tokio::test]
async fn failed_run_settles_child_to_error_and_idle() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    resources.register_bound_session(PARENT, CWD);
    let ctx = workflow_context(
        CWD.into(),
        resources.clone() as Arc<dyn SessionResources>,
        Arc::new(FailingModel),
    );

    let result = crate::agent::workflow::WorkflowAgentExecutor::new(ctx)
        .execute(params())
        .await;
    assert!(matches!(result, AgentRunResult::Dead { .. }), "{result:?}");

    let meta = child_meta(&resources);
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
}

/// workflow kill / 会话关闭通过 drop execute future 取消执行；此时无法 await，
/// 由结算守卫兜底写 Cancelled + Stopped，child 不再成为父会话的续跑候选。
#[tokio::test]
async fn dropped_run_settles_child_to_cancelled_and_stopped() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    resources.register_bound_session(PARENT, CWD);
    let entered = Arc::new(tokio::sync::Notify::new());
    let ctx = workflow_context(
        CWD.into(),
        resources.clone() as Arc<dyn SessionResources>,
        Arc::new(GatedModel {
            entered: Arc::clone(&entered),
            release: Arc::new(tokio::sync::Notify::new()),
        }),
    );
    let executor = crate::agent::workflow::WorkflowAgentExecutor::new(ctx);
    let handle = tokio::spawn(async move { executor.execute(params()).await });

    entered.notified().await;
    handle.abort();
    let _ = handle.await;

    let meta = wait_for_terminal_child(&resources).await;
    assert_eq!(meta.agent_status, AgentStatus::Cancelled);
    assert_eq!(meta.work_state, WorkState::Stopped);
}

#[cfg(not(target_os = "emscripten"))]
mod sqlite_recovery {
    use std::path::Path;
    use std::sync::Arc;

    use peri_acp_types::session_resources::{
        FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    };
    use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
    use peri_acp_types::workflow::{AgentExecutor, AgentRunResult};
    use peri_acp_types::workspace::SessionBinding;
    use peri_resources::sessions::SessionResourcesImpl;
    use tempfile::TempDir;

    use crate::agent::workflow::agent::settlement_tests::params;
    use crate::agent::workflow::agent::settlement_tests::workflow_context;
    use crate::agent::workflow::agent::settlement_tests::DoneModel;
    use crate::agent::workflow::agent::settlement_tests::PARENT;

    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("the output fixture result is required for this assertion");
        assert!(
            output.status.success(),
            "Git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> TempDir {
        let directory =
            tempfile::tempdir().expect("the temporary test directory must be available");
        git(directory.path(), &["init", "-q"]);
        git(
            directory.path(),
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        );
        directory
    }

    /// 真实 SQLite：完成后的 workflow child 既不是 `working` 候选，也不是需要
    /// 收敛的遗留 `Active` child；父会话进入时零候选、零失败准备。
    #[tokio::test]
    async fn completed_workflow_child_is_not_a_recovery_candidate() {
        let repo = repository();
        let db = tempfile::tempdir().expect("the temporary test directory must be available");
        let resources = Arc::new(
            SessionResourcesImpl::open(db.path().join("threads.db"))
                .await
                .expect("the temporary session store fixture must open successfully"),
        );
        let workspace = resources
            .resolve_workspace(repo.path())
            .await
            .expect("the resolve workspace fixture result is required for this assertion");
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        resources
            .create_session(&NewSession {
                thread_id: PARENT.to_owned(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: Some("workflow parent".into()),
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new(r#"{"v":1,"fixture":true}"#),
            })
            .await
            .expect("the create session fixture result is required for this assertion");

        let ctx = workflow_context(
            cwd,
            resources.clone() as Arc<dyn SessionResources>,
            Arc::new(DoneModel),
        );
        let result = crate::agent::workflow::WorkflowAgentExecutor::new(ctx)
            .execute(params())
            .await;
        assert!(matches!(result, AgentRunResult::Ok { .. }), "{result:?}");

        let parent = ThreadId::from(PARENT.to_owned());
        let tree = resources
            .list_session_tree(&parent)
            .await
            .expect("the list session tree fixture result is required for this assertion");
        let child = tree
            .iter()
            .find(|meta| meta.parent_thread_id.as_deref() == Some(PARENT))
            .expect("workflow child must be persisted");
        assert_eq!(child.agent_status, AgentStatus::Done);
        assert_eq!(child.work_state, WorkState::Idle);

        let scope = resources
            .load_recovery_scope(&parent)
            .await
            .expect("the load recovery scope fixture result is required for this assertion");
        assert!(
            scope.candidates.is_empty(),
            "settled workflow child must not be a recovery candidate: {:?}",
            scope.candidates
        );
        let interrupted = resources
            .load_interrupted_threads(&parent)
            .await
            .expect("the load interrupted threads fixture result is required for this assertion");
        assert!(
            interrupted.is_empty(),
            "settled workflow child must not need Active convergence: {interrupted:?}"
        );
    }
}
