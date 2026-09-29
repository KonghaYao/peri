use super::*;

// ── P2-1（实施质量审查）：链收集 vs 渲染面静态声明直接对拍 ─────────────────

/// 最小 ReactLLM fake（装配路径不调用 LLM）。
struct ParityFakeLlm;

#[async_trait]
impl peri_agent::agent::react::ReactLLM for ParityFakeLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[peri_agent::messages::BaseMessage],
        _tools: &[&dyn peri_agent::tools::BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<peri_agent::agent::react::Reasoning> {
        unimplemented!("对拍测试不调用 LLM")
    }
}

/// 最小 Model fake（HITL auto-classifier 构造消费，不调用）。
struct ParityFakeModel;

#[async_trait]
impl peri_model::Model for ParityFakeModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        unimplemented!("对拍测试不调用模型")
    }
}

/// 构造最小 AssemblyContext（复刻 peri-middlewares assembly_test base_context
/// 的段落持有者相关字段；条件注册字段全部关闭——对拍只关心持有者槽位）。
fn make_parity_context(
    disabled: &[&str],
    overrides: Option<peri_acp_types::agents::AgentOverrides>,
    language: Option<String>,
) -> peri_agent::session::factory::AssemblyContext {
    use std::collections::BTreeMap;

    use parking_lot::RwLock;
    use peri_acp_types::tools::TodoItem;
    use peri_agent::{
        agent::{async_tasks::TaskManager, AgentCancellationToken},
        tools::BaseTool,
    };

    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
    let (bg_event_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let llm_factory = Arc::new(|_model_alias: Option<&str>| {
        Box::new(ParityFakeLlm) as Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>
    });

    peri_agent::session::factory::AssemblyContext {
        agent_catalog: Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        cwd: "/tmp/parity-test".to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Default),
        model_name: "parity-model".to_string(),
        provider_name: "parity-provider".to_string(),
        auxiliary_model: None,
        auto_classifier_model: Arc::new(tokio::sync::Mutex::new(
            Box::new(ParityFakeModel) as Box<dyn peri_model::Model>
        )),
        preload_skills: Vec::new(),
        plugin_skill_roots: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        session_id: "session-parity-test".to_string(),
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        lsp_servers: Vec::new(),
        lsp_pool: None,
        workflow_executor: None,
        workflow_middleware: None,
        event_handler: Arc::new(ParityFakeEventHandler),
        task_manager: Arc::new(TaskManager::new()),
        bg_event_tx,
        on_bg_complete: None,
        langfuse_bridge: None,
        session_resources: None,
        parent_thread_id: None,
        register_runtime: None,
        deregister_runtime: None,
        child_handler_factory: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        system_prompt_for_sub: String::new(),
        llm_factory,
        system_builder: Arc::new(
            |_ov: Option<&peri_acp_types::agents::AgentOverrides>, _cwd: &str| String::new(),
        ),
        todo_tx,
        goal_controller: None,
        meta_harness_disabled: disabled.iter().map(|s| s.to_string()).collect(),
        agent_overrides: overrides,
        language,
    }
}

/// P2-1（实施质量审查）：链收集与渲染面静态声明**直接对拍**——同一 disabled
/// 状态 / overrides / language 下，真实装配链的 `collect_prompt_sections` 与
/// 渲染面 `build_collected_sections` 段落 (id, zone, order, 内容) 集合相等。
///
/// 锁定不变式：「5 个段落持有者的装配条件全部只按 disabled 集合过滤」。
/// 若未来装配条件因非 disabled 原因排除某持有者（链收集少段而静态声明仍
/// 收集），本测试立即失败——冻结提示词与链状态静默失同步的前哨。
#[test]
fn chain_collection_parity_with_build_collected_sections() {
    use peri_acp_types::agents::AgentOverrides;
    use peri_acp_types::meta_harness::MetaHarnessState;
    use peri_agent::session::factory::build_middleware_chain;
    use peri_middlewares::assembly::ProductionChainAssembler;

    let cases = &[
        ("默认装配", None, None, &[] as &[&str]),
        (
            "关闭 gated 持有者",
            None,
            None,
            &[
                "PermissionMiddleware",
                "HumanInTheLoopMiddleware",
                "SubAgentMiddleware",
                "SkillsMiddleware",
            ],
        ),
        (
            "关闭基础持有者",
            None,
            None,
            &["DefaultSystemPromptMiddleware", "LangMiddleware"],
        ),
        (
            "关闭全部持有者",
            None,
            None,
            &[
                "DefaultSystemPromptMiddleware",
                "LangMiddleware",
                "PermissionMiddleware",
                "HumanInTheLoopMiddleware",
                "SubAgentMiddleware",
                "SkillsMiddleware",
            ],
        ),
        (
            "persona + 语言注入",
            Some(AgentOverrides {
                persona: Some("parity persona".into()),
                tone: Some("parity tone".into()),
                proactiveness: None,
                mode: None,
            }),
            Some("zh-CN".to_string()),
            &[],
        ),
    ];

    for (name, overrides, language, disabled) in cases {
        let ctx = make_parity_context(disabled, overrides.clone(), language.clone());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let mut chain_sections: Vec<(String, u16, u16, String)> = out
            .chain
            .collect_prompt_sections()
            .into_iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.zone as u16,
                    s.order,
                    s.content.as_str().to_string(),
                )
            })
            .collect();

        let state = MetaHarnessState {
            disabled_middlewares: disabled.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let mut declared: Vec<(String, u16, u16, String)> =
            crate::session::build_collected_sections(
                &state,
                overrides.as_ref(),
                language.as_deref(),
            )
            .into_iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.zone as u16,
                    s.order,
                    s.content.as_str().to_string(),
                )
            })
            .collect();

        // 集合对拍（契约 2：收集不承诺顺序——链收集按 middleware 链序、
        // 静态声明按持有者声明序，渲染面统一按 (zone, order) 排序；此处
        // 只锁定「同状态下收集到的段落集合相等」，顺序由渲染面位置属性
        // 测试独立锁定）。
        chain_sections.sort();
        declared.sort();
        assert_eq!(
            chain_sections, declared,
            "case [{name}]：链收集与静态声明必须一致（同一 disabled 状态）"
        );
    }
}
