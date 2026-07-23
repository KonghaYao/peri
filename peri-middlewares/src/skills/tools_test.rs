//! SkillTool + DiscoverSkillsTool 单元测试
//!
//! 测试环境：用 tempfile::TempDir 创建临时 skill 目录，
//! 写入测试 SKILL.md 文件，验证工具的正常路径和错误路径。

use super::*;
use crate::skills::{scan_skill_roots, SkillRoot, SkillSource};
use serde_json::json;
use std::sync::{Arc, RwLock};

/// 创建一个包含简单 SKILL.md 的临时 skill 目录
fn setup_temp_skill_dir() -> (tempfile::TempDir, PathBuf, String) {
    let temp = tempfile::TempDir::new().unwrap();
    let skill_dir = temp.path().join("test-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    let content = "---\nname: test-skill\ndescription: A test skill for unit tests\n---\n\n# Test Skill\n\nThis is the skill body.\n";
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    (temp, skill_dir, content.to_string())
}

/// 创建预填充缓存的 DiscoverSkillsTool
fn make_discover_tool_with_cache(roots: &[SkillRoot]) -> DiscoverSkillsTool {
    let skills = scan_skill_roots(roots);
    let cached = Arc::new(RwLock::new(Some(skills)));
    DiscoverSkillsTool::new(cached)
}
fn make_skill_tool_with_cache(plugin_roots: Vec<SkillRoot>) -> SkillTool {
    let skills = scan_skill_roots(&plugin_roots);
    let cached = Arc::new(RwLock::new(Some(skills)));
    SkillTool::new(cached)
}

// ─── SkillTool 测试 ──────────────────────────────────────────────────────────

#[test]
fn test_skill_tool_name_and_description() {
    let tool = make_skill_tool_with_cache(vec![]);
    assert_eq!(tool.name(), "SkillTool");
    // description 应包含关键信息
    assert!(tool.description().contains("Load the full content"));
    assert!(tool.description().contains("skill name"));
}

#[test]
fn test_skill_tool_parameters() {
    let tool = make_skill_tool_with_cache(vec![]);
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    // skill_name 应为必填字段
    let required: Vec<&str> = params["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"skill_name"));
}

#[tokio::test]
async fn test_skill_tool_missing_skill_name() {
    // Arrange
    let (_temp, skill_dir, _) = setup_temp_skill_dir();
    let root = SkillRoot {
        path: skill_dir.parent().unwrap().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_skill_tool_with_cache(vec![root]);
    let input = json!({});
    let cwd = skill_dir.parent().unwrap().to_str().unwrap();

    // Act
    let result = tool.invoke(input, ToolContext::new(&[], cwd)).await;

    // Assert: 缺少 skill_name 应返回错误
    assert!(result.is_err(), "缺 skill_name 应返回错误");
    assert!(result.unwrap_err().to_string().contains("skill_name"));
}

#[tokio::test]
async fn test_skill_tool_loads_disk_skill() {
    // Arrange: 创建临时 skill 目录，放入 SKILL.md
    let (_temp, skill_dir, _expected_content) = setup_temp_skill_dir();
    let root = SkillRoot {
        path: skill_dir.parent().unwrap().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_skill_tool_with_cache(vec![root]);
    let input = json!({"skill_name": "test-skill"});
    let cwd = skill_dir.parent().unwrap().to_str().unwrap();

    // Act
    let result = tool.invoke(input, ToolContext::new(&[], cwd)).await;

    // Assert: 应加载完整 SKILL.md 内容
    assert!(result.is_ok(), "磁盘 skill 加载应成功");
    let content = result.unwrap();
    assert!(content.contains("Test Skill"));
    assert!(content.contains("skill body"));
}

#[tokio::test]
async fn test_skill_tool_case_insensitive_match() {
    // Arrange
    let (_temp, skill_dir, _) = setup_temp_skill_dir();
    let root = SkillRoot {
        path: skill_dir.parent().unwrap().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_skill_tool_with_cache(vec![root]);
    let cwd = skill_dir.parent().unwrap().to_str().unwrap();

    // Act: 大小写不一致
    let result = tool
        .invoke(
            json!({"skill_name": "TEST-SKILL"}),
            ToolContext::new(&[], cwd),
        )
        .await;

    // Assert: 大小写无关匹配
    assert!(result.is_ok(), "大小写无关匹配应成功");
}

#[tokio::test]
async fn test_skill_tool_skill_not_found() {
    // Arrange
    let (_temp, skill_dir, _) = setup_temp_skill_dir();
    let root = SkillRoot {
        path: skill_dir.parent().unwrap().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_skill_tool_with_cache(vec![root]);
    let cwd = skill_dir.parent().unwrap().to_str().unwrap();

    // Act: 不存在的 skill
    let result = tool
        .invoke(
            json!({"skill_name": "nonexistent-skill"}),
            ToolContext::new(&[], cwd),
        )
        .await;

    // Assert: 返回错误消息
    assert!(result.is_err(), "不存在的 skill 应返回错误");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"));
    assert!(err.contains("nonexistent-skill"));
}

#[tokio::test]
async fn test_skill_tool_namespace_prefix_match() {
    // Arrange: 创建 skill 目录（不带前缀），但传入带命名空间前缀的名称
    let (_temp, skill_dir, _) = setup_temp_skill_dir();
    let root = SkillRoot {
        path: skill_dir.parent().unwrap().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_skill_tool_with_cache(vec![root]);
    let cwd = skill_dir.parent().unwrap().to_str().unwrap();

    // Act: 带命名空间前缀 `ns:test-skill`
    let result = tool
        .invoke(
            json!({"skill_name": "ns:test-skill"}),
            ToolContext::new(&[], cwd),
        )
        .await;

    // Assert: 去掉前缀后应匹配成功
    assert!(result.is_ok(), "去命名空间前缀后应匹配成功");
}

#[tokio::test]
async fn test_skill_tool_empty_cache_returns_error() {
    // 构造一个缓存为 None 的工具，模拟 before_agent 未运行的场景
    let tool = SkillTool::new(Arc::new(RwLock::new(None)));
    let result = tool
        .invoke(
            json!({"skill_name": "any-skill"}),
            ToolContext::new(&[], ""),
        )
        .await;

    // 缓存为空时应返回明确错误，而非 panic
    assert!(result.is_err(), "空缓存应返回错误而非 panic");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("cache is empty") || err.contains("before_agent"),
        "错误应提示缓存为空原因，实际: {err}"
    );
}

// ─── DiscoverSkillsTool 测试 ─────────────────────────────────────────────────

#[test]
fn test_discover_skills_name_and_description() {
    let root = SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    assert_eq!(tool.name(), "DiscoverSkillsTool");
    assert!(tool.description().contains("Search for available skills"));
    assert!(tool.description().contains("JSON array"));
}

#[test]
fn test_discover_skills_parameters() {
    let root = SkillRoot {
        path: PathBuf::new(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    // query 不是必填
    let required = params.get("required").and_then(|v| v.as_array());
    assert!(required.map(|r| r.is_empty()).unwrap_or(true));
}

#[tokio::test]
async fn test_discover_skills_returns_all_without_query() {
    // Arrange: 创建两个临时 skill 目录
    let temp = tempfile::TempDir::new().unwrap();
    let skill1_dir = temp.path().join("skill-a");
    std::fs::create_dir(&skill1_dir).unwrap();
    std::fs::write(
        skill1_dir.join("SKILL.md"),
        "---\nname: skill-a\ndescription: First test skill\n---\n\n# Skill A\n",
    )
    .unwrap();
    let skill2_dir = temp.path().join("skill-b");
    std::fs::create_dir(&skill2_dir).unwrap();
    std::fs::write(
        skill2_dir.join("SKILL.md"),
        "---\nname: skill-b\ndescription: Second test skill\n---\n\n# Skill B\n",
    )
    .unwrap();

    let root = SkillRoot {
        path: temp.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    let cwd = temp.path().to_str().unwrap();

    // Act: 无 query
    let result = tool.invoke(json!({}), ToolContext::new(&[], cwd)).await;

    // Assert: 返回所有 skill
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("skill-a"));
    assert!(output.contains("skill-b"));
    assert!(output.contains("project"));
}

#[tokio::test]
async fn test_discover_skills_filters_by_query() {
    // Arrange
    let temp = tempfile::TempDir::new().unwrap();
    let skill1_dir = temp.path().join("coding-helper");
    std::fs::create_dir(&skill1_dir).unwrap();
    std::fs::write(
        skill1_dir.join("SKILL.md"),
        "---\nname: coding-helper\ndescription: Helps with coding tasks\n---\n\n# Coding Helper\n",
    )
    .unwrap();
    let skill2_dir = temp.path().join("writing-tool");
    std::fs::create_dir(&skill2_dir).unwrap();
    std::fs::write(
        skill2_dir.join("SKILL.md"),
        "---\nname: writing-tool\ndescription: Helps with writing\n---\n\n# Writing Tool\n",
    )
    .unwrap();

    let root = SkillRoot {
        path: temp.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    let cwd = temp.path().to_str().unwrap();

    // Act: 按名称筛选
    let result = tool
        .invoke(json!({"query": "coding"}), ToolContext::new(&[], cwd))
        .await;

    // Assert: 只匹配 coding-helper
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("coding-helper"));
    assert!(!output.contains("writing-tool"));
}

#[tokio::test]
async fn test_discover_skills_case_insensitive_filter() {
    // Arrange
    let temp = tempfile::TempDir::new().unwrap();
    let skill_dir = temp.path().join("MyTool");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: MyTool\ndescription: A tool description\n---\n\n# MyTool\n",
    )
    .unwrap();

    let root = SkillRoot {
        path: temp.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    let cwd = temp.path().to_str().unwrap();

    // Act: 小写 query 筛选大写名称
    let result = tool
        .invoke(json!({"query": "mytool"}), ToolContext::new(&[], cwd))
        .await;

    // Assert: 大小写无关筛选
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("MyTool"));
}

#[tokio::test]
async fn test_discover_skills_empty_result() {
    // Arrange
    let temp = tempfile::TempDir::new().unwrap();
    let skill_dir = temp.path().join("skill-x");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: skill-x\ndescription: Just a skill\n---\n\n# Skill X\n",
    )
    .unwrap();

    let root = SkillRoot {
        path: temp.path().to_path_buf(),
        source: SkillSource::Project,
        plugin_name: None,
    };
    let tool = make_discover_tool_with_cache(&[root]);
    let cwd = temp.path().to_str().unwrap();

    // Act: 不匹配的 query
    let result = tool
        .invoke(
            json!({"query": "zzz-not-exist"}),
            ToolContext::new(&[], cwd),
        )
        .await;

    // Assert: 返回空数组
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.trim(), "[]");
}

// ─── find_and_load_skill 边界测试 ────────────────────────────────────────────

#[test]
fn test_find_and_load_skill_not_found_in_empty_list() {
    // 空列表应返回错误
    let result = find_and_load_skill(&[], "any-skill");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

use std::path::PathBuf;
