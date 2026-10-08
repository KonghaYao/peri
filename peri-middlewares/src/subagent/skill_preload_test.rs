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

/// 测试用批次适配器：`AgentState` + 本批输入身份。
///
/// 生产由 `AgentContext` 提供批次身份（`input_message_ids`）；`AgentState` 作为
/// legacy 适配器恒返回 `None`（等价「本次没有新输入」），因此主 Agent 启发式
/// 路径的用例统一用本适配器显式声明本批身份（H7：不扫描历史最后一条 Human）。
struct BatchState {
    inner: AgentState,
    ids: Vec<peri_agent::messages::MessageId>,
}

impl BatchState {
    /// 全部给定消息都算作本批输入。
    fn new(cwd: &str, messages: Vec<BaseMessage>) -> Self {
        let mut inner = AgentState::new(cwd);
        let mut ids = Vec::new();
        for message in messages {
            ids.push(message.id());
            inner.add_message(message);
        }
        Self { inner, ids }
    }

    fn messages(&self) -> &[BaseMessage] {
        self.inner.messages()
    }
}

impl std::ops::Deref for BatchState {
    type Target = AgentState;
    fn deref(&self) -> &AgentState {
        &self.inner
    }
}

impl peri_agent::middleware::state::MiddlewareState for BatchState {
    fn cwd(&self) -> &str {
        self.inner.cwd()
    }
    fn messages(&self) -> &[BaseMessage] {
        self.inner.messages()
    }
    fn input_message_ids(&self) -> Option<&[peri_agent::messages::MessageId]> {
        Some(&self.ids)
    }
    fn add_message(&mut self, message: BaseMessage) {
        self.inner.add_message(message);
    }
    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = self
            .inner
            .messages_mut()
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        true
    }
    fn current_step(&self) -> usize {
        self.inner.current_step()
    }
    fn push_recall(&mut self, item: String) {
        self.inner.push_recall(item);
    }
    fn drain_recall(&mut self) -> Vec<String> {
        self.inner.drain_recall()
    }
    fn v2_queue(&self) -> &peri_agent::session::MessageQueue {
        self.inner.v2_queue()
    }
}

/// seed registry：同一 server 下多个技能（Started + Completed 造 Discovered 条目，
/// 模拟发现任务完成态）。条目无 resources 绑定 ⇒ activation 走 legacy 正文路径。
fn seed_registry_with_skills(server: &str, skills: &[&str]) -> Arc<McpSkillRegistry> {
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    let metas = skills
        .iter()
        .map(|skill| SkillMetadata {
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
        })
        .collect();
    reg.mark_discovery_started(server, handle.clone());
    reg.mark_discovery_completed(server, handle, metas);
    reg
}

/// seed registry：单个技能（见 [`seed_registry_with_skills`]）。
fn seed_registry_with_skill(server: &str, skill: &str) -> Arc<McpSkillRegistry> {
    seed_registry_with_skills(server, &[skill])
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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("hello there")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "无 skill token 时不注入");
}

#[tokio::test]
async fn test_no_op_when_no_human_message() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = BatchState::new(dir.path().to_str().unwrap(), vec![]);

    mw.before_input(&mut state).await.unwrap();

    assert!(state.messages().is_empty());
}

/// registry 命中：注入 `Human → Ai(tool_use SkillTool) → Tool(tool_result)`，
/// 正文带来源标注（与 SkillTool / 命令面同源）。
#[tokio::test]
async fn test_preload_mcp_skill_injects_annotated_content() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("use /mcp__demo__hello")],
    );

    mw.before_input(&mut state).await.unwrap();

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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/brainstorming 帮我")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "裸名应命中 registry");
    assert!(state.messages()[2]
        .content()
        .contains("Body of brainstorming."));
}

#[tokio::test]
async fn test_system_skill_old_prefixed_token_is_not_auto_preloaded() {
    let dir = tempdir().unwrap();
    let reg = seed_registry_with_skill("workspace", "brainstorming");
    reg.mark_system_origins(&["workspace".to_string()]);
    let mw = middleware(Arc::clone(&reg));
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/workspace:brainstorming")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "旧前缀 token 不得激活系统 skill");
}

