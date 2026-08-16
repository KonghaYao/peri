//! Tests for kit::ui_command — ui 域命令单源模块。

#[cfg(test)]
use super::*;

#[test]
fn test_ui_command_specs_non_empty() {
    let specs = ui_command_specs();
    assert!(!specs.is_empty(), "ui 域清单不得为空");
    // PANELS（slash_command 非空，15 条）+ /setup = 16 条
    assert_eq!(specs.len(), 16);
}

#[test]
fn test_ui_command_specs_contains_setup_and_history_alias() {
    let specs = ui_command_specs();
    // /setup 条目恒存在
    assert!(
        specs.iter().any(|s| s.name == "setup"),
        "清单必须含 setup 条目"
    );
    // history 是 threads（ThreadBrowser 面板）的别名，挂在对应条目上
    let threads = specs
        .iter()
        .find(|s| s.name == "threads")
        .expect("清单必须含 threads（ThreadBrowser 面板）条目");
    assert!(
        threads.aliases.contains(&"history"),
        "threads 条目必须携带 history 别名"
    );
}

#[test]
fn test_ui_command_specs_names_unique() {
    // 上送注册以 name 为 ui:<name> 唯一键，清单内不得重名
    let mut seen = std::collections::HashSet::new();
    for s in ui_command_specs() {
        assert!(seen.insert(s.name), "duplicate ui command name {}", s.name);
    }
}

#[test]
fn test_resolve_ui_command_bare_name_hit() {
    assert_eq!(
        resolve_ui_command("model"),
        Some(UiCommandAction::OpenPanel(PanelKind::Model))
    );
    assert_eq!(
        resolve_ui_command("threads"),
        Some(UiCommandAction::OpenPanel(PanelKind::ThreadBrowser))
    );
    assert_eq!(
        resolve_ui_command("workflows"),
        Some(UiCommandAction::OpenPanel(PanelKind::Workflow))
    );
}

#[test]
fn test_resolve_ui_command_aliases() {
    for alias in ["history", "resume", "his"] {
        assert_eq!(
            resolve_ui_command(alias),
            Some(UiCommandAction::OpenPanel(PanelKind::ThreadBrowser)),
            "别名 {alias} 应映射到 ThreadBrowser"
        );
    }
}

#[test]
fn test_resolve_ui_command_ui_prefix() {
    assert_eq!(
        resolve_ui_command("ui:history"),
        Some(UiCommandAction::OpenPanel(PanelKind::ThreadBrowser))
    );
    assert_eq!(
        resolve_ui_command("ui:model"),
        Some(UiCommandAction::OpenPanel(PanelKind::Model))
    );
    assert_eq!(
        resolve_ui_command("ui:setup"),
        Some(UiCommandAction::ToggleSetup)
    );
}

#[test]
fn test_resolve_ui_command_setup_bare() {
    assert_eq!(
        resolve_ui_command("setup"),
        Some(UiCommandAction::ToggleSetup)
    );
}

#[test]
fn test_resolve_ui_command_case_insensitive() {
    assert_eq!(
        resolve_ui_command("History"),
        Some(UiCommandAction::OpenPanel(PanelKind::ThreadBrowser))
    );
    assert_eq!(
        resolve_ui_command("UI:model"),
        Some(UiCommandAction::OpenPanel(PanelKind::Model))
    );
}

#[test]
fn test_resolve_ui_command_miss() {
    assert_eq!(resolve_ui_command("unknown"), None);
    assert_eq!(resolve_ui_command(""), None);
    assert_eq!(resolve_ui_command("clear"), None, "core 域裸名不归 ui 域");
    assert_eq!(resolve_ui_command("compact"), None, "core 域裸名不归 ui 域");
}

#[test]
fn test_resolve_ui_command_non_ui_domain_falls_through() {
    // 非 ui 域显式形态一律 fall through（设计 §78），不得误拦
    assert_eq!(resolve_ui_command("core:compact"), None);
    assert_eq!(
        resolve_ui_command("core:model"),
        None,
        "core:model 不得命中 Model 面板"
    );
    assert_eq!(resolve_ui_command("mcp:demo:hello"), None);
    assert_eq!(
        resolve_ui_command("mcp:demo:model"),
        None,
        "mcp 完整形态不得命中面板名"
    );
    assert_eq!(resolve_ui_command("plugin:ecc:deploy"), None);
    assert_eq!(resolve_ui_command("unknown:foo"), None);
    // ui 域层数上限 2 段冒号（设计 §52）：`ui:` 前缀后仍含冒号即词法非法
    assert_eq!(
        resolve_ui_command("ui:foo:bar"),
        None,
        "ui: 双层形态（3 段冒号）不得命中"
    );
    assert_eq!(
        resolve_ui_command("ui:ui:model"),
        None,
        "双重 ui: 前缀不得命中 Model 面板"
    );
}
