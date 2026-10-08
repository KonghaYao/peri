use super::*;

// ─── McpSkillReleaser（决策 A2/D 放行跳板）─────────────────────────────────

/// 最小事件 sink（releaser execute 需要 CommandContext，对齐
/// plugin/loader_test.rs 先例）。
struct NoopEventSink;

#[async_trait::async_trait]
impl peri_acp_types::event::EventSink for NoopEventSink {
    async fn push_event(
        &self,
        _session_id: &str,
        _event: &peri_acp_types::event::ExecutorEvent,
        _context_window: u32,
    ) {
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 构造最小 CommandContext（supports_inject / raw_text 按场景置位）。
fn make_releaser_ctx(raw_text: &str, supports_inject: bool) -> CommandContext {
    let mut ctx = CommandContext::new(
        "s1".to_string(),
        vec![],
        "/tmp".to_string(),
        Arc::new(NoopEventSink),
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    ctx.raw_text = raw_text.to_string();
    ctx.supports_inject = supports_inject;
    ctx
}

/// 造 Discovered 条目（content 带全文；对齐 mcp_skills_test 的 complete）。
fn seed_discovered(reg: &Arc<McpSkillRegistry>, server: &str, skill: &str, content: &str) {
    let token: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: format!("mcp__{server}__{skill}"),
        aliases: Vec::new(),
        description: format!("MCP skill {skill}"),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: format!("skill://{server}/{skill}/SKILL.md"),
        }),
        content: Some(content.to_string()),
        resources: vec![],
        frontmatter: None,
    };
    reg.mark_discovery_started(server, token.clone());
    reg.mark_discovery_completed(server, token, vec![meta]);
}

/// W2：命令形态命中多个 origin（同末段 server 名）→ 显式拒绝 + 列出**可输入的
/// 完整名**候选；不注入任何内容、不静默取首个。
#[tokio::test]
async fn releaser_ambiguous_command_lists_candidates_without_injection() {
    let reg = Arc::new(McpSkillRegistry::new());
    seed_discovered(&reg, "a:srv", "beta", "Body A");
    seed_discovered(&reg, "b:srv", "beta", "Body B");
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("/srv:beta", false))
        .await;
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty(), "歧义不得注入任何内容");
            let fb = result.feedback.expect("歧义应有 Info 反馈");
            assert!(fb.message.contains("多个来源"), "实际: {}", fb.message);
            assert!(
                fb.message.contains("a:srv:beta") && fb.message.contains("b:srv:beta"),
                "候选必须列全且为可输入的完整名: {}",
                fb.message
            );
        }
        _other => panic!("歧义应回 Done + Info"),
    }
}

/// 交互式（supports_inject）：Inject(原文)——命令含 `/` 前缀与 args 整段
/// 放行进 agent 管线，由 SkillPreload 完成注入（决策 A2 核心语义）。
#[tokio::test]
async fn releaser_interactive_injects_raw_text() {
    let reg = Arc::new(McpSkillRegistry::new());
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("/demo:hello some args", true))
        .await;
    match outcome {
        CommandOutcome::Inject(text) => {
            assert_eq!(
                text, "/demo:hello some args",
                "原文整段放行（含 / 前缀与 args）"
            )
        }
        _other => panic!("交互式应 Inject 原文"),
    }
}

/// 交互式但原文缺失（理论不可达：拦截层恒透传）→ 回退 Done + Info，不吞
/// 命令不静默。
#[tokio::test]
async fn releaser_interactive_empty_raw_text_falls_back_done() {
    let reg = Arc::new(McpSkillRegistry::new());
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser.execute(make_releaser_ctx("", true)).await;
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty());
            let fb = result.feedback.expect("回退应有 Info 反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("原文缺失"),
                "反馈应说明原文缺失，实际: {}",
                fb.message
            );
        }
        _other => panic!("原文缺失应回退 Done"),
    }
}