#[tokio::test]
async fn test_explicit_system_skill_name_keeps_preload_semantics() {
    let dir = tempdir().unwrap();
    let reg = seed_registry_with_skill("workspace", "brainstorming");
    reg.mark_system_origins(&["workspace".to_string()]);
    let mw = SkillPreloadMiddleware::new(vec!["workspace:brainstorming".to_string()])
        .with_mcp_registry(Some(reg));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("run task"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "显式名单维持旧解析语义");
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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/beta /alpha")],
    );

    mw.before_input(&mut state).await.unwrap();

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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/miss-a /mcp__demo__hello /miss-b")],
    );

    mw.before_input(&mut state).await.unwrap();

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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/demo:hello 帮我")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "别名命中应注入 Ai + Tool");
    assert!(state.messages()[2].content().contains("Body of hello."));
}

/// plugin 多冒号 server key（`plugin:p1:demosrv`）下 `/demosrv:beta`
/// 按 server 名末段命中（与命令面 fullname 派生同构）。
#[tokio::test]
async fn test_preload_plugin_server_via_trailing_segment() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("plugin:p1:demosrv", "beta"));
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/demosrv:beta 帮我")],
    );

    mw.before_input(&mut state).await.unwrap();

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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/anything 帮我")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 1, "未装配 registry ⇒ 只保留 Human");
}

/// registry miss：缺口语义（不注入、不回落磁盘）。
#[tokio::test]
async fn test_registry_miss_injects_nothing() {
    let dir = tempdir().unwrap();
    let mw = middleware(seed_registry_with_skill("demo", "hello"));
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/nonexistent 不存在")],
    );

    mw.before_input(&mut state).await.unwrap();

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
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human("/beta 歧义命令")],
    );

    mw.before_input(&mut state).await.unwrap();

    assert_eq!(
        state.messages().len(),
        1,
        "歧义命中必须显式拒绝：不注入任何消息（仅原始 Human）"
    );
}

/// 宿主显式名单路径（显式 `skill_names`）：与主链同一条 registry 查找路径。
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

// ─── 宿主显式名单路径的缺口注入（W6：子代理 / workflow）───────────────────

/// Tool 消息回执 `(tool_call_id, 正文, is_error)`（按消息顺序）。
fn tool_receipts(state: &AgentState) -> Vec<(String, String, bool)> {
    state
        .messages()
        .iter()
        .filter_map(|message| match message {
            BaseMessage::Tool {
                tool_call_id,
                is_error,
                ..
            } => Some((tool_call_id.clone(), message.content(), *is_error)),
            _ => None,
        })
        .collect()
}

/// 显式名单路径（子代理 / workflow）+ registry 未装配：每个声明名各一对
/// ToolUse/tool_error（按声明顺序）。
#[tokio::test]
async fn test_explicit_list_path_unwired_registry_injects_gap_receipts() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec!["alpha".to_string(), "beta".to_string()]);
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 4, "1 Human + Ai + 2 缺口回执");
    let calls = state.messages()[1].tool_calls();
    let names: Vec<_> = calls
        .iter()
        .map(|call| call.arguments["skill_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"], "ToolUse 顺序 = 声明顺序");

    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 2);
    let call_ids: Vec<_> = calls.iter().map(|call| call.id.clone()).collect();
    let receipt_ids: Vec<_> = receipts.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(receipt_ids, call_ids, "ToolUse 与回执一一配对");
    for (_, text, is_error) in &receipts {
        assert!(*is_error, "缺口回执必须 is_error = true");
        assert!(
            text.contains("MCP skill registry is not wired"),
            "复用 SkillTool 装配缺口产品串，实际: {text}"
        );
    }
    assert!(receipts[0].1.contains("cannot activate 'alpha'"));
    assert!(receipts[1].1.contains("cannot activate 'beta'"));
}

/// 显式名单路径 + registry 未命中：tool_error 携带 not found 产品串。
#[tokio::test]
async fn test_explicit_list_path_miss_injects_not_found_receipt() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec!["missing-skill-x".to_string()])
        .with_mcp_registry(Some(seed_registry_with_skill("demo", "hello")));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "1 Human + Ai + 1 缺口回执");
    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].2, "未命中回执必须 is_error = true");
    assert!(
        receipts[0].1.contains(
            "Skill 'missing-skill-x' not found. Use DiscoverSkillsTool to see available skills."
        ),
        "复用 SkillTool 未命中产品串，实际: {}",
        receipts[0].1
    );
}

