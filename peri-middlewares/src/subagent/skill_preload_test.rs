//! `SkillPreloadMiddleware` 测试（W4b 后：只按名查 MCP registry，无磁盘回落）。
//!
//! 覆盖：`/skill` token 提取、registry 命中的注入结构（Ai tool_use + Tool
//! tool_result）、别名/裸名/plugin 末段 server 形态、miss 的缺口语义（不注入）、
//! 跨 origin 歧义的显式拒绝、输入顺序保持、以及「未装配 registry ⇒ 缺口，不
//! 回落磁盘」。
//!
//! **已删除**（对应机制在 W4b 移除）：本地磁盘 skill 目录的预载用例、
//! `extra_dirs`/plugin roots、`mcp__` 前缀防误注入回归（本地来源已不存在，
//! 该分支无从触发）、`<ns>:<name>` 的磁盘回退。

use peri_agent::{agent::state::AgentState, middleware::r#trait::Middleware};
use tempfile::tempdir;

use super::*;

use std::sync::Arc;

use peri_acp_types::{
    mcp_skills::{mcp_skill_name, HandleToken, McpSkillRegistry},
    skills::{SkillMetadata, SkillOrigin, SkillSource},
};

/// seed registry：Started + Completed 造 Discovered 条目（模拟发现任务完成态）。
fn seed_registry_with_skill(server: &str, skill: &str) -> Arc<McpSkillRegistry> {
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: mcp_skill_name(server, skill),
        aliases: Vec::new(),
        description: format!("MCP skill {skill}"),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: format!("skill://{server}/{skill}/SKILL.md"),
        }),
        content: Some(format!("# Hello\n\nBody of {skill}.\n")),
        // 测试 fixture：无 resources 绑定
        resources: Vec::new(),
        frontmatter: None,
    };
    reg.mark_discovery_started(server, handle.clone());
    reg.mark_discovery_completed(server, handle, vec![meta]);
    reg
}

fn middleware(reg: Arc<McpSkillRegistry>) -> SkillPreloadMiddleware {
    SkillPreloadMiddleware::new(vec![]).with_mcp_registry(Some(reg))
}

// ─── token 提取（不变式）──────────────────────────────────────────────────

#[test]
fn test_extract_skill_names_basic() {
    assert_eq!(
        extract_skill_names_from_text("/brainstorming"),
        vec!["brainstorming"]
    );
}

#[test]
fn test_extract_skill_names_multiple() {
    assert_eq!(
        extract_skill_names_from_text("/skill-a /skill-b"),
        vec!["skill-a", "skill-b"]
    );
}

#[test]
fn test_extract_skill_names_in_sentence() {
    assert_eq!(
        extract_skill_names_from_text("please use /code-review now"),
        vec!["code-review"]
    );
}

#[test]
fn test_extract_skill_names_no_match() {
    assert!(extract_skill_names_from_text("no slash tokens here").is_empty());
}

#[test]
fn test_extract_skill_names_slash_only() {
    assert!(extract_skill_names_from_text("/").is_empty());
}

#[test]
fn test_extract_skill_names_rejects_path_like() {
    // 路径片段（含 `/`）不是 skill token
    assert!(extract_skill_names_from_text("a/b/c").is_empty());
}

// ─── 注入结构 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_no_op_when_empty_names_and_no_token() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("hello there"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "无 skill token 时不注入");
}

#[tokio::test]
async fn test_no_op_when_no_human_message() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    mw.before_agent(&mut state).await.unwrap();

    assert!(state.messages().is_empty());
}

/// registry 命中：注入 `Human → Ai(tool_use SkillTool) → Tool(tool_result)`，
/// 正文带来源标注（与 SkillTool / 命令面同源）。
#[tokio::test]
async fn test_preload_mcp_skill_injects_annotated_content() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("use /mcp__demo__hello"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "registry 命中应注入 Ai + Tool");
    let tool_content = state.messages()[2].content();
    assert!(tool_content.contains("Body of hello."), "应含正文");
    assert!(
        tool_content.contains("This skill is served by MCP server \"demo\""),
        "应含来源标注 server 名，实际: {tool_content}"
    );
    assert!(
        tool_content.contains("skill://demo/hello/SKILL.md"),
        "应含来源标注 uri，实际: {tool_content}"
    );
}

/// 裸名 `/hello`（`/skill-name` UX）：registry origin 感知查找的裸名分支。
#[tokio::test]
async fn test_preload_bare_name_hits_registry() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("workspace", "brainstorming"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/brainstorming 帮我"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "裸名应命中 registry");
    assert!(state.messages()[2]
        .content()
        .contains("Body of brainstorming."));
}

