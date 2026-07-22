//! SkillTool 测试。
//!
//! 测试覆盖：正常查找（项目/builtin）、参数透传、不存在+模糊匹配、
//! 大小写无关、disable_bundled、缺少必填参数。

use super::*;
use std::path::Path;

/// 在 dir 下创建虚构的 SKILL.md
fn write_skill(dir: &Path, name: &str, desc: &str) -> std::path::PathBuf {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = format!(
        "---\nname: '{}'\ndescription: '{}'\n---\n\n# {}\n\nFull skill content.\n",
        name, desc, name
    );
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    skill_dir.join("SKILL.md")
}

fn make_ctx() -> ToolContext<'static> {
    ToolContext::new(&[], "")
}

// ── 正常路径 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_finds_project_skill() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    write_skill(&skills_dir, "auto-issue-fixer", "Issue lifecycle manager");

    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(serde_json::json!({"skill": "auto-issue-fixer"}), make_ctx())
        .await
        .unwrap();

    assert!(
        result.contains("Full skill content"),
        "应返回 SKILL.md 内容"
    );
}

#[tokio::test]
async fn test_finds_builtin_skill() {
    // builtin skill（use-artifacts）应可被查找到
    let dir = tempfile::tempdir().unwrap();
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(serde_json::json!({"skill": "use-artifacts"}), make_ctx())
        .await
        .unwrap();

    assert!(
        result.contains("artifact") || result.contains("Artifact"),
        "应返回 builtin use-artifacts 的 SKILL.md 内容"
    );
}

#[tokio::test]
async fn test_case_insensitive_match() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    write_skill(&skills_dir, "my-skill", "Test skill");

    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(serde_json::json!({"skill": "My-Skill"}), make_ctx())
        .await
        .unwrap();

    assert!(
        result.contains("Full skill content"),
        "大小写无关匹配应能查找到 my-skill"
    );
}

#[tokio::test]
async fn test_args_passthrough() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    write_skill(&skills_dir, "test-skill", "Test");

    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(
            serde_json::json!({"skill": "test-skill", "args": "--verbose"}),
            make_ctx(),
        )
        .await
        .unwrap();

    assert!(
        result.contains("[调用参数: --verbose]"),
        "args 应附加到返回内容末尾"
    );
}

#[tokio::test]
async fn test_project_skill_overrides_builtin() {
    // Project 级 skill 同名覆盖 Builtin skill。
    // builtin 中有 use-artifacts，现在 project 中创建同名 skill。
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    write_skill(&skills_dir, "use-artifacts", "Custom project version");

    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(serde_json::json!({"skill": "use-artifacts"}), make_ctx())
        .await
        .unwrap();

    // project 级 skill 应优先于 builtin
    assert!(
        result.contains("Custom project version"),
        "project 同名 skill 应覆盖 builtin，实际: {}",
        result.lines().next().unwrap_or("")
    );
    assert!(
        !result.contains("Teach the agent when and how to use the artifact tool"),
        "不应包含 builtin 版本的描述，实际: {}",
        result.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

// ── 错误路径 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_not_found_with_suggestions() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join(".claude").join("skills");
    write_skill(&skills_dir, "auto-issue-fixer", "Issue lifecycle");
    write_skill(&skills_dir, "blog-writer", "Blog writing");

    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool
        .invoke(serde_json::json!({"skill": "auto-iss-fixer"}), make_ctx())
        .await;

    assert!(result.is_err(), "不存在的 skill 应返回 Err");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Unknown skill"),
        "错误信息应含 Unknown skill，实际: {}",
        err
    );
    assert!(
        err.contains("Did you mean"),
        "错误信息应含模糊匹配建议，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_not_found_no_skills_available() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], true); // disable_bundled=true
    let result = tool
        .invoke(serde_json::json!({"skill": "nonexistent"}), make_ctx())
        .await;

    assert!(result.is_err(), "不存在的 skill 应返回 Err");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("没有找到可用的 skill"),
        "无可用 skill 时应提示，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_missing_skill_param() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let result = tool.invoke(serde_json::json!({}), make_ctx()).await;

    assert!(result.is_err(), "缺少 skill 参数应返回 Err");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("必填"), "错误信息应提示必填，实际: {}", err);
}

#[tokio::test]
async fn test_disable_bundled_excludes_builtin() {
    let dir = tempfile::tempdir().unwrap();
    // disable_bundled=true 时，builtin skill 不在查找范围内
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], true);
    let result = tool
        .invoke(serde_json::json!({"skill": "use-artifacts"}), make_ctx())
        .await;

    assert!(
        result.is_err(),
        "disable_bundled=true 时 builtin skill 应不可见"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Unknown skill"),
        "应返回 Unknown skill 错误，实际: {}",
        err
    );
}

// ── 工具基本信息 ──────────────────────────────────────────────────────────

#[test]
fn test_tool_name_is_skill() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    assert_eq!(tool.name(), "Skill");
}

#[test]
fn test_tool_parameters_require_skill() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SkillTool::new(dir.path().to_str().unwrap(), vec![], false);
    let params = tool.parameters();
    let required = params["required"].as_array().unwrap();
    let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"skill"), "required 应含 'skill'");
    assert!(!names.contains(&"args"), "args 不应在 required 中");
}