/// 显式名单路径 + 跨 origin 歧义：回执与 `SkillTool` 歧义文案同构（含候选清单）。
#[tokio::test]
async fn test_explicit_list_path_ambiguous_receipt_lists_candidates() {
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
    let mw = SkillPreloadMiddleware::new(vec!["beta".to_string()]).with_mcp_registry(Some(reg));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "歧义 ⇒ 缺口回执，不注入任何正文");
    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].2, "歧义回执必须 is_error = true");
    let text = &receipts[0].1;
    assert!(
        text.contains("is ambiguous across 2 origins"),
        "实际: {text}"
    );
    assert!(
        text.contains("a:srv:beta") && text.contains("b:srv:beta"),
        "实际: {text}"
    );
    assert!(text.contains("disambiguate"), "实际: {text}");
}

/// 显式名单路径命中与缺口混合：顺序 = 声明顺序，配对与 is_error 分类正确。
#[tokio::test]
async fn test_explicit_list_path_mixed_hits_and_gaps_keep_declaration_order() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec![
        "beta".to_string(),
        "missing-x".to_string(),
        "alpha".to_string(),
    ])
    .with_mcp_registry(Some(seed_registry_with_skills("demo", &["alpha", "beta"])));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(
        state.messages().len(),
        5,
        "1 Human + Ai + 3 结果（命中/缺口/命中）"
    );
    let calls = state.messages()[1].tool_calls();
    let names: Vec<_> = calls
        .iter()
        .map(|call| call.arguments["skill_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["beta", "missing-x", "alpha"], "顺序 = 声明顺序");

    let receipts = tool_receipts(&state);
    let call_ids: Vec<_> = calls.iter().map(|call| call.id.clone()).collect();
    let receipt_ids: Vec<_> = receipts.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(receipt_ids, call_ids, "ToolUse 与结果一一配对");
    let errors: Vec<_> = receipts.iter().map(|(_, _, is_error)| *is_error).collect();
    assert_eq!(errors, vec![false, true, false], "仅缺口项 is_error = true");
    assert!(receipts[0].1.contains("Body of beta."));
    assert!(receipts[1].1.contains("'missing-x' not found"));
    assert!(receipts[2].1.contains("Body of alpha."));
}

/// 显式名单路径 + activation 失败（条目无内容绑定）：回执携带原因文本。
#[tokio::test]
async fn test_explicit_list_path_activation_failure_receipt_carries_reason() {
    let dir = tempdir().unwrap();
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: mcp_skill_name("demo", "detached"),
        aliases: Vec::new(),
        description: "Detached skill".to_string(),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "demo".to_string(),
            uri: "skill://demo/detached/SKILL.md".to_string(),
        }),
        // 无正文 = 无内容绑定 ⇒ activation 失败（MissingBinding），且不回落任何来源
        content: None,
        resources: Vec::new(),
        frontmatter: None,
    };
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed("demo", handle, vec![meta]);
    let mw = SkillPreloadMiddleware::new(vec!["detached".to_string()]).with_mcp_registry(Some(reg));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 3, "激活失败 ⇒ 缺口回执，不注入正文");
    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].2, "激活失败回执必须 is_error = true");
    // 回执正文 = `SkillTool` activation 失败产品串（`crate::skills` 单一派生点）。
    assert_eq!(
        receipts[0].1,
        "SkillTool: cannot activate 'detached' (skill entry has no content binding)"
    );
}

// ─── M8：批内去重与预载预算 ───────────────────────────────────────────────

/// 同一 canonical skill（全名 + 别名）批内只加载一次，保持首次出现顺序。
#[tokio::test]
async fn test_explicit_list_path_dedupes_same_canonical_skill() {
    let dir = tempdir().unwrap();
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: mcp_skill_name("demo", "alpha"),
        aliases: vec!["alpha".to_string()],
        description: "Alpha skill".to_string(),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "demo".to_string(),
            uri: "skill://demo/alpha/SKILL.md".to_string(),
        }),
        content: Some("alpha body\n".to_string()),
        resources: Vec::new(),
        frontmatter: None,
    };
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed("demo", handle, vec![meta]);
    let mw = SkillPreloadMiddleware::new(vec!["mcp__demo__alpha".to_string(), "alpha".to_string()])
        .with_mcp_registry(Some(reg));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    let calls = state.messages()[1].tool_calls();
    assert_eq!(
        calls.len(),
        1,
        "同一 canonical 身份只读一次、只注入一次：{calls:?}"
    );
    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].1.contains("alpha body"));
}

