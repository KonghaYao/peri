//! Tests for mod_skills

use peri_agent::{agent::state::AgentState, middleware::r#trait::Middleware};
use tempfile::tempdir;

use super::*;

/// Helper: call prompt_contribution with concrete State type for testing.
fn contribution(mw: &SkillsMiddleware) -> Option<String> {
    Middleware::prompt_contribution(mw)
}

fn write_skill(dir: &std::path::Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = format!(
        "---\nname: '{}'\ndescription: '{}'\n---\n\n# {}\n",
        name, desc, name
    );
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
}

#[tokio::test]
async fn test_no_skills_no_op() {
    // 使用临时目录作为所有 skills 目录来源，确保测试隔离
    let empty_dir = tempdir().unwrap();
    let empty_path = empty_dir.path().to_path_buf();

    let mw = SkillsMiddleware::new()
        .with_user_dir(empty_path.clone())
        .with_project_dir(empty_path);
    let mut state = AgentState::new("/nonexistent/path");
    let result = mw.before_agent(&mut state).await;
    assert!(result.is_ok());
    assert!(contribution(&mw).is_none());
    assert_eq!(state.messages().len(), 0);
}

#[tokio::test]
async fn test_injects_summary() {
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "tui-dev", "构建 TUI 应用");
    write_skill(&skills_dir, "codebase-exploration", "深度代码搜索");

    let mw = SkillsMiddleware::new();
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    assert_eq!(
        state.messages().len(),
        0,
        "before_agent 不应再 prepend 消息"
    );
    let content = contribution(&mw).unwrap();
    assert!(content.contains("tui-dev"));
    assert!(content.contains("codebase-exploration"));
    assert!(content.contains("Skills"));
}

#[tokio::test]
async fn test_custom_project_dir() {
    let dir = tempdir().unwrap();
    write_skill(dir.path(), "custom-skill", "自定义技能");

    let mw = SkillsMiddleware::new().with_project_dir(dir.path().to_path_buf());
    let mut state = AgentState::new("/any/cwd");
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(content.contains("custom-skill"));
}

#[tokio::test]
async fn test_build_summary_contains_slash_prefix() {
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "test-skill", "test description");

    let mw = SkillsMiddleware::new();
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(
        content.contains("'/skill-name'"),
        "提示词应包含 '/skill-name' 格式，实际: {}",
        content
    );
}

#[tokio::test]
async fn test_build_summary_does_not_contain_hash_prefix() {
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    write_skill(&skills_dir, "test-skill", "test description");

    let mw = SkillsMiddleware::new();
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(
        !content.contains("#skill_name"),
        "提示词不应包含旧 #skill_name 格式，实际: {}",
        content
    );
}

#[tokio::test]
async fn test_extra_dirs_injected() {
    let dir = tempdir().unwrap();
    let extra1 = dir.path().join("extra1");
    let extra2 = dir.path().join("extra2");
    std::fs::create_dir_all(&extra1).unwrap();
    std::fs::create_dir_all(&extra2).unwrap();
    write_skill(&extra1, "extra-skill-1", "from extra 1");
    write_skill(&extra2, "extra-skill-2", "from extra 2");

    let mw = SkillsMiddleware::new()
        .with_user_dir(dir.path().to_path_buf())
        .with_project_dir(dir.path().to_path_buf())
        .with_plugin_roots(vec![
            SkillRoot {
                path: extra1.clone(),
                source: SkillSource::Plugin,
                plugin_name: None,
            },
            SkillRoot {
                path: extra2.clone(),
                source: SkillSource::Plugin,
                plugin_name: None,
            },
        ]);

    let mut state = AgentState::new(dir.path().to_str().unwrap());
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(
        content.contains("extra-skill-1"),
        "Should include skill from extra dir 1"
    );
    assert!(
        content.contains("extra-skill-2"),
        "Should include skill from extra dir 2"
    );
}

#[tokio::test]
async fn test_extra_dirs_nonexistent_skipped() {
    let dir = tempdir().unwrap();
    let mw = SkillsMiddleware::new()
        .with_user_dir(dir.path().to_path_buf())
        .with_project_dir(dir.path().to_path_buf())
        .with_plugin_roots(vec![SkillRoot {
            path: dir.path().join("nonexistent"),
            source: SkillSource::Plugin,
            plugin_name: None,
        }]);

    let mut state = AgentState::new(dir.path().to_str().unwrap());
    let result = mw.before_agent(&mut state).await;
    assert!(result.is_ok());
    assert!(contribution(&mw).is_none(), "No skills should be injected");
}

#[tokio::test]
async fn test_extra_dirs_priority_after_project() {
    let dir = tempdir().unwrap();
    // project skills directory (acts as cwd/.claude/skills)
    let project_skills = dir.path().join("project-skills");
    std::fs::create_dir_all(&project_skills).unwrap();
    write_skill(&project_skills, "project-skill", "from project");

    let extra_dir = dir.path().join("extra");
    std::fs::create_dir_all(&extra_dir).unwrap();
    write_skill(&extra_dir, "extra-skill", "from extra");

    let mw = SkillsMiddleware::new()
        .with_user_dir(dir.path().to_path_buf())
        .with_project_dir(project_skills)
        .with_plugin_roots(vec![SkillRoot {
            path: extra_dir,
            source: SkillSource::Plugin,
            plugin_name: None,
        }]);

    let mut state = AgentState::new("/nonexistent");
    mw.before_agent(&mut state).await.unwrap();

    let content = contribution(&mw).unwrap();
    assert!(content.contains("project-skill"));
    assert!(content.contains("extra-skill"));
}

