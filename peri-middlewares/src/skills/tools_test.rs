//! SkillTool + DiscoverSkillsTool 单元测试（W4b 后：数据源只有 MCP registry）。
//!
//! 覆盖面：工具元数据、缓存（registry 投影）内的按名查找与歧义裁决、
//! DiscoverSkillsTool 的 JSON 投影与来源标签、以及「未装配 registry ⇒ 显式
//! 装配缺口」——**不再有任何本地磁盘用例**（宿主已无技能 FS 读取点，J5）。
//!
//! 正文读取（统一 activation：`resources/read` + digest/frontmatter 校验）
//! 由 `crate::mcp::skill_activation_test.rs` 与 `peri-acp` 的端到端用例锚定，
//! 本文件不重复构造假 peer。

use super::*;
use peri_acp_types::mcp_skills::mcp_skill_name;
use peri_acp_types::skills::{SkillOrigin, SkillSource};
use serde_json::json;
use std::sync::{Arc, RwLock};

/// 构造一个 MCP 来源条目（workspace 形状的 URI：`skill://{scope}/{name}/SKILL.md`）。
fn fake_mcp_skill(server: &str, scope: &str, skill: &str) -> SkillMetadata {
    SkillMetadata {
        name: mcp_skill_name(server, skill),
        aliases: Vec::new(),
        description: format!("MCP skill {skill}"),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: format!("skill://{scope}/{skill}/SKILL.md"),
        }),
        content: None,
        resources: Vec::new(),
        frontmatter: None,
    }
}

fn cached(entries: Vec<SkillMetadata>) -> Arc<RwLock<Option<Vec<SkillMetadata>>>> {
    Arc::new(RwLock::new(Some(entries)))
}

fn ctx() -> peri_agent::tools::ToolContext<'static> {
    peri_agent::tools::ToolContext::new(&[], ".")
}

// ─── 工具元数据 ──────────────────────────────────────────────────────────────

#[test]
fn skill_tool_metadata_is_stable() {
    let tool = SkillTool::new(cached(vec![]), None);
    assert_eq!(tool.name(), "SkillTool");
    assert!(tool.is_direct());
    assert_eq!(tool.namespace(), Some("skills"));
    assert!(tool
        .description()
        .contains("Load the full content of a skill"));
    let params = tool.parameters();
    assert_eq!(params["required"], json!(["skill_name"]));
}

#[test]
fn discover_tool_metadata_is_stable() {
    let tool = DiscoverSkillsTool::new(cached(vec![]));
    assert_eq!(tool.name(), "DiscoverSkillsTool");
    assert!(tool.is_direct());
    assert_eq!(tool.namespace(), Some("skills"));
    assert!(tool.parameters()["required"] == json!([]));
}

// ─── 查找语义（缓存即 registry 投影）────────────────────────────────────────

#[tokio::test]
async fn skill_tool_requires_skill_name() {
    let tool = SkillTool::new(cached(vec![]), None);
    let error = tool.invoke(json!({}), ctx()).await.unwrap_err();
    assert!(error.to_string().contains("missing required parameter"));
}

