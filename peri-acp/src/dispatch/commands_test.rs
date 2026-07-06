use std::path::PathBuf;

use peri_middlewares::skills::SkillMetadata;

use super::commands::build_available_commands;

#[test]
fn test_build_available_commands_includes_builtins() {
    let cmds = build_available_commands(&[]);
    // 至少 13 个内置命令
    assert!(cmds.len() >= 13, "至少 13 条内置命令，实际: {}", cmds.len());
    // 验证关键命令存在
    let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"help"), "help 命令应存在");
    assert!(names.contains(&"clear"), "clear 命令应存在");
    assert!(names.contains(&"compact"), "compact 命令应存在");
    assert!(names.contains(&"agents"), "agents 命令应存在");
}

#[test]
fn test_build_available_commands_includes_skills() {
    let skills = vec![
        SkillMetadata {
            name: "my-skill".into(),
            description: "My custom skill".into(),
            path: PathBuf::from("/fake/my-skill/SKILL.md"),
            ..Default::default()
        },
        SkillMetadata {
            name: "other".into(),
            description: "Other skill".into(),
            path: PathBuf::from("/fake/other/SKILL.md"),
            ..Default::default()
        },
    ];
    let cmds = build_available_commands(&skills);
    let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"my-skill"), "my-skill 应存在");
    assert!(names.contains(&"other"), "other 应存在");
}

#[test]
fn test_build_available_commands_no_skills_no_leak() {
    let cmds = build_available_commands(&[]);
    assert!(
        !cmds.iter().any(|c| c.name.as_str().starts_with("skill:")),
        "不应包含 skill: 前缀命令"
    );
}