#[test]
fn test_load_disable_bundled_skills_defaults_false_when_missing() {
    // settings.json 无 disableBundledSkills 字段时返回 false
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, r#"{"config": {}}"#).unwrap();

    let value = super::load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value, "缺字段时应默认 false");
}

#[test]
fn test_load_disable_bundled_skills_reads_true() {
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config": {"disableBundledSkills": true}}"#,
    )
    .unwrap();

    let value = super::load_disable_bundled_skills_from_path(&settings_path);
    assert!(value, "disableBundledSkills=true 时应返回 true");
}

#[test]
fn test_load_disable_bundled_skills_reads_false_explicit() {
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config": {"disableBundledSkills": false}}"#,
    )
    .unwrap();

    let value = super::load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value);
}

#[test]
fn test_load_disable_bundled_skills_handles_missing_file() {
    // 文件不存在时返回 false
    let value =
        super::load_disable_bundled_skills_from_path(std::path::Path::new("/nonexistent.json"));
    assert!(!value);
}

#[test]
fn test_load_disable_bundled_skills_reads_flat_true() {
    // 扁平 JSON（无 config 包裹）也应支持
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, r#"{"disableBundledSkills": true}"#).unwrap();

    let value = super::load_disable_bundled_skills_from_path(&settings_path);
    assert!(value, "扁平 JSON disableBundledSkills=true 时应返回 true");
}

#[test]
fn test_load_disable_bundled_skills_handles_malformed_json() {
    // 畸形 JSON（如崩溃留下的半截文件）应默认 false
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config": {"disableBundledSkills": broken}"#,
    )
    .unwrap();

    let value = super::load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value, "畸形 JSON 应默认 false");
}

// ===== E2E: Builtin skills 全链路验证（Task 7） =====

#[test]
fn test_e2e_frozen_summary_contains_builtin_use_artifacts() {
    // 验证：disable_bundled=false 时 frozen summary 含 builtin use-artifacts
    let summary = SkillsMiddleware::build_frozen_summary("/tmp", vec![], false);
    let summary = summary.expect("非空时应返回 Some");
    assert!(
        summary.contains("use-artifacts"),
        "frozen summary 应含 builtin use-artifacts，实际: {}",
        summary
    );
    // D4：catalog 用 [builtin] 来源标签（不再暴露虚拟路径/description）
    assert!(
        summary.contains("- **use-artifacts** [builtin]"),
        "frozen summary 应以 [builtin] 来源标签列出 use-artifacts，实际: {}",
        summary
    );
}

#[test]
fn test_e2e_frozen_summary_excludes_builtin_when_disabled() {
    // 验证：disable_bundled=true 时 Builtin root 不被追加，
    // frozen summary 不含 builtin use-artifacts
    let summary = SkillsMiddleware::build_frozen_summary("/tmp", vec![], true);
    // 可能返回 None（无任何 skill）或 Some（仅含磁盘 skill）
    if let Some(s) = summary {
        assert!(
            !s.contains("use-artifacts"),
            "disable_bundled=true 时不应含 Builtin use-artifacts，实际: {}",
            s
        );
    }
}

/// [回归测试] D3：模型可见 skill 协议唯一——SkillTool(skill_name) +
/// DiscoverSkillsTool，旧 Skill(skill, args) 已移除。
///
/// 历史背景（审计 prompt-sections-audit.md P1-6）：主 agent 链曾同时注册
/// `SkillTool`（skills/tools.rs）与 `Skill`（tools/skill.rs，参数 skill+args），
/// 模型面对一对"同名职责、参数冲突"的工具。D3 收敛后 SkillsMiddleware 是
/// 主链与 subagent 链共用的 skill 工具源，collect_tools 必须恰好返回两个
/// 工具且不含 "Skill"。
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

/// [回归测试] D3：SkillTool 对"catalog 有但磁盘已删除"的 skill 返回可恢复错误，
/// 不破坏 frozen prefix（冻结摘要不变，错误只发生在加载时刻）。
#[tokio::test]
async fn test_skill_tool_error_is_recoverable_when_file_deleted_mid_session() {
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills").join("gone-skill");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: 'gone-skill'\ndescription: 'd'\n---\n\nbody",
    )
    .unwrap();

    let mw = SkillsMiddleware::new();
    let cache = mw.skills_cache();
    // 模拟 before_agent 已扫描（缓存含 gone-skill）
    let roots = vec![SkillRoot {
        path: dir.path().join(".claude").join("skills"),
        source: SkillSource::Project,
        plugin_name: None,
    }];
    let skills = scan_skill_roots(&roots);
    *cache.write().unwrap() = Some(skills);

    // 会话中途删除磁盘文件
    std::fs::remove_dir_all(&skills_dir).unwrap();

    let tool = tools::SkillTool::new(cache);
    let result = tool
        .invoke(
            serde_json::json!({"skill_name": "gone-skill"}),
            peri_agent::tools::ToolContext::new(&[], "/tmp"),
        )
        .await;
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("session catalog") && msg.contains("DiscoverSkillsTool"),
        "错误应说明 catalog/磁盘边界并提供可恢复路径，实际: {msg}"
    );
}
