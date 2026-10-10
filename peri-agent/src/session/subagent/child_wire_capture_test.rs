//! H1/H2 子侧最终请求（**Agent factory / bridge 层**）验收。
//!
//! 覆盖范围：`SubagentSpawnConfig` → `build_subagent_session_v2` →
//! `AgentModelBridge`（生产装配函数与真实 bridge 组合），用 mock `Model`
//! 捕获真实 `ModelRequest`，断言最终请求面（system + messages）：
//! 子身份（子能力投影）恰一次、不含父冻结字节；贡献（`before_agent` 后填充）
//! 恰一次并经统一组合权威合并；定义型 / fork / live resume 共用同一装配点。
//!
//! **不覆盖**（不得据此宣称 H2 全完成）：ACP 端到端与真实 middlewares 生产
//! 链（`build_subagent_middlewares` / AgentsMd / Skills / ToolSearch + 能力
//! 投影）、后台路径的注册/收尾与冷恢复。本文件的
//! `LateChildContribution` 是 Agent 层的模拟中间件，只证明请求时读取与组合
//! 语义，不代表真实生产链已验收。

use super::*;
use std::sync::Mutex;

/// 捕获请求的 mock Model（生产 bridge 直接消费它）。
struct CaptureModel {
    requests: Mutex<Vec<peri_model::ModelRequest>>,
    answer: String,
}