/// 条数超限：超出部分**不读正文**，改为一一配对的缺口回执（不静默丢弃）。
#[tokio::test]
async fn test_explicit_list_path_item_budget_yields_paired_gap_receipts() {
    let dir = tempdir().unwrap();
    let skills: Vec<String> = (0..18).map(|index| format!("s{index:02}")).collect();
    let refs: Vec<&str> = skills.iter().map(String::as_str).collect();
    let mw = SkillPreloadMiddleware::new(skills.clone())
        .with_mcp_registry(Some(seed_registry_with_skills("demo", &refs)));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 18, "18 条声明各一对回执（含超限项）");
    let errors = receipts.iter().filter(|(_, _, is_error)| *is_error).count();
    assert_eq!(errors, 2, "仅超限的 2 项是缺口：{receipts:?}");
    assert!(
        receipts[17].1.contains("batch item limit"),
        "超限回执必须说明原因：{}",
        receipts[17].1
    );
    assert!(
        receipts[0].1.contains("Body of s00."),
        "预算内条目正文完整（不加载截断正文）：{}",
        receipts[0].1
    );
}

/// 单项字节超预算：不加载截断正文冒充完整指令，改为缺口回执。
#[tokio::test]
async fn test_explicit_list_path_item_bytes_yield_gap_receipt() {
    let dir = tempdir().unwrap();
    let reg = Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = Arc::new(1u32);
    let meta = SkillMetadata {
        name: mcp_skill_name("demo", "huge"),
        aliases: Vec::new(),
        description: "Huge skill".to_string(),
        path: std::path::PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "demo".to_string(),
            uri: "skill://demo/huge/SKILL.md".to_string(),
        }),
        content: Some("x".repeat(MAX_PRELOAD_ITEM_BYTES + 1)),
        resources: Vec::new(),
        frontmatter: None,
    };
    reg.mark_discovery_started("demo", handle.clone());
    reg.mark_discovery_completed("demo", handle, vec![meta]);
    let mw = SkillPreloadMiddleware::new(vec!["huge".to_string()]).with_mcp_registry(Some(reg));
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("普通消息（无 token）"));

    mw.before_agent(&mut state).await.unwrap();

    let receipts = tool_receipts(&state);
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].2, "超预算必须是缺口回执");
    assert!(
        receipts[0].1.contains("exceeds byte budget"),
        "回执必须说明是字节预算：{}",
        receipts[0].1
    );
    assert!(
        !receipts[0].1.contains("xxxx"),
        "不得注入截断正文冒充完整指令"
    );
}

/// 启发式路径：已识别但被预算挡下的技能必须显式提示（不假装未声明）；
/// 未识别 token 仍零注入（不制造未知技能错误）。
#[tokio::test]
async fn test_heuristic_path_reports_budget_gaps_but_not_unknown_tokens() {
    let dir = tempdir().unwrap();
    let skills: Vec<String> = (0..18).map(|index| format!("h{index:02}")).collect();
    let refs: Vec<&str> = skills.iter().map(String::as_str).collect();
    let mut tokens: Vec<String> = skills.iter().map(|name| format!("/{name}")).collect();
    tokens.push("/never-declared".to_string());
    let mw = middleware(seed_registry_with_skills("demo", &refs));
    let mut state = BatchState::new(
        dir.path().to_str().unwrap(),
        vec![BaseMessage::human(tokens.join(" "))],
    );

    mw.before_input(&mut state).await.unwrap();

    let calls: Vec<String> = state
        .messages()
        .iter()
        .flat_map(|message| message.tool_calls())
        .map(|call| call.arguments["skill_name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !calls.iter().any(|name| name == "never-declared"),
        "未识别 token 不得注入（不制造未知技能错误）：{calls:?}"
    );
    assert!(
        calls.iter().any(|name| name == "h17"),
        "已识别但被预算挡下的技能必须显式提示：{calls:?}"
    );
    let receipts = tool_receipts(&state);
    let budget_gap = receipts
        .iter()
        .find(|(_, text, _)| text.contains("batch item limit"));
    assert!(budget_gap.is_some(), "预算缺口回执必须存在：{receipts:?}");
}
