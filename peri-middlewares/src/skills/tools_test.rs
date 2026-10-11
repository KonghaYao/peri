//! SkillTool + DiscoverSkillsTool 单元测试（W4b 后：数据源只有 MCP registry）。
//!
//! 覆盖面：工具元数据、registry 当前投影内的按名查找与歧义裁决、
//! DiscoverSkillsTool 的 JSON 投影与来源标签、以及目录状态（L2）的显式区分：
//! **未装配 ⇒ 稳定可操作的目录不可用错误**（不再泄露内部实现串）、
//! **发现未收口 ⇒ 未就绪**（不把空投影当空目录）、**合法空目录 ⇒ `[]`**。
//! ——**不再有任何本地磁盘用例**（宿主已无技能 FS 读取点，J5）。
//!
//! 正文读取（统一 activation：`resources/read` + digest/frontmatter 校验）
//! 由 `crate::mcp::skill_activation_test.rs` 与 `peri-acp` 的端到端用例锚定，
//! 本文件不重复构造假 peer。

use super::*;
use peri_acp_types::mcp_skills::{mcp_skill_name, HandleToken, McpSkillRegistry};
use peri_acp_types::skills::{SkillMetadata, SkillOrigin, SkillSource};
use serde_json::json;
use std::sync::Arc;

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

/// registry 已发现（收口）状态。
fn registry_with(entries: Vec<SkillMetadata>) -> Arc<McpSkillRegistry> {
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed("demo", handle, entries);
    reg
}

/// registry 装配但发现**未收口**（Started 未完成）：目录「初始化中」。
fn registry_discovering() -> Arc<McpSkillRegistry> {
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    reg.mark_discovery_started("demo", handle);
    reg
}

fn ctx() -> peri_agent::tools::ToolContext<'static> {
    peri_agent::tools::ToolContext::new(&[], ".")
}

// ─── 工具元数据 ──────────────────────────────────────────────────────────────

#[test]
fn skill_tool_metadata_is_stable() {
    let tool = SkillTool::new(None);
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
    let tool = DiscoverSkillsTool::new(None);
    assert_eq!(tool.name(), "DiscoverSkillsTool");
    assert!(tool.is_direct());
    assert_eq!(tool.namespace(), Some("skills"));
    assert!(tool.parameters()["required"] == json!([]));
}

// ─── 查找语义（调用时读取 registry 投影）────────────────────────────────────

#[tokio::test]
async fn skill_tool_requires_skill_name() {
    let tool = SkillTool::new(None);
    let error = tool.invoke(json!({}), ctx()).await.unwrap_err();
    assert!(error.to_string().contains("missing required parameter"));
}

/// 未装配 registry ⇒ 稳定可操作的「目录不可用」，不泄露内部实现串（L2）。
#[tokio::test]
async fn skill_tool_without_registry_reports_stable_unavailable_message() {
    let tool = SkillTool::new(None);
    let error = tool
        .invoke(json!({"skill_name": "anything"}), ctx())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("skill catalog is not available"),
        "unexpected message: {message}"
    );
    assert!(
        !message.contains("before_agent") && !message.contains("cache"),
        "不得泄露内部实现串: {message}"
    );
}

/// 发现未收口 ⇒ 目录未就绪，不返回假空成功、也不误报 not found（L2）。
#[tokio::test]
async fn skill_tool_reports_initializing_catalog() {
    let tool = SkillTool::new(Some(registry_discovering()));
    let error = tool
        .invoke(json!({"skill_name": "anything"}), ctx())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("still initializing"),
        "unexpected message: {message}"
    );
    assert!(!message.contains("not found"), "unexpected: {message}");
}

/// 合法空目录（发现已收口、条目为空）：按 not-found 回答，不谎称未就绪。
#[tokio::test]
async fn skill_tool_empty_catalog_reports_not_found() {
    let tool = SkillTool::new(Some(registry_with(vec![])));
    let error = tool
        .invoke(json!({"skill_name": "anything"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"));
}

/// registry 未装配时不再回落任何来源（正文读取入口由 activation 唯一承担）：
/// 命中判断自身也需要 registry，因此必须在目录不可用处停下。
#[tokio::test]
async fn skill_tool_without_registry_never_reports_not_found() {
    let tool = SkillTool::new(None);
    let error = tool
        .invoke(json!({"skill_name": "brain"}), ctx())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("not found"));
}

#[tokio::test]
async fn skill_tool_unknown_name_lists_discovery_hint() {
    let tool = SkillTool::new(Some(registry_with(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )])));
    let error = tool
        .invoke(json!({"skill_name": "nope"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"));
}

/// 全名 `mcp__server__skill` 精确命中（大小写不敏感）：命中后进入 activation
/// （本用例的条目无内容绑定 ⇒ activation 失败，但错误必须**不是** not found）。
#[tokio::test]
async fn skill_tool_resolves_full_name_case_insensitively() {
    let tool = SkillTool::new(Some(registry_with(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )])));
    let error = tool
        .invoke(json!({"skill_name": "mcp__WORKSPACE__Brain"}), ctx())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("cannot activate"), "unexpected: {message}");
    assert!(!message.contains("not found"));
}

