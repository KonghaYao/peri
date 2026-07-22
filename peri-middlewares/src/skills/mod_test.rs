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
    assert!(
        summary.contains("<builtin>/use-artifacts"),
        "frozen summary 应含虚拟路径 <builtin>/use-artifacts，实际: {}",
        summary
    );
}

#[test]
fn test_e2e_frozen_summary_excludes_builtin_when_disabled() {
    // 验证：disable_bundled=true 时 Builtin root 不被追加，
    // frozen summary 不含 <builtin>/use-artifacts
    let summary = SkillsMiddleware::build_frozen_summary("/tmp", vec![], true);
    // 可能返回 None（无任何 skill）或 Some（仅含磁盘 skill）
    if let Some(s) = summary {
        assert!(
            !s.contains("<builtin>/use-artifacts"),
            "disable_bundled=true 时不应含 Builtin use-artifacts，实际: {}",
            s
        );
    }
}
