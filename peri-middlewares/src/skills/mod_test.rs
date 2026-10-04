//! `SkillsMiddleware` 测试（W4b 后：零文件系统依赖，目录只来自 MCP registry）。
//!
//! 覆盖：配置位读取（F12）、摘要渲染与来源标签（J1/D4）、系统来源投递规则
//! （冻结优先 / 未冻结只用 system）、registry 投影进 `cached_skills`、
//! 工具面形状、13_skills 段落声明。
//!
//! **已删除**（对应的本地扫描机制在 W4b 移除）：临时 skills 目录/插件根的
//! 扫描聚合、`resolve_roots` 覆盖参数、builtin 嵌入摘要、磁盘删除后的可恢复
//! 错误——技能内容与目录现在都经 MCP 侧，见 `crate::mcp::skill_discovery_test`
//! 与 `peri-acp` 的会话端到端用例。

use peri_agent::{agent::state::AgentState, middleware::r#trait::Middleware};
use tempfile::tempdir;

use super::*;

/// Helper: call prompt_contribution with concrete State type for testing.
fn contribution(mw: &SkillsMiddleware) -> Option<String> {
    Middleware::prompt_contribution(mw)
}

fn fake_skill(server: &str, scope: &str, name: &str) -> SkillMetadata {
    SkillMetadata {
        name: peri_acp_types::mcp_skills::mcp_skill_name(server, name),
        aliases: Vec::new(),
        description: format!("{name} description"),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: format!("skill://{scope}/{name}/SKILL.md"),
        }),
        content: None,
        resources: Vec::new(),
        frontmatter: None,
    }
}

/// 造一个已发现若干 skill 的 registry。
fn registry_with(server: &str, skills: Vec<SkillMetadata>) -> Arc<McpSkillRegistry> {
    use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(server.to_string());
    reg.mark_discovery_started(server, handle.clone());
    reg.mark_discovery_completed(server, handle, skills);
    reg
}

// ─── 摘要渲染与来源标签（D4 / J1）─────────────────────────────────────────

#[test]
fn build_summary_exposes_names_and_scope_labels() {
    let skills = vec![
        fake_skill("workspace", "project", "brainstorming"),
        fake_skill("workspace", "builtin", "example"),
    ];
    let summary = SkillsMiddleware::build_summary(&skills);
    assert!(summary.contains("mcp__workspace__brainstorming"));
    assert!(
        summary.contains("[project]"),
        "scope 标签来自 URI: {summary}"
    );
    assert!(
        summary.contains("[builtin]"),
        "scope 标签来自 URI: {summary}"
    );
}

#[test]
fn build_summary_does_not_inject_descriptions() {
    // description 是检索元数据而非可信指令：不进摘要正文（D4）
    let skills = vec![fake_skill("workspace", "project", "brainstorming")];
    let summary = SkillsMiddleware::build_summary(&skills);
    assert!(
        !summary.contains("#brainstorming"),
        "不得使用旧 #skill_name 形态: {summary}"
    );
    assert!(
        summary.contains("'/skill-name'"),
        "摘要应提示 slash 触发口径: {summary}"
    );
}

#[test]
fn render_frozen_summary_is_none_for_empty_catalog() {
    // 技能根不存在/无技能 ⇒ 空属正常（X5），不是错误
    assert!(SkillsMiddleware::render_frozen_summary(&[]).is_none());
    assert!(SkillsMiddleware::render_frozen_summary(&[fake_skill(
        "workspace",
        "project",
        "brain"
    )])
    .is_some());
}

#[test]
fn source_label_falls_back_to_mcp_for_foreign_uri_shapes() {
    let mut skill = fake_skill("remote", "user", "brain");
    skill.origin = Some(SkillOrigin::Mcp {
        server: "remote".to_string(),
        uri: "skill://hello/SKILL.md".to_string(),
    });
    assert_eq!(SkillsMiddleware::source_label(&skill), "mcp");
}

// ─── registry 投影与投递规则（F2 / J1）───────────────────────────────────

#[tokio::test]
async fn without_registry_cache_and_contribution_stay_empty() {
    // 未装配技能面：没有投影、没有摘要，也不回落任何本地来源（J5）
    let dir = tempdir().unwrap();
    let mw = SkillsMiddleware::new();
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    assert!(mw.skills_cache().read().unwrap().is_none());
    assert!(contribution(&mw).is_none());
}

#[tokio::test]
async fn registry_projection_fills_cache_every_turn() {
    let dir = tempdir().unwrap();
    let reg = registry_with("demo", vec![fake_skill("demo", "user", "hello")]);
    let mw = SkillsMiddleware::new().with_mcp_registry(Some(Arc::clone(&reg)));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    for _ in 0..2 {
        mw.before_agent(&mut state).await.unwrap();
    }

    let cache = mw.skills_cache();
    let skills = cache.read().unwrap();
    let names: Vec<&str> = skills
        .as_ref()
        .expect("投影后缓存非空")
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["mcp__demo__hello"], "条目逐轮重建: {names:?}");
}