/// `<server>:<skill>` 别名形态（与 `/server:skill` 命令同构）。
#[tokio::test]
async fn skill_tool_resolves_server_alias() {
    let tool = SkillTool::new(Some(registry_with(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )])));
    let error = tool
        .invoke(json!({"skill_name": "workspace:brain"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot activate"));
}

/// 裸名命中（`/skill-name` 与 SkillTool(bare) 同口径）。
#[tokio::test]
async fn skill_tool_resolves_bare_name() {
    let tool = SkillTool::new(Some(registry_with(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )])));
    let error = tool
        .invoke(json!({"skill_name": "brain"}), ctx())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot activate"));
}

/// 跨 origin 同名：显式歧义错误 + 候选清单（不静默取首个）。
#[tokio::test]
async fn skill_tool_ambiguous_across_origins_lists_candidates() {
    let reg = Arc::new(McpSkillRegistry::new());
    for server in ["workspace", "remote"] {
        let handle: HandleToken = Arc::new(server.to_string());
        reg.mark_discovery_started(server, handle.clone());
        reg.mark_discovery_completed(
            server,
            handle,
            vec![fake_mcp_skill(server, "project", "brain")],
        );
    }
    let tool = SkillTool::new(Some(reg));
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
    let tool = DiscoverSkillsTool::new(Some(registry_with(vec![
        fake_mcp_skill("workspace", "project", "brain"),
        fake_mcp_skill("workspace", "builtin", "example"),
        fake_mcp_skill("workspace", "user", "guide"),
    ])));
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
    let tool = DiscoverSkillsTool::new(Some(registry_with(vec![
        fake_mcp_skill("workspace", "project", "brainstorming"),
        fake_mcp_skill("workspace", "project", "code-review"),
    ])));
    let output = tool.invoke(json!({"query": "BRAIN"}), ctx()).await.unwrap();
    let skills: Value = serde_json::from_str(&output).unwrap();
    let items = skills.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0]["name"].as_str().unwrap().contains("brainstorming"));
}

#[tokio::test]
async fn discover_tool_query_without_match_returns_empty_array() {
    let tool = DiscoverSkillsTool::new(Some(registry_with(vec![fake_mcp_skill(
        "workspace",
        "project",
        "brain",
    )])));
    let output = tool
        .invoke(json!({"query": "nothing-matches"}), ctx())
        .await
        .unwrap();
    assert_eq!(output, "[]");
}

/// 合法空目录（发现已收口、无条目）⇒ `[]`，不是错误（L2）。
#[tokio::test]
async fn discover_tool_empty_catalog_returns_empty_array() {
    let tool = DiscoverSkillsTool::new(Some(registry_with(vec![])));
    assert_eq!(tool.invoke(json!({}), ctx()).await.unwrap(), "[]");
}

/// 发现未收口 ⇒ 未就绪错误，**不**返回假空成功（L2）。
#[tokio::test]
async fn discover_tool_initializing_catalog_is_not_a_false_empty() {
    let tool = DiscoverSkillsTool::new(Some(registry_discovering()));
    let error = tool.invoke(json!({}), ctx()).await.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("still initializing"),
        "unexpected: {message}"
    );
}

/// 未装配 ⇒ 稳定可操作的目录不可用（不泄露内部实现串、不返回假空成功）。
#[tokio::test]
async fn discover_tool_without_registry_reports_stable_unavailable_message() {
    let tool = DiscoverSkillsTool::new(None);
    let error = tool.invoke(json!({}), ctx()).await.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("skill catalog is not available"),
        "unexpected: {message}"
    );
    assert!(!message.contains("before_agent"), "unexpected: {message}");
}

/// 工具面读取的是**调用时**投影：同一条目在 registry 增删后结果随之变化。
#[tokio::test]
async fn discover_tool_reflects_catalog_changes_at_call_time() {
    let reg = registry_with(vec![fake_mcp_skill("workspace", "project", "alpha")]);
    let tool = DiscoverSkillsTool::new(Some(Arc::clone(&reg)));
    let first = tool.invoke(json!({}), ctx()).await.unwrap();
    assert!(first.contains("alpha"));

    let handle: HandleToken = Arc::new(2u32);
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed(
        "demo",
        handle,
        vec![fake_mcp_skill("workspace", "project", "beta")],
    );
    let second = tool.invoke(json!({}), ctx()).await.unwrap();
    assert!(second.contains("beta"), "unexpected: {second}");
    assert!(!second.contains("alpha"), "不读旧快照: {second}");
}