#[tokio::test]
async fn skill_tool_empty_cache_is_reported_as_not_ready() {
    let tool = SkillTool::new(Arc::new(RwLock::new(None)), None);
    let error = tool
        .invoke(json!({"skill_name": "anything"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Skills cache is empty"));
}

/// 未装配 registry ⇒ 显式装配缺口（不读缓存正文、不读磁盘）。
#[tokio::test]
async fn skill_tool_without_registry_reports_assembly_gap() {
    let tool = SkillTool::new(
        cached(vec![fake_mcp_skill("workspace", "project", "brain")]),
        None,
    );
    let error = tool
        .invoke(json!({"skill_name": "brain"}), ctx())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("registry is not wired"),
        "unexpected message: {message}"
    );
}

#[tokio::test]
async fn skill_tool_unknown_name_lists_discovery_hint() {
    let tool = SkillTool::new(
        cached(vec![fake_mcp_skill("workspace", "project", "brain")]),
        None,
    );
    let error = tool
        .invoke(json!({"skill_name": "nope"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"));
}

/// 全名 `mcp__server__skill` 精确命中（大小写不敏感）。
#[tokio::test]
async fn skill_tool_resolves_full_name_case_insensitively() {
    let tool = SkillTool::new(
        cached(vec![fake_mcp_skill("workspace", "project", "brain")]),
        None,
    );
    // 命中即进入 registry 装配检查（本用例只验证「查找命中」这一步：
    // 未装配 registry 时报装配缺口而不是 not found）。
    let error = tool
        .invoke(json!({"skill_name": "mcp__WORKSPACE__Brain"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("registry is not wired"));
}

/// `<server>:<skill>` 别名形态（与 `/server:skill` 命令同构）。
#[tokio::test]
async fn skill_tool_resolves_server_alias() {
    let tool = SkillTool::new(
        cached(vec![fake_mcp_skill("workspace", "project", "brain")]),
        None,
    );
    let error = tool
        .invoke(json!({"skill_name": "workspace:brain"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("registry is not wired"));
}

/// 裸名命中（`/skill-name` 与 SkillTool(bare) 同口径）。
#[tokio::test]
async fn skill_tool_resolves_bare_name() {
    let tool = SkillTool::new(
        cached(vec![fake_mcp_skill("workspace", "project", "brain")]),
        None,
    );
    let error = tool
        .invoke(json!({"skill_name": "brain"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("registry is not wired"));
}

/// 跨 origin 同名：显式歧义错误 + 候选清单（不静默取首个）。
#[tokio::test]
async fn skill_tool_ambiguous_across_origins_lists_candidates() {
    let tool = SkillTool::new(
        cached(vec![
            fake_mcp_skill("workspace", "project", "brain"),
            fake_mcp_skill("remote", "user", "brain"),
        ]),
        None,
    );
    let error = tool
        .invoke(json!({"skill_name": "brain"}), ctx())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("ambiguous"), "unexpected: {message}");
    assert!(message.contains("remote:brain"), "unexpected: {message}");
    assert!(message.contains("workspace:brain"), "unexpected: {message}");
}

// ─── DiscoverSkillsTool 投影 ────────────────────────────────────────────────

#[tokio::test]
async fn discover_tool_reports_all_entries_with_scope_labels() {
    let tool = DiscoverSkillsTool::new(cached(vec![
        fake_mcp_skill("workspace", "project", "brain"),
        fake_mcp_skill("workspace", "builtin", "example"),
        fake_mcp_skill("workspace", "user", "guide"),
    ]));
    let output = tool.invoke(json!({}), ctx()).await.unwrap();
    let skills: Value = serde_json::from_str(&output).unwrap();
    let labels: Vec<&str> = skills
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"project"));
    assert!(labels.contains(&"builtin"));
    assert!(labels.contains(&"user"));
}

#[tokio::test]
async fn discover_tool_filters_by_query_case_insensitively() {
    let tool = DiscoverSkillsTool::new(cached(vec![
        fake_mcp_skill("workspace", "project", "brainstorming"),
        fake_mcp_skill("workspace", "project", "code-review"),
    ]));
    let output = tool.invoke(json!({"query": "BRAIN"}), ctx()).await.unwrap();
    let skills: Value = serde_json::from_str(&output).unwrap();
    let items = skills.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0]["name"].as_str().unwrap().contains("brainstorming"));
}

#[tokio::test]
async fn discover_tool_query_without_match_returns_empty_array() {
    let tool = DiscoverSkillsTool::new(cached(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )]));
    let output = tool
        .invoke(json!({"query": "nothing-matches"}), ctx())
        .await
        .unwrap();
    assert_eq!(output, "[]");
}

#[tokio::test]
async fn discover_tool_empty_cache_reports_not_ready() {
    let tool = DiscoverSkillsTool::new(Arc::new(RwLock::new(None)));
    let error = tool.invoke(json!({}), ctx()).await.unwrap_err();
    assert!(error.to_string().contains("Skills cache is empty"));
}