impl CaptureModel {
    fn new(answer: &str) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            answer: answer.to_string(),
        })
    }

    fn last_system(&self) -> String {
        let requests = self.requests.lock().unwrap();
        let Some(request) = requests.last() else {
            return String::new();
        };
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                peri_model::ModelMessage::System { content } => Some(
                    content
                        .iter()
                        .filter_map(|block| match block {
                            peri_model::ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn conversation_text(&self) -> String {
        let requests = self.requests.lock().unwrap();
        let Some(request) = requests.last() else {
            return String::new();
        };
        request
            .messages
            .iter()
            .filter(|message| !matches!(message, peri_model::ModelMessage::System { .. }))
            .map(|message| match message {
                peri_model::ModelMessage::User { content }
                | peri_model::ModelMessage::Assistant { content, .. } => content
                    .iter()
                    .filter_map(|block| match block {
                        peri_model::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait::async_trait]
impl peri_model::Model for CaptureModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_streaming: true,
            ..peri_model::ModelCapabilities::default()
        }
    }

    /// v2 stages 走 durable prepared 路径（bridge 的 `prepare_reasoning` →
    /// `Model::prepare_stream`）；mock 必须提供该入口，否则是 provider
    /// 能力缺失而不是断言失败。checkpoint 冻结本次请求全体（测试据此与
    /// `stream` 路径共用同一捕获面）。
    fn prepare_stream(
        &self,
        request: peri_model::ModelRequest,
    ) -> peri_model::ModelResult<peri_model::PreparedModelCall> {
        self.requests.lock().unwrap().push(request.clone());
        let answer = self.answer.clone();
        let checkpoint = serde_json::json!({
            "provider": "fixture-capture",
            "model": "capture-model",
            "endpoint": "https://fixture.invalid/messages",
            "credentialRef": "fixture:no-credentials",
            "body": request,
        });
        Ok(peri_model::PreparedModelCall::new(
            checkpoint,
            move |cancellation| {
                let response = peri_model::ModelResponse::new(
                    peri_model::ModelMessage::assistant_text(answer),
                    peri_model::StopReason::EndTurn,
                    None,
                    None,
                )?;
                Ok(peri_model::ModelStream::with_parent_cancellation(
                    futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(
                        response,
                    ))]),
                    cancellation,
                ))
            },
        ))
    }

    async fn stream(
        &self,
        request: peri_model::ModelRequest,
        cancellation: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        self.requests.lock().unwrap().push(request);
        let response = peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text(self.answer.clone()),
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

/// 子链贡献夹具：`before_agent` 之后才写入（模拟 Skills / ToolSearch 的真实
/// 时序）——bridge 必须在请求时读取，而不是装配前拍快照。
struct LateChildContribution {
    contribution: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl crate::middleware::r#trait::Middleware for LateChildContribution {
    fn name(&self) -> &str {
        "LateChildContribution"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.contribution.lock().unwrap().clone()
    }

    async fn before_agent(
        &self,
        _state: &mut dyn crate::middleware::capabilities::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        let mut slot = self.contribution.lock().unwrap();
        if slot.is_none() {
            *slot = Some("CHILD_SKILLS_AND_DEFERRED".to_string());
        }
        Ok(())
    }
}

/// 生产形态子链装配器：一条链同时持有贡献中间件与（可选）工具。
struct ContributionAssembler {
    contribution: Arc<Mutex<Option<String>>>,
}

impl SubagentChainAssembler for ContributionAssembler {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        let mut chain = MiddlewareChain::new();
        chain.add(Box::new(LateChildContribution {
            contribution: Arc::clone(&self.contribution),
        }));
        chain
    }
}

fn child_config(
    store: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    model: Arc<CaptureModel>,
    assembler: Arc<dyn SubagentChainAssembler>,
    fork: bool,
    cwd: &str,
) -> SubagentSpawnConfig {
    SubagentSpawnConfig {
        agent_name: "wire-child".into(),
        prompt: "child prompt".into(),
        parent_messages: if fork {
            vec![BaseMessage::human("parent turn")]
        } else {
            Vec::new()
        },
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 3,
        fork_directive_kind: fork.then_some(ForkDirectiveKind::Fork),
        run_mode: SubagentRunMode::Sync,
        skill_names: vec![],
        // H1：生产模型来源（bridge 在子链装配点构造）。
        llm: SubagentLlmSource::model(
            Arc::clone(&model) as Arc<dyn peri_model::Model>,
            "capture-model",
        ),
        chain_assembler: assembler,
        tools: vec![],
        tool_filter: Arc::new(|_| true),
        system_prompt: Some("CHILD_IDENTITY_SENTINEL".into()),
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        parent_tool_call_id: None,
        cancel_token: None,
        cwd: Some(cwd.into()),
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

async fn bound_parent(
    db_name: &str,
) -> (
    Arc<dyn peri_acp_types::session_resources::SessionResources>,
    Arc<Session>,
    String,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let repo = crate::session::test_resources::git_repository();
    let db = tempfile::tempdir().unwrap();
    let resources = peri_resources::Resources::open_with(Some(db.path().join(db_name)))
        .await
        .unwrap();
    let (store, _shutdown) = resources.into_parts();
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let parent_id = create_bound_root(&store, &workspace, None).await;
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(parent_id),
    );
    (store, parent, cwd, repo, db)
}

/// 定义型 spawn：身份恰一次（bridge base），贡献在 `before_agent` 后进入同一
/// 请求；messages 不重复身份。
#[tokio::test]
async fn defined_child_request_carries_identity_and_contributions_exactly_once() {
    let (store, parent, cwd, _repo, _db) = bound_parent("child-wire-defined.db").await;
    let model = CaptureModel::new("child done");
    let contribution = Arc::new(Mutex::new(None));
    let config = child_config(
        store,
        Arc::clone(&model),
        Arc::new(ContributionAssembler {
            contribution: Arc::clone(&contribution),
        }),
        false,
        &cwd,
    );
    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();
    assert!(!spawned.interrupted);

    let system = model.last_system();
    let expected = format!(
        "CHILD_IDENTITY_SENTINEL{}\n\nCHILD_SKILLS_AND_DEFERRED",
        peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
    );
    assert_eq!(
        system, expected,
        "生产子链最终 system 应为身份 + 统一边界 + 请求时贡献"
    );
    assert_eq!(
        system.matches("CHILD_IDENTITY_SENTINEL").count(),
        1,
        "身份恰一次: {system}"
    );
    assert_eq!(
        system
            .matches(peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .count(),
        1,
        "boundary marker 恰一个: {system}"
    );
    assert_eq!(
        system.matches("CHILD_SKILLS_AND_DEFERRED").count(),
        1,
        "贡献恰一次: {system}"
    );
    let conversation = model.conversation_text();
    assert!(
        !conversation.contains("CHILD_IDENTITY_SENTINEL"),
        "身份不得再写入 transcript/对话体: {conversation}"
    );
}

/// fork spawn：父消息作为 ancestor 进入请求，身份仍只来自子投影 + 贡献。
#[tokio::test]
async fn fork_child_request_keeps_single_identity_with_parent_ancestors() {
    let (store, parent, cwd, _repo, _db) = bound_parent("child-wire-fork.db").await;
    let model = CaptureModel::new("fork done");
    let contribution = Arc::new(Mutex::new(None));
    let config = child_config(
        store,
        Arc::clone(&model),
        Arc::new(ContributionAssembler {
            contribution: Arc::clone(&contribution),
        }),
        true,
        &cwd,
    );
    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();
    assert!(!spawned.interrupted);

    let system = model.last_system();
    assert_eq!(
        system.matches("CHILD_IDENTITY_SENTINEL").count(),
        1,
        "fork 身份恰一次: {system}"
    );
    assert_eq!(
        system.matches("CHILD_SKILLS_AND_DEFERRED").count(),
        1,
        "fork 贡献恰一次: {system}"
    );
    let conversation = model.conversation_text();
    assert!(
        conversation.contains("parent turn"),
        "fork 必须携带父上下文: {conversation}"
    );
    assert!(
        !conversation.contains("CHILD_IDENTITY_SENTINEL"),
        "身份不得再写入 fork 子 transcript: {conversation}"
    );
}

/// live resume：身份从子会话持久历史恢复为 bridge base，贡献仍由恢复后的
/// 子链在请求时提供。
/// [回归测试] 仅检查计数会漏掉 identity 落在 dynamic boundary 后面的顺序错误。
#[tokio::test]
async fn resumed_child_request_reads_persisted_identity_and_fresh_contributions() {
    let (store, parent, cwd, _repo, _db) = bound_parent("child-wire-resume.db").await;
    let first = CaptureModel::new("first run");
    let contribution = Arc::new(Mutex::new(None));
    let config = child_config(
        Arc::clone(&store),
        Arc::clone(&first),
        Arc::new(ContributionAssembler {
            contribution: Arc::clone(&contribution),
        }),
        false,
        &cwd,
    );
    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();

    let second = CaptureModel::new("resumed run");
    let resume_contribution = Arc::new(Mutex::new(None));
    // 夹具在 resume 路径内部完成委派登记（与生产“当前可信 invocation”同源）。
    let mut resume = resume_config_with(
        Arc::clone(&store),
        spawned.child_thread_id.clone(),
        SubagentLlmSource::model(
            Arc::clone(&second) as Arc<dyn peri_model::Model>,
            "capture-model",
        ),
        SubagentRunMode::Sync,
        None,
        None,
    );
    resume.chain_assembler = Arc::new(ContributionAssembler {
        contribution: Arc::clone(&resume_contribution),
    });
    let resumed = SessionFactory::resume_subagent(Some(&parent), resume)
        .await
        .unwrap();
    assert!(!resumed.interrupted);

    let system = second.last_system();
    assert_eq!(
        system,
        first.last_system(),
        "恢复前后请求的 system 顺序与字节应一致"
    );
    assert_eq!(
        system.matches("CHILD_IDENTITY_SENTINEL").count(),
        1,
        "恢复身份恰一次（来自持久历史）: {system}"
    );
    assert_eq!(
        system.matches("CHILD_SKILLS_AND_DEFERRED").count(),
        1,
        "恢复贡献恰一次: {system}"
    );
    assert_ne!(
        system.matches("CHILD_IDENTITY_SENTINEL").count(),
        0,
        "身份不得因恢复丢失: {system}"
    );
}

/// [回归测试] 子 Agent 再 fork 时，只继承对话，不继承父子会话自己的身份。
#[tokio::test]
async fn nested_fork_request_excludes_parent_child_identity() {
    let (store, root, cwd, _repo, _db) = bound_parent("child-wire-nested-fork.db").await;
    let first_model = CaptureModel::new("first done");
    let first = SessionFactory::spawn_subagent(
        Some(&root),
        child_config(
            Arc::clone(&store),
            first_model,
            Arc::new(ContributionAssembler {
                contribution: Arc::new(Mutex::new(None)),
            }),
            false,
            &cwd,
        ),
    )
    .await
    .unwrap();
    let inherited = first
        .session
        .transcript()
        .read()
        .visible_model_messages()
        .unwrap();
    assert!(inherited.iter().any(|message| {
        matches!(message, BaseMessage::System { .. })
            && message.content() == "CHILD_IDENTITY_SENTINEL"
    }));
    let second_model = CaptureModel::new("second done");
    let mut config = child_config(
        store,
        Arc::clone(&second_model),
        Arc::new(ContributionAssembler {
            contribution: Arc::new(Mutex::new(None)),
        }),
        true,
        &cwd,
    );
    config.parent_messages = inherited;
    config.system_prompt = Some("GRANDCHILD_IDENTITY_SENTINEL".into());
    let second = SessionFactory::spawn_subagent(Some(&first.session), config)
        .await
        .unwrap();
    assert!(!second.interrupted);
    let system = second_model.last_system();
    assert_eq!(
        system,
        format!(
            "GRANDCHILD_IDENTITY_SENTINEL{}\n\nCHILD_SKILLS_AND_DEFERRED",
            peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
        )
    );
    let request = second_model.requests.lock().unwrap();
    let systems: Vec<_> = request
        .last()
        .unwrap()
        .messages
        .iter()
        .filter(|message| matches!(message, peri_model::ModelMessage::System { .. }))
        .collect();
    assert_eq!(systems.len(), 1, "孙 Agent 请求只能有自己的 System block");
}

/// [回归测试] Root 的同文 System 属于普通历史，不可按文本误判为子身份。
#[tokio::test]
async fn fork_from_root_preserves_own_system_even_when_it_matches_frozen_prompt() {
    let (store, root, cwd, _repo, _db) = bound_parent("child-wire-root-system.db").await;
    let root = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder()
            .system_prompt("ROOT_SYSTEM_SENTINEL")
            .build(),
        root.store().thread_id.clone(),
    );
    let root_system = BaseMessage::system("ROOT_SYSTEM_SENTINEL");
    root.transcript().write().append(root_system.clone());
    let model = CaptureModel::new("done");
    let mut config = child_config(
        store,
        Arc::clone(&model),
        Arc::new(ContributionAssembler {
            contribution: Arc::new(Mutex::new(None)),
        }),
        true,
        &cwd,
    );
    config.parent_messages = vec![root_system, BaseMessage::human("root task")];
    let spawned = SessionFactory::spawn_subagent(Some(&root), config)
        .await
        .unwrap();
    assert!(!spawned.interrupted);
    let system = model.last_system();
    assert!(system.contains("ROOT_SYSTEM_SENTINEL"));
    assert!(system.contains("CHILD_IDENTITY_SENTINEL"));
    let request = model.requests.lock().unwrap();
    assert_eq!(
        request
            .last()
            .unwrap()
            .messages
            .iter()
            .filter(|message| matches!(message, peri_model::ModelMessage::System { .. }))
            .count(),
        2,
        "root 的普通 System 必须保留在 fork 历史里"
    );
}

/// 后台 spawn：与前台共用同一装配点；请求在后台任务内发出，等待捕获后断言
/// 身份/贡献仍恰一次（覆盖 run_mode 分支，不覆盖后台注册/收尾的完整性）。
#[tokio::test]
async fn background_child_request_carries_single_identity_and_contributions() {
    let (store, parent, cwd, _repo, _db) = bound_parent("child-wire-bg.db").await;
    let model = CaptureModel::new("bg done");
    let contribution = Arc::new(Mutex::new(None));
    let mut config = child_config(
        store,
        Arc::clone(&model),
        Arc::new(ContributionAssembler {
            contribution: Arc::clone(&contribution),
        }),
        false,
        &cwd,
    );
    config.run_mode = SubagentRunMode::Background;
    config.task_manager = Some(Arc::new(crate::agent::async_tasks::TaskManager::new()));
    let (bg_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel();
    config.bg_event_sender = Some(bg_tx);
    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();
    assert!(spawned.task_id.is_some());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while model.requests.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !model.requests.lock().unwrap().is_empty(),
        "后台子会话必须发出模型请求"
    );
    let system = model.last_system();
    assert_eq!(
        system.matches("CHILD_IDENTITY_SENTINEL").count(),
        1,
        "后台身份恰一次: {system}"
    );
    assert_eq!(
        system.matches("CHILD_SKILLS_AND_DEFERRED").count(),
        1,
        "后台贡献恰一次: {system}"
    );
}
