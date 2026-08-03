//! Tests for skill_preload

use peri_agent::{agent::state::AgentState, middleware::r#trait::Middleware};
use tempfile::tempdir;

use super::*;

fn write_skill(dir: &std::path::Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = format!(
        "---\nname: '{}'\ndescription: '{}'\n---\n\n# {}\n\nSkill content for {}.\n",
        name, desc, name, name
    );
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
}

#[tokio::test]
async fn test_no_op_when_empty_names() {
    // Arrange
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec![], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert
    assert_eq!(state.messages().len(), 0);
}

#[tokio::test]
async fn test_inject_single_skill() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "api-guide", "API 开发指南");

    let mw =
        SkillPreloadMiddleware::new(vec!["api-guide".to_string()], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert: Ai + Tool = 2 条消息
    assert_eq!(state.messages().len(), 2, "应注入 2 条消息（Ai + Tool）");
    assert!(
        matches!(&state.messages()[0], BaseMessage::Ai { .. }),
        "第一条应为 Ai"
    );
    assert!(
        matches!(&state.messages()[1], BaseMessage::Tool { .. }),
        "第二条应为 Tool"
    );
    // D3：注入的 ToolUse 与统一模型可见协议一致（SkillTool(skill_name)，
    // 旧 Skill(skill, args) 已移除）
    let tc = &state.messages()[0].tool_calls()[0];
    assert_eq!(tc.name, "SkillTool", "注入的 ToolUse 应名为 SkillTool");
    assert_eq!(
        tc.arguments["skill_name"].as_str(),
        Some("api-guide"),
        "ToolUse input 应携带 skill_name"
    );
    assert!(
        tc.arguments.get("skill").is_none(),
        "不应再使用旧参数名 skill"
    );
}

#[tokio::test]
async fn test_inject_multiple_skills() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "skill-a", "技能 A");
    write_skill(&skills_dir, "skill-b", "技能 B");
    write_skill(&skills_dir, "skill-c", "技能 C");

    let mw = SkillPreloadMiddleware::new(
        vec![
            "skill-a".to_string(),
            "skill-b".to_string(),
            "skill-c".to_string(),
        ],
        dir.path().to_str().unwrap(),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert: Ai + Tool × 3 = 4 条消息
    assert_eq!(state.messages().len(), 4, "3 个 skill 应注入 4 条消息");
}

#[tokio::test]
async fn test_skip_missing_skill() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "exists", "存在的 skill");

    let mw = SkillPreloadMiddleware::new(
        vec!["exists".to_string(), "nonexistent".to_string()],
        dir.path().to_str().unwrap(),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert: 只有 "exists" → Ai + Tool = 2 条
    assert_eq!(state.messages().len(), 2, "不存在的 skill 应静默跳过");
}

#[tokio::test]
async fn test_no_op_when_all_skills_missing() {
    // Arrange
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(
        vec!["nonexistent".to_string()],
        dir.path().to_str().unwrap(),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert
    assert_eq!(state.messages().len(), 0, "全部找不到时应 no-op");
}

#[tokio::test]
async fn test_message_order() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "skill-x", "技能 X");
    write_skill(&skills_dir, "skill-y", "技能 Y");

    let mw = SkillPreloadMiddleware::new(
        vec!["skill-x".to_string(), "skill-y".to_string()],
        dir.path().to_str().unwrap(),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert
    let msgs = state.messages();
    assert!(
        matches!(&msgs[0], BaseMessage::Ai { .. }),
        "messages[0] 应为 Ai"
    );
    assert!(msgs[0].has_tool_calls(), "Ai 消息应包含工具调用");
    assert_eq!(msgs[0].tool_calls().len(), 2, "Ai 消息应有 2 个工具调用");
    assert!(
        matches!(&msgs[1], BaseMessage::Tool { .. }),
        "messages[1] 应为 Tool"
    );
    assert!(
        matches!(&msgs[2], BaseMessage::Tool { .. }),
        "messages[2] 应为 Tool"
    );
}