/// 未冻结（legacy/无冻结面）时：只有**系统来源**进 contribution，
/// 外部 origin 保持既有延迟发现语义（不进 prompt）。
#[tokio::test]
async fn unfrozen_contribution_uses_system_origins_only() {
    let dir = tempdir().unwrap();
    let reg = registry_with(
        "workspace",
        vec![fake_skill("workspace", "project", "local-skill")],
    );
    let handle: peri_acp_types::mcp_skills::HandleToken = Arc::new("workspace".to_string());
    reg.mark_discovery_started("remote", handle.clone());
    reg.mark_discovery_completed(
        "remote",
        handle,
        vec![fake_skill("remote", "user", "remote-skill")],
    );
    reg.mark_system_origins(&["workspace".to_string()]);

    let mw = SkillsMiddleware::new().with_mcp_registry(Some(Arc::clone(&reg)));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(
        content.contains("mcp__workspace__local-skill"),
        "系统来源进摘要: {content}"
    );
    assert!(
        !content.contains("mcp__remote__remote-skill"),
        "非 system 来源不进 prompt contribution: {content}"
    );
}

/// 冻结摘要优先：会话内不再按当轮投影重渲染（ARC-FROZEN-001 的投递面）。
#[tokio::test]
async fn frozen_summary_wins_over_current_projection() {
    let dir = tempdir().unwrap();
    let reg = registry_with(
        "workspace",
        vec![fake_skill("workspace", "project", "appeared-later")],
    );
    reg.mark_system_origins(&["workspace".to_string()]);
    let mw = SkillsMiddleware::new()
        .with_mcp_registry(Some(reg))
        .with_frozen_summary("FROZEN CATALOG".to_string());
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert_eq!(content, "FROZEN CATALOG");
}

// ─── 工具面与段落声明 ────────────────────────────────────────────────────

#[test]
fn test_collect_tools_exposes_only_unified_skill_protocol() {
    let mw = SkillsMiddleware::new();
    let tools = mw.collect_tools("/tmp");
    let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["DiscoverSkillsTool", "SkillTool"],
        "模型可见 skill 协议必须唯一，不得残留 Skill(skill, args)"
    );
    // 参数契约：SkillTool 接收 skill_name（非 skill/args）
    let skill_tool = tools.iter().find(|t| t.name() == "SkillTool").unwrap();
    let params = skill_tool.parameters();
    assert!(
        params["properties"]["skill_name"].is_object(),
        "SkillTool 参数必须是 skill_name，实际: {}",
        params
    );
    assert!(
        params["properties"].get("skill").is_none() && params["properties"].get("args").is_none(),
        "SkillTool 不得再暴露旧 skill/args 参数"
    );
}

#[test]
fn skills_section_declaration_shape() {
    let sections = SkillsMiddleware::sections();
    assert_eq!(sections.len(), 1, "13_skills 段应唯一");
    let section = &sections[0];
    assert_eq!(section.id, "13_skills");
    assert_eq!(section.zone, PromptSectionZone::Uncached);
    assert_eq!(section.order, 6);
    let content = section.content.as_str();
    assert!(
        content.contains("# Skills"),
        "机制说明标题应保留（include_str 零拷贝）"
    );
    assert!(
        content.contains("## Skill loading protocol"),
        "loading 协议机制说明保留"
    );
    assert!(
        content.contains("Skill roots are resolved by the provider in priority order"),
        "discovery 小节引导保留（动态内容后缀拼接）"
    );
    // 段落文件不再硬编码任何本地路径/扫描参数（J5 失同步防线）
    let file_content = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-acp/prompts/sections/13_skills.md"
    ));
    assert!(
        !file_content.contains("Each skill root is scanned recursively"),
        "13_skills.md 不应再硬编码扫描参数"
    );
    assert!(
        !file_content.contains("mid-session are NOT reflected") || file_content.contains("MCP"),
        "13_skills.md 目录语义应描述 MCP 来源"
    );
}

#[test]
fn discovery_protocol_describes_mcp_sources_without_local_paths() {
    let text = format_discovery_protocol();
    assert!(
        text.contains("MCP"),
        "协议文本应说明技能经 MCP 提供: {text}"
    );
    assert!(
        text.contains("DiscoverSkillsTool") && text.contains("SkillTool"),
        "协议文本应给出两个工具: {text}"
    );
    for stale in [
        "~/.claude/skills",
        "scanned recursively",
        "levels deep",
        "directories per root",
    ] {
        assert!(
            !text.contains(stale),
            "协议文本不得残留本地扫描/路径事实（{stale}）: {text}"
        );
    }
}
