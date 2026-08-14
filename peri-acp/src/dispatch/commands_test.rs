use std::path::PathBuf;

use peri_acp_types::PeriCaps;
use peri_middlewares::skills::{SkillMetadata, SkillSource};

use super::commands::{build_available_commands, build_available_commands_update};

#[test]
fn test_build_available_commands_includes_builtins() {
    let cmds = build_available_commands(&[]);
    // 基座：仅功能性内置命令（compact/loop）
    assert_eq!(
        cmds.len(),
        2,
        "应为 2 条功能性内置命令，实际: {}",
        cmds.len()
    );
    // 验证功能性命令存在
    let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"compact"), "compact 命令应存在");
    assert!(names.contains(&"loop"), "loop 命令应存在");
}

/// 界面性命令按 `caps.ui_commands` 门控：TUI（全 cap）广播，外部客户端不广播。
#[test]
fn test_ui_commands_gated_by_caps() {
    // 外部客户端（无 cap）：只有功能性命令
    let caps_off = PeriCaps::default();
    let update = build_available_commands_update(&[], &[], &caps_off);
    let value = serde_json::to_value(&update).unwrap();
    let names: Vec<&str> = value["availableCommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "无 ui_commands cap 时仅功能性命令");
    for ui_cmd in [
        "help", "clear", "context", "cost", "mode", "effort", "history", "agents", "rename",
        "lang", "exit", "cron",
    ] {
        assert!(!names.contains(&ui_cmd), "非功能性命令 {ui_cmd} 不应广播");
    }

    // TUI（全 cap）：附加全部界面性命令
    let caps_on = PeriCaps::all_enabled();
    let update = build_available_commands_update(&[], &[], &caps_on);
    let value = serde_json::to_value(&update).unwrap();
    let names: Vec<&str> = value["availableCommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 13, "全 cap 时 = 2 功能性 + 11 界面性");
    assert!(names.contains(&"compact"));
    assert!(names.contains(&"clear"), "clear 应广播（TUI 场景）");
    assert!(names.contains(&"exit"), "exit 应广播（TUI 场景）");
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

// ─── build_available_commands_update 纯函数（Slice 6 / DD-5）───────────────

fn local_skill(name: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.into(),
        description: format!("Local {name}"),
        path: PathBuf::from(format!("/fake/{name}/SKILL.md")),
        ..Default::default()
    }
}

fn mcp_skill(name: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.into(),
        description: format!("MCP {name}"),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        ..Default::default()
    }
}

#[test]
fn test_build_update_mcp_entries_in_available_commands() {
    let local = vec![local_skill("local-a")];
    let mcp = vec![mcp_skill("mcp__demo__hello")];
    let caps = PeriCaps::all_enabled();

    let update = build_available_commands_update(&local, &mcp, &caps);
    let value = serde_json::to_value(&update).unwrap();

    let commands = value["availableCommands"].as_array().unwrap();
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"compact"), "内置命令应存在");
    assert!(
        names.contains(&"local-a"),
        "本地 skill 应进 availableCommands"
    );
    assert!(
        names.contains(&"mcp__demo__hello"),
        "MCP 条目应进 availableCommands（name/description 同构）"
    );
    // 合并后条目数 = 2 功能性 + 11 界面性（全 cap）+ 1 本地 + 1 MCP
    assert_eq!(commands.len(), 15, "合并后条目数应为 2 + 11 + local + mcp");
    let mcp_entry = commands
        .iter()
        .find(|c| c["name"] == "mcp__demo__hello")
        .unwrap();
    assert_eq!(mcp_entry["description"], "MCP mcp__demo__hello");
}