#[tokio::test]
async fn test_tool_call_ids_match() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "my-skill", "My skill");

    let mw =
        SkillPreloadMiddleware::new(vec!["my-skill".to_string()], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert
    let msgs = state.messages();
    let ai_id = &msgs[0].tool_calls()[0].id;
    if let BaseMessage::Tool { tool_call_id, .. } = &msgs[1] {
        assert_eq!(
            tool_call_id, ai_id,
            "Tool 消息的 tool_call_id 应与 Ai 消息一致"
        );
    } else {
        unreachable!("messages[1] 应为 Tool");
    }
}

#[tokio::test]
async fn test_tool_result_contains_skill_content() {
    // Arrange
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "commit-skill", "提交技能");

    let mw = SkillPreloadMiddleware::new(
        vec!["commit-skill".to_string()],
        dir.path().to_str().unwrap(),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert
    let tool_content = state.messages()[1].content();
    assert!(
        tool_content.contains("Skill content for commit-skill"),
        "Tool 结果应包含 skill 全文内容"
    );
}

#[tokio::test]
async fn test_auto_detect_skill_from_human_message() {
    // Arrange: 模拟主 Agent 场景——skill_names 为空，但 state 中有包含 /skill-name 的 Human 消息
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    // 使用不与全局 ~/.claude/skills/ 冲突的名称
    write_skill(&skills_dir, "test-diagnose-auto", "自动检测技能");

    let mw = SkillPreloadMiddleware::new(vec![], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    // 模拟 executor 添加用户消息
    state.add_message(BaseMessage::human("帮我用 /test-diagnose-auto 调试一下"));

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert: 应自动检测并注入 Ai + Tool = 2 条消息，加上原始 Human 消息共 3 条
    assert_eq!(
        state.messages().len(),
        3,
        "应注入 2 条消息（Ai + Tool），加上原始 Human 消息共 3 条"
    );
    assert!(
        matches!(&state.messages()[0], BaseMessage::Human { .. }),
        "第一条应为 Human"
    );
    assert!(
        matches!(&state.messages()[1], BaseMessage::Ai { .. }),
        "第二条应为 Ai（fake Skill）"
    );
    assert!(
        matches!(&state.messages()[2], BaseMessage::Tool { .. }),
        "第三条应为 Tool（skill 内容）"
    );
    let tool_content = state.messages()[2].content();
    assert!(
        tool_content.contains("Skill content for test-diagnose-auto"),
        "Tool 结果应包含 skill 全文"
    );
}

#[tokio::test]
async fn test_auto_detect_multiple_skills() {
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "skill-a", "技能 A");
    write_skill(&skills_dir, "skill-b", "技能 B");

    let mw = SkillPreloadMiddleware::new(vec![], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/skill-a /skill-b 帮我看看"));

    mw.before_agent(&mut state).await.unwrap();

    // 1 Human + 1 Ai + 2 Tool = 4 条
    assert_eq!(
        state.messages().len(),
        4,
        "2 个 skill 应注入 Ai + 2 Tool = 3 条，加 Human 共 4 条"
    );
    assert_eq!(
        state.messages()[1].tool_calls().len(),
        2,
        "Ai 消息应有 2 个 ToolUse"
    );
}

#[tokio::test]
async fn test_auto_detect_no_matching_skill() {
    let dir = tempdir().unwrap();
    // 不创建任何 skill 文件

    let mw = SkillPreloadMiddleware::new(vec![], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    state.add_message(BaseMessage::human("/nonexistent-skill 不存在"));

    mw.before_agent(&mut state).await.unwrap();

    // 只有原始 Human 消息，无注入
    assert_eq!(state.messages().len(), 1, "找不到 skill 时不应注入任何消息");
}

#[tokio::test]
async fn test_auto_detect_no_human_message() {
    let dir = tempdir().unwrap();
    let mw = SkillPreloadMiddleware::new(vec![], dir.path().to_str().unwrap());
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    // 不添加任何消息

    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(state.messages().len(), 0, "无 Human 消息时应 no-op");
}

#[test]
fn test_extract_skill_names_basic() {
    let names = extract_skill_names_from_text("/diagnose");
    assert_eq!(names, vec!["diagnose"]);
}

#[test]
fn test_extract_skill_names_multiple() {
    let names = extract_skill_names_from_text("/diagnose /auto-issue-fixer /caveman");
    assert_eq!(names, vec!["diagnose", "auto-issue-fixer", "caveman"]);
}

#[test]
fn test_extract_skill_names_in_sentence() {
    let names = extract_skill_names_from_text("帮我用 /diagnose 调试一下这个问题");
    assert_eq!(names, vec!["diagnose"]);
}

#[test]
fn test_extract_skill_names_no_match() {
    let names = extract_skill_names_from_text("普通消息没有 skill");
    assert!(names.is_empty());
}

#[test]
fn test_extract_skill_names_slash_only() {
    let names = extract_skill_names_from_text("/");
    assert!(names.is_empty());
}

#[test]
fn test_extract_skill_names_rejects_path_like() {
    // /foo/bar is one whitespace token; "foo/bar" contains '/' → rejected
    let names = extract_skill_names_from_text("/foo/bar");
    assert!(names.is_empty(), "/foo/bar 含 '/' 应被整体拒绝");
}

#[tokio::test]
async fn test_preload_from_extra_dirs() {
    // Arrange: skill 不在标准路径，只在 extra_dirs 中
    let dir = tempdir().unwrap();
    let extra_dir = dir.path().join("plugin-skills");
    std::fs::create_dir_all(&extra_dir).unwrap();
    write_skill(&extra_dir, "plugin-skill", "插件技能");

    let mw = SkillPreloadMiddleware::new(
        vec!["plugin-skill".to_string()],
        "/nonexistent/cwd", // cwd 下没有 skill
    )
    .with_plugin_roots(vec![crate::skills::SkillRoot {
        path: extra_dir,
        source: crate::skills::SkillSource::Plugin,
        plugin_name: None,
    }]);
    let mut state = AgentState::new("/nonexistent/cwd");

    // Act
    mw.before_agent(&mut state).await.unwrap();

    // Assert: 应从 extra_dirs 找到并注入 Ai + Tool = 2 条消息
    assert_eq!(
        state.messages().len(),
        2,
        "应从 extra_dirs 找到 skill 并注入"
    );
    let tool_content = state.messages()[1].content();
    assert!(
        tool_content.contains("Skill content for plugin-skill"),
        "Tool 结果应包含插件 skill 全文"
    );
}

#[tokio::test]
async fn test_preload_loads_builtin_skill_content() {
    // SubAgent 路径：显式 skill_names 含 use-artifacts，应能从 BUILTIN_SKILLS 加载全文
    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    state.add_message(peri_agent::messages::BaseMessage::human("hi"));

    let mw = super::SkillPreloadMiddleware::new(vec!["use-artifacts".to_string()], "/tmp");

    mw.before_agent(&mut state).await.unwrap();

    // 应注入 Ai + Tool 消息（共 2 条），且 ToolResult 含 BUILTIN_SKILLS 的 SKILL.md 内容
    let msgs = state.messages();
    let tool_result_content = msgs
        .iter()
        .find(|m| matches!(m, peri_agent::messages::BaseMessage::Tool { .. }))
        .map(|m| m.content())
        .expect("应有 ToolResult 消息");
    assert!(
        tool_result_content.contains("Artifact"),
        "ToolResult 应含 BUILTIN_SKILLS 的 SKILL.md 全文，实际: {}",
        tool_result_content
    );
}
