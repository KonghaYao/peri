// ─── build_subagent_middlewares 单元测试 ───────────────────────────────────

use super::{build_subagent_middlewares, SubAgentMiddlewareConfig};

#[test]
fn test_build_middleware_fork_config_无_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork("/tmp"));
    assert_eq!(middlewares.len(), 3);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec!["AgentsMdMiddleware", "SkillsMiddleware", "TodoMiddleware"]
    );
}

#[test]
fn test_build_middleware_agent_def_空技能_无_skill_preload() {
    let middlewares =
        build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(vec![], "/tmp"));
    assert_eq!(middlewares.len(), 3);
    assert!(!middlewares
        .iter()
        .any(|m| m.name() == "SkillPreloadMiddleware"));
}

#[test]
fn test_build_middleware_agent_def_有技能_包含_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["test-skill".to_string()],
        "/tmp",
    ));
    assert_eq!(middlewares.len(), 4);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "TodoMiddleware"
        ]
    );
}

#[test]
fn test_build_middleware_顺序固定() {
    // 有 skills 时验证完整顺序
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["a".to_string()],
        "/tmp",
    ));
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "TodoMiddleware"
        ]
    );
}

// ─── MetaHarness（设计 §2.5）：子链关闭契约测试 ───────────────────────────────

/// 子链独立装配：关闭的 middleware 不进子链，未禁用项保持原相对顺序。
#[test]
fn test_build_middleware_meta_harness_disabled_filters_chain() {
    let baseline: Vec<String> = build_subagent_middlewares(
        SubAgentMiddlewareConfig::for_agent_def(vec!["a".to_string()], "/tmp"),
    )
    .iter()
    .map(|m| m.name().to_string())
    .collect();
    assert_eq!(
        baseline,
        vec![
            "AgentsMdMiddleware".to_string(),
            "SkillsMiddleware".to_string(),
            "SkillPreloadMiddleware".to_string(),
            "TodoMiddleware".to_string()
        ]
    );

    let cases: &[&str] = &[
        "AgentsMdMiddleware",
        "SkillsMiddleware",
        "SkillPreloadMiddleware",
        "TodoMiddleware",
    ];
    for mw in cases {
        let mut disabled = std::collections::HashSet::new();
        disabled.insert(mw.to_string());
        let names: Vec<String> = build_subagent_middlewares(
            SubAgentMiddlewareConfig::for_agent_def(vec!["a".to_string()], "/tmp")
                .with_meta_harness_disabled(disabled),
        )
        .iter()
        .map(|m| m.name().to_string())
        .collect();
        assert!(
            !names.iter().any(|n| n == mw),
            "disabled {mw} 后仍出现在子链: {names:?}"
        );
        let expected: Vec<String> = baseline.iter().filter(|n| *n != mw).cloned().collect();
        assert_eq!(names, expected, "disabled {mw} 后剩余顺序漂移");
    }
}

/// SkillPreload 关闭：即使 agent 定义声明 skills 也不注册。
#[test]
fn test_build_middleware_meta_harness_disabled_skill_preload_suppresses_declared_skills() {
    let mut disabled = std::collections::HashSet::new();
    disabled.insert("SkillPreloadMiddleware".to_string());
    let middlewares = build_subagent_middlewares(
        SubAgentMiddlewareConfig::for_agent_def(vec!["declared-skill".to_string()], "/tmp")
            .with_meta_harness_disabled(disabled),
    );
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec!["AgentsMdMiddleware", "SkillsMiddleware", "TodoMiddleware"]
    );
}

/// 空 disabled 集合（默认）下子链与原行为完全一致（for_fork 3 件套）。
#[test]
fn test_build_middleware_meta_harness_default_empty_unchanged() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork("/tmp"));
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec!["AgentsMdMiddleware", "SkillsMiddleware", "TodoMiddleware"]
    );
}

// ─── frozen 数据传递测试 ──────────────────────────────────────────────────

use peri_agent::agent::state::AgentState;
use tempfile::TempDir;

/// 验证：传入 frozen CLAUDE.md 内容时，SubAgent 中间件链的 AgentsMdMiddleware
/// 应直接 prepend frozen 内容，跳过磁盘读取。
///
/// 这是 SC#2 修复的核心契约：SubAgent 必须复用 main agent 捕获的 frozen 数据，
/// 不能在 spawn 时重新读盘。
#[tokio::test]
async fn test_subagent_中间件链_注入_frozen_claude_md() {
    // Arrange: 空白 tempdir（无 CLAUDE.md），但提供 frozen 内容
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    let frozen_content =
        "# FROZEN TEST CLAUDE.md\nThis content must be injected verbatim.".to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd).with_frozen(
        Some(frozen_content.clone()),
        None,
        None,
    );

    // Act: 构造中间件链，模拟 SubAgent spawn
    let middlewares = build_subagent_middlewares(config);
    let _state = AgentState::new(&cwd);

    // 找到 AgentsMdMiddleware（v2 通过 prompt_contribution 声明贡献，不再 prepend_message）
    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .expect("AgentsMdMiddleware 必须在链首");

    // Assert: prompt_contribution 应返回 frozen 内容
    let contribution = agents_md
        .prompt_contribution()
        .expect("v2 prompt_contribution 应返回 frozen CLAUDE.md 内容");
    assert!(
        contribution.contains("FROZEN TEST CLAUDE.md"),
        "frozen 内容应通过 prompt_contribution 暴露，实际：{}",
        contribution
    );
}

/// 验证：未提供 frozen 数据时（遗留/测试场景），中间件回退到磁盘读取。
/// 在空白 tempdir 中不注入任何 System 消息。
#[tokio::test]
async fn test_subagent_中间件链_无_frozen_回退磁盘() {
    // Arrange: 空白 tempdir，无 frozen 数据
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd);
    let middlewares = build_subagent_middlewares(config);
    let mut state = AgentState::new(&cwd);

    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .unwrap();
    agents_md.before_agent(&mut state).await.unwrap();

    // Assert: 没有 CLAUDE.md 时，不注入任何 System 消息
    assert!(
        state.messages().is_empty(),
        "无 frozen + 无磁盘文件时不应注入消息"
    );
}