#[tokio::test]
async fn test_preload_injects_multiple_skills_in_input_order() {
    let dir = tempdir().unwrap();
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new("demo".to_string());
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed(
        "demo",
        handle,
        ["alpha", "beta"]
            .into_iter()
            .map(|skill| SkillMetadata {
                name: mcp_skill_name("demo", skill),
                aliases: Vec::new(),
                description: format!("MCP skill {skill}"),
                path: std::path::PathBuf::new(),
                source: SkillSource::Mcp,
                plugin_name: None,
                origin: Some(SkillOrigin::Mcp {
                    server: "demo".to_string(),
                    uri: format!("skill://demo/{skill}/SKILL.md"),
                }),
                content: Some(format!("Body of {skill}.\n")),
                resources: Vec::new(),
                frontmatter: None,
            })
            .collect(),
    );
    let mw = middleware(reg);
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/beta /alpha"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 4, "两个命中 = Ai + 2 Tool");
    let calls = state.messages()[1].tool_calls();
    let names: Vec<_> = calls
        .iter()
        .map(|call| call.arguments["skill_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["beta", "alpha"], "注入顺序 = 输入顺序");
    assert!(state.messages()[2].content().contains("Body of beta."));
    assert!(state.messages()[3].content().contains("Body of alpha."));
}

/// miss 夹在命中之间：注入顺序仍等于用户输入顺序（miss 不占槽位）。
#[tokio::test]
async fn test_preload_preserves_input_order_with_miss_in_between() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/miss-a /mcp__demo__hello /miss-b"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "仅 1 个命中（Ai + Tool）");
    let calls = state.messages()[1].tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].arguments["skill_name"].as_str(),
        Some("mcp__demo__hello")
    );
}

// ─── 命中形态 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_preload_mcp_skill_by_alias() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/demo:hello 帮我"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "别名命中应注入 Ai + Tool");
    assert!(state.messages()[2].content().contains("Body of hello."));
}

/// plugin 多冒号 server key（`plugin:p1:demosrv`）下 `/demosrv:beta`
/// 按 server 名末段命中（与命令面 fullname 派生同构）。
#[tokio::test]
async fn test_preload_plugin_server_via_trailing_segment() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("plugin:p1:demosrv", "beta"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/demosrv:beta 帮我"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "末段形态应命中");
    let tool_content = state.messages()[2].content();
    assert!(tool_content.contains("Body of beta."));
    assert!(
        tool_content.contains("MCP server \"plugin:p1:demosrv\""),
        "标注应含完整 server key，实际: {tool_content}"
    );
}

// ─── 缺口与歧义（不注入、不回落）─────────────────────────────────────────

/// 未装配 registry：缺口报告，不注入、不读任何路径（J5）。
#[tokio::test]
async fn test_missing_registry_reports_gap_without_injection() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec![]);
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/anything 帮我"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "未装配 registry ⇒ 只保留 Human");
}

/// registry miss：缺口语义（不注入、不回落磁盘）。
#[tokio::test]
async fn test_registry_miss_injects_nothing() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/nonexistent 不存在"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "miss 应静默跳过（缺口）");
}

/// 同名跨 origin（末段 server 名相同）→ 显式拒绝注入，不静默取首个。
#[tokio::test]
async fn test_preload_ambiguous_origin_rejects_injection() {
    let dir = tempdir().unwrap();
    let reg = Arc::new(McpSkillRegistry::new());
    for server in ["a:srv", "b:srv"] {
        let handle: HandleToken = Arc::new(1u32);
        let meta = SkillMetadata {
            name: mcp_skill_name(server, "beta"),
            aliases: Vec::new(),
            description: "Beta skill".to_string(),
            path: std::path::PathBuf::new(),
            source: SkillSource::Mcp,
            plugin_name: None,
            origin: Some(SkillOrigin::Mcp {
                server: server.to_string(),
                uri: format!("skill://{server}/beta/SKILL.md"),
            }),
            content: Some("# Beta\n\nBody\n".to_string()),
            resources: Vec::new(),
            frontmatter: None,
        };
        reg.mark_discovery_started(server, handle.clone());
        reg.mark_discovery_completed(server, handle, vec![meta]);
    }
    let mw = middleware(reg);
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/beta 歧义命令"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(
        state.messages().len(),
        1,
        "歧义命中必须显式拒绝：不注入任何消息（仅原始 Human）"
    );
}

/// SubAgent 路径（显式 `skill_names`）：与主链同一条 registry 查找路径。
#[tokio::test]
async fn test_explicit_skill_names_path_uses_registry() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec!["brainstorming".to_string()])
        .with_mcp_registry(Some(seed_registry_with_skill("workspace", "brainstorming")));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "显式列表命中应注入");
    assert!(state.messages()[2]
        .content()
        .contains("Body of brainstorming."));
}