/// RPC（supports_inject=false，决策 D）：直返 skill 全文 + 标注（与预载注入
/// 同源 annotate_mcp_content），feedback 说明语义差异。
#[tokio::test]
async fn releaser_rpc_returns_skill_content_with_annotation() {
    let reg = Arc::new(McpSkillRegistry::new());
    seed_discovered(&reg, "demo", "hello", "Body of hello.");
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("/demo:hello", false))
        .await;
    match outcome {
        CommandOutcome::Done(result) => {
            let last = result.messages.last().expect("RPC 命中应追加内容消息");
            let content = last.content();
            assert!(content.contains("Body of hello."), "应含 skill 全文");
            assert!(
                content.contains("This skill is served by MCP server \"demo\""),
                "应含来源标注，实际: {content}"
            );
            assert!(
                content.contains("SearchExtraTools"),
                "应含工具通路提醒，实际: {content}"
            );
            let fb = result.feedback.expect("RPC 命中应有反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("内容已返回"),
                "反馈应说明直返语义，实际: {}",
                fb.message
            );
        }
        _other => panic!("RPC 命中应直返 Done"),
    }
}

/// RPC 且 registry miss（server 未连接/无该 skill）→ Done + Info 提示，不
/// 吞命令不静默；messages 原样（不追加内容）。
#[tokio::test]
async fn releaser_rpc_miss_falls_back_done_info() {
    let reg = Arc::new(McpSkillRegistry::new());
    seed_discovered(&reg, "demo", "hello", "Body of hello.");
    let releaser = McpSkillReleaser {
        registry: Arc::clone(&reg),
    };
    let outcome = releaser
        .execute(make_releaser_ctx("other:bye", false))
        .await;
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty(), "miss 不追加内容");
            let fb = result.feedback.expect("miss 应有反馈");
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert!(
                fb.message.contains("未发现"),
                "反馈应说明未发现，实际: {}",
                fb.message
            );
        }
        _other => panic!("RPC miss 应回退 Done"),
    }
}

#[tokio::test]
async fn cached_skill_discovery_reads_no_bodies() {
    let text = "---\nname: cached\ndescription: Cached skill\n---\n\n# Cached\n";
    let request_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![SpecSkill {
            uri: "skill://cached/SKILL.md",
            name: "cached",
            description: "Cached skill",
            text,
            digest_override: None,
            get_text: None,
            get_error: false,
            get_wrong_uri: false,
        }],
        None,
        Some(Arc::clone(&request_log)),
        true,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    let handle = make_spec_handle(&running);
    let (cache, origin) = super::cache_fixture::scoped_cache(
        handle.clone(),
        cache,
        peri_acp_types::workspace::WorkspaceId::new(),
        None,
    );

    let first = Arc::new(McpSkillRegistry::new());
    let first_token: HandleToken = Arc::new(41u32);
    first.mark_discovery_started("srv", first_token.clone());
    run_discovery_with_cache(
        first.clone(),
        None,
        handle.clone(),
        first_token,
        AgentCancellationToken::new(),
        Some((cache.clone(), origin.clone())),
        false,
    )
    .await;
    assert_eq!(first.all_skills().len(), 1);
    assert!(first.all_skills()[0].content.is_none(), "发现期不携带正文");
    {
        let log = request_log.lock().unwrap();
        assert_eq!(
            log.len(),
            1,
            "首次发现只允许 skills/list 一个请求（W2：不读正文），实际: {log:?}"
        );
        assert!(log[0].starts_with("skills/list"), "实际: {log:?}");
    }
    let first_requests = request_log.lock().unwrap().len();

    let second = Arc::new(McpSkillRegistry::new());
    let second_token: HandleToken = Arc::new(42u32);
    second.mark_discovery_started("srv", second_token.clone());
    run_discovery_with_cache(
        second.clone(),
        None,
        handle,
        second_token,
        AgentCancellationToken::new(),
        Some((cache, origin)),
        false,
    )
    .await;

    assert_eq!(second.all_skills().len(), 1);
    assert_eq!(
        request_log.lock().unwrap().len(),
        first_requests,
        "skills/list 分页应从持久化缓存读取（发现期无正文读取）"
    );
    assert!(
        request_log
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.starts_with("resources/read")),
        "两次发现都不得产生 resources/read"
    );
}
