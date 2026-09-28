use super::*;

// ── FrozenSessionData 渲染测试（L5：渲染面留 ACP，经 build_frozen_data）───

/// 构造带 SkillsProvider 的 SessionManager（frozen 渲染输入）。
async fn make_manager(tmp: &tempfile::TempDir) -> SessionManager {
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    SessionManager::new(
        session_resources,
        LlmProvider::from_config(&peri_config).unwrap(),
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        None,
        Arc::new(SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    )
}

/// [回归测试] 同一 frozen 输入必须产生字节相同的 system prompt。
///
/// 历史背景（ARC-FROZEN-001）：system prompt 在 session/new 时一次性冻结；
/// 相同会话输入（cwd/language/skill roots/date/permission mode）若因调用方
/// 上下文差异产生不同前缀，会破坏 Anthropic 前缀缓存，并使主 agent 与
/// subagent 看到不一致的策略。本测试固定全部输入，验证
/// `build_frozen_data` 是确定性的。
///
/// `#[serial]`：`build_frozen_data` 扫描用户级 `~/.claude/skills`（HOME），
/// 与 requests_test 的 `#[serial]` 组（B3 用例重定向 HOME）互斥，防止两次
/// build 间读到不同 HOME 快照。
#[tokio::test]
#[serial]
async fn test_frozen_session_data_build_is_deterministic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp).await;
    let cwd = "/tmp";

    let a = mgr.build_frozen_data(cwd, &[], &[]);
    let b = mgr.build_frozen_data(cwd, &[], &[]);

    assert_eq!(
        a.system_prompt(),
        b.system_prompt(),
        "相同 frozen 输入两次 build 应产生相同 system prompt"
    );
    assert_eq!(
        a.skill_summary(),
        b.skill_summary(),
        "相同 frozen 输入两次 build 应产生相同 skill 摘要"
    );
}

/// [回归测试] 已冻结的 system prompt 与 skill 摘要不受会话中途磁盘变化影响。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md P2-11）：skill
/// 摘要与 system prompt 在 session/new 冻结；会话内磁盘 skill 增删不得改变
/// 已冻结产物（冻结是前缀缓存稳定性的有意权衡，不能按需重扫）。
///
/// `#[serial]`：与 requests_test 的 `#[serial]` 组（B3 用例重定向 HOME）
/// 互斥，防止冻结前后两次扫描读到不同用户级 skills 快照。
#[tokio::test]
#[serial]
async fn test_frozen_system_prompt_immune_to_disk_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp).await;
    let cwd = tmp.path().to_str().unwrap();

    // 冻结前：cwd 含 skill-a
    let skills_dir_a = tmp.path().join(".claude").join("skills").join("skill-a");
    std::fs::create_dir_all(&skills_dir_a).unwrap();
    std::fs::write(
        skills_dir_a.join("SKILL.md"),
        "---\nname: 'skill-a'\ndescription: 'A test skill'\n---\n\nbody",
    )
    .unwrap();

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    let frozen_prompt = frozen.system_prompt().to_string();
    let frozen_summary = frozen.skill_summary().map(|s| s.to_string());
    assert!(
        frozen_summary.as_deref().unwrap_or("").contains("skill-a"),
        "冻结摘要应包含冻结时的 skill-a"
    );

    // 会话中途：删除 skill-a，新增 skill-b 与 CLAUDE.md
    std::fs::remove_dir_all(&skills_dir_a).unwrap();
    let skills_dir_b = tmp.path().join(".claude").join("skills").join("skill-b");
    std::fs::create_dir_all(&skills_dir_b).unwrap();
    std::fs::write(
        skills_dir_b.join("SKILL.md"),
        "---\nname: 'skill-b'\ndescription: 'B test skill'\n---\n\nbody",
    )
    .unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# New CLAUDE.md").unwrap();

    // 已冻结产物不变（不按需重读磁盘）
    assert_eq!(
        frozen.system_prompt(),
        frozen_prompt,
        "已冻结 system prompt 不应受会话中途磁盘变化影响"
    );
    assert_eq!(
        frozen.skill_summary().map(|s| s.to_string()),
        frozen_summary,
        "已冻结 skill 摘要不应随磁盘重扫"
    );
}