#[test]
fn test_build_update_meta_skill_names_and_mcp_skill_names() {
    let local = vec![local_skill("local-a"), local_skill("local-b")];
    let mcp = vec![mcp_skill("mcp__demo__hello")];
    let caps = PeriCaps::all_enabled();

    let update = build_available_commands_update(&local, &mcp, &caps);
    let value = serde_json::to_value(&update).unwrap();

    // skillNames 仅本地名，MCP 名不进
    let skill_names = value["_meta"]["skillNames"].as_array().unwrap();
    assert_eq!(
        skill_names,
        &vec![serde_json::json!("local-a"), serde_json::json!("local-b")],
        "skillNames 应仅含本地名"
    );
    assert!(
        !skill_names
            .iter()
            .any(|n| n.as_str() == Some("mcp__demo__hello")),
        "MCP 名不得进 skillNames"
    );

    // mcpSkillNames 并列、值正确
    let mcp_names = value["_meta"]["mcpSkillNames"].as_array().unwrap();
    assert_eq!(
        mcp_names,
        &vec![serde_json::json!("mcp__demo__hello")],
        "mcpSkillNames 应含 MCP 名"
    );
}

#[test]
fn test_build_update_mcp_skill_names_absent_when_mcp_empty() {
    let local = vec![local_skill("local-a")];
    let caps = PeriCaps::all_enabled();

    let update = build_available_commands_update(&local, &[], &caps);
    let value = serde_json::to_value(&update).unwrap();

    assert!(
        value["_meta"].get("mcpSkillNames").is_none(),
        "mcp 为空时不得附加 mcpSkillNames"
    );
    assert!(
        value["_meta"]["skillNames"].is_array(),
        "skillNames 仍应存在（caps.skill_names=true）"
    );
}

/// 评审 LOW-1 回归：本地 skill 恰名 `mcp__x__y` 时，合并去重（本地优先），
/// availableCommands 不得出现两条同名条目（TUI 双显示 + 归类歧义）。
#[test]
fn test_build_update_merged_dedupes_name_collision_local_wins() {
    let local = vec![local_skill("mcp__demo__hello")];
    let mcp = vec![mcp_skill("mcp__demo__hello")];
    let caps = PeriCaps::all_enabled();

    let update = build_available_commands_update(&local, &mcp, &caps);
    let value = serde_json::to_value(&update).unwrap();
    let commands = value["availableCommands"].as_array().unwrap();
    let name_matches = commands
        .iter()
        .filter(|c| c["name"] == "mcp__demo__hello")
        .count();
    assert_eq!(name_matches, 1, "同名条目应去重为一条: {value}");

    // 去重后保留的是本地条目（description 为本地样式）
    let entry = commands
        .iter()
        .find(|c| c["name"] == "mcp__demo__hello")
        .unwrap();
    assert_eq!(
        entry["description"],
        serde_json::json!("Local mcp__demo__hello"),
        "本地条目应优先保留"
    );
}

#[test]
fn test_build_update_caps_behavior_unchanged() {
    let local = vec![local_skill("local-a")];
    let mcp = vec![mcp_skill("mcp__demo__hello")];

    // caps.skill_names = false：无 skillNames key（既有门控语义不变）
    let caps_off = PeriCaps {
        skill_names: false,
        ..PeriCaps::all_enabled()
    };
    let update_off = build_available_commands_update(&local, &mcp, &caps_off);
    let value_off = serde_json::to_value(&update_off).unwrap();
    assert!(
        value_off["_meta"].get("skillNames").is_none(),
        "caps.skill_names=false 时不得有 skillNames"
    );
    assert!(
        value_off["_meta"]["mcpSkillNames"].is_array(),
        "mcpSkillNames 不加 cap 门控，仍应附加"
    );

    // caps.skill_names = true：skillNames 存在
    let caps_on = PeriCaps::all_enabled();
    let update_on = build_available_commands_update(&local, &mcp, &caps_on);
    let value_on = serde_json::to_value(&update_on).unwrap();
    assert_eq!(
        value_on["_meta"]["skillNames"],
        serde_json::json!(["local-a"])
    );
}