/// [回归测试] 16_workflow 已整段删除（波 4 演进 C2）：冻结 prompt 恒不
/// 声明 Workflow（ultracode skill 完整承载 WorkflowTool 指引，设计 §3.1.2）；
/// `build_frozen_data` 的 workflow_enabled 参数随 gate 清理删除。
#[tokio::test]
async fn test_frozen_prompt_never_claims_workflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp).await;
    let cwd = "/tmp";

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    assert!(
        !frozen.system_prompt().contains("Workflow Orchestration"),
        "16_workflow 段落已删除：冻结 prompt 不得声明 Workflow"
    );
}

/// [回归测试] 子 agent / fork / workflow agent 复用的冻结 prompt 与主
/// prompt 字节相同（16_workflow 已删除，无子面向 feature 差异）。
#[tokio::test]
async fn test_frozen_subagent_prompt_identical_to_main() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp).await;
    let cwd = "/tmp";

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    assert!(
        !frozen.system_prompt().contains("Workflow Orchestration"),
        "16_workflow 段落已删除：冻结 prompt 不得声明 Workflow"
    );
    // 子面向 prompt 字段已随 C5 移除：子 agent / fork / workflow agent
    // 直接复用主冻结 prompt（16_workflow 删除后两版字节相同的语义固化）。
    assert!(
        !frozen.system_prompt().is_empty(),
        "主冻结 prompt 非空（子面向唯一复用来源）"
    );
}

/// [回归测试] advisor 裁决 B（2026-08-14）：workflow agent 链不装配审批
/// middleware（broker: None → PermissionMiddleware::disabled()），10_hitl
/// 描述的是主会话审批机制，对 workflow 模型是误导性指令——workflow 渲染
/// 路径（fallback + agentType builder）必须排除 10_hitl，兑现
/// presence-is-the-gate 契约（C3 D5 决策修订，design §3.1.1 契约 3 /
/// §3.5 语义边界；2026-08-15 拆分：10_hitl 持有者改为 PermissionMiddleware，
/// 过滤目标段落不变）。
#[tokio::test]
async fn test_workflow_prompt_excludes_hitl_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp).await;
    let frozen = mgr.build_frozen_data("/tmp", &[], &[]);

    // 主链冻结 prompt 保留 10_hitl（PermissionMiddleware 默认装配）
    assert!(
        frozen.system_prompt().contains("Human-in-the-Loop (HITL)"),
        "主链冻结 prompt 应保留 10_hitl（PermissionMiddleware 默认装配）"
    );

    let skills: Arc<dyn peri_acp_types::ports::SkillsPort> = Arc::new(SkillsProvider);
    let fallback = crate::host::workflow_agent::build_workflow_system_prompt_fallback(
        Arc::clone(&skills),
        frozen.meta_harness().clone(),
    );
    let prompt = fallback("/tmp", Some("2026-01-01"), frozen.language());
    assert!(
        !prompt.contains("Human-in-the-Loop (HITL)"),
        "workflow 链无审批 middleware：提示词不得包含 10_hitl"
    );

    // agentType builder（workflow 子 agent）同样排除
    let builder = crate::host::workflow_agent::build_workflow_agent_prompt_builder(
        Arc::clone(&skills),
        frozen.meta_harness().clone(),
    );
    let agent_prompt = builder(None, "/tmp", Some("2026-01-01"), frozen.language());
    assert!(
        !agent_prompt.contains("Human-in-the-Loop (HITL)"),
        "workflow agentType builder 同样不得包含 10_hitl"
    );
}
