//! Tests for panel_registry
#[cfg(test)]
use super::*;
#[cfg(test)]
use ratatui_kit::ratatui::layout::Constraint;
#[cfg(test)]
use serial_test::serial;

#[cfg(test)]
fn setup_atoms() {
    crate::kit::atoms::init_atoms();
    // 重置为空
    *OPEN_PANELS.state().write() = Vec::new();
    *ACTIVE_PANEL.state().write() = None;
}

#[test]
fn test_meta_all_panels_present() {
    // 验证 PANELS 穷举所有 PanelKind
    for kind in ALL_PANEL_KINDS {
        assert!(
            meta(*kind).is_some(),
            "PanelKind {:?} missing from PANELS",
            kind
        );
    }
    assert_eq!(PANELS.len(), ALL_PANEL_KINDS.len());
}

#[test]
fn test_shortcuts_unique() {
    let mut seen = std::collections::HashSet::new();
    for m in PANELS {
        // '\0' 表示无快捷键，允许多个面板共用
        if m.shortcut_letter == '\0' {
            continue;
        }
        assert!(
            seen.insert(m.shortcut_letter),
            "duplicate shortcut letter {} for {:?}",
            m.shortcut_letter,
            m.kind
        );
    }
}

#[test]
#[serial]
fn test_open_panel_pushes_to_stack() {
    setup_atoms();
    open_panel(PanelKind::Model);
    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::Model]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Model));
}

#[test]
#[serial]
fn test_open_two_panels_different_groups() {
    setup_atoms();
    open_panel(PanelKind::Model); // Settings 组
    open_panel(PanelKind::Cron); // Tools 组
    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::Model, PanelKind::Cron]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Cron));
}

#[test]
#[serial]
fn test_open_same_mutex_group_replaces() {
    setup_atoms();
    // Model 和 Config 都属于 Settings 组
    open_panel(PanelKind::Model);
    open_panel(PanelKind::Config);
    let stack = OPEN_PANELS.state().read().clone();
    // Model 应被替换为 Config
    assert_eq!(stack, vec![PanelKind::Config]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Config));
}

#[test]
#[serial]
fn test_open_already_open_brings_to_top() {
    setup_atoms();
    open_panel(PanelKind::Model); // Settings
    open_panel(PanelKind::Cron); // Tools（不同组，共存）
    open_panel(PanelKind::Model); // 再次打开 Model——应移到栈顶

    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::Cron, PanelKind::Model]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Model));
}

#[test]
#[serial]
fn test_close_active_panel() {
    setup_atoms();
    open_panel(PanelKind::Model);
    open_panel(PanelKind::Cron); // 栈: [Model, Cron], active=Cron

    let closed = close_active_panel();
    assert_eq!(closed, Some(PanelKind::Cron));
    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::Model]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Model));
}

#[test]
#[serial]
fn test_close_active_when_empty_returns_none() {
    setup_atoms();
    let closed = close_active_panel();
    assert_eq!(closed, None);
}

#[test]
#[serial]
fn test_close_panel_specific() {
    setup_atoms();
    open_panel(PanelKind::Model);
    open_panel(PanelKind::Cron); // 栈: [Model, Cron]

    // 关闭非栈顶的 Model
    let removed = close_panel(PanelKind::Model);
    assert!(removed);
    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::Cron]);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Cron));
}

#[test]
#[serial]
fn test_close_panel_not_present_returns_false() {
    setup_atoms();
    let removed = close_panel(PanelKind::Model);
    assert!(!removed);
}

#[test]
#[serial]
fn test_toggle_panel() {
    setup_atoms();
    let opened = toggle_panel(PanelKind::Model);
    assert!(opened);
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Model));

    let closed = toggle_panel(PanelKind::Model);
    assert!(!closed);
    assert!(OPEN_PANELS.state().read().is_empty());
    assert_eq!(*ACTIVE_PANEL.state().read(), None);
}

#[test]
#[serial]
fn test_close_all_panels() {
    setup_atoms();
    open_panel(PanelKind::Model);
    open_panel(PanelKind::Cron);
    close_all_panels();
    assert!(OPEN_PANELS.state().read().is_empty());
    assert_eq!(*ACTIVE_PANEL.state().read(), None);
}

// ── §6.7 SubAgentDetail pane（Slice 2）───────────────────────────────────

#[test]
#[serial]
fn test_subagent_detail_panel_registered() {
    // 穷举矩阵：meta 存在、标题/描述 FTL key 存在、无快捷键（Enter 分派打开）
    let m = meta(PanelKind::SubAgentDetail).expect("SubAgentDetail 必须注册");
    assert_eq!(m.shortcut_letter, '\0', "详情 pane 无快捷键");
    assert!(!panel_title(PanelKind::SubAgentDetail).trim().is_empty());
    assert!(!panel_description(PanelKind::SubAgentDetail).is_empty());
    assert_eq!(m.mutex_group, MutexGroup::Agent, "与 Agent 面板同互斥组");
}

#[test]
#[serial]
fn test_subagent_detail_opens_and_replaces_agent_group() {
    setup_atoms();
    open_panel(PanelKind::Agent);
    // 与 Agent 同 MutexGroup：打开详情关闭 Agent 面板（栈顶唯一）
    open_panel(PanelKind::SubAgentDetail);
    let stack = OPEN_PANELS.state().read().clone();
    assert_eq!(stack, vec![PanelKind::SubAgentDetail]);
    assert_eq!(
        *ACTIVE_PANEL.state().read(),
        Some(PanelKind::SubAgentDetail)
    );
    // Esc 单层关闭（close_active_panel 弹栈）
    let closed = close_active_panel();
    assert_eq!(closed, Some(PanelKind::SubAgentDetail));
    assert!(OPEN_PANELS.state().read().is_empty());
}

#[test]
fn test_from_key_code_ctrl_m_maps_to_model() {
    assert_eq!(from_key_code(KeyCode::Char('m')), Some(PanelKind::Model));
    assert_eq!(from_key_code(KeyCode::Char('M')), Some(PanelKind::Model));
}

#[test]
fn test_from_key_code_unmapped_letter_returns_none() {
    // 'c' 和 'k' 未注册面板（Ctrl+C/K 保留）
    assert_eq!(from_key_code(KeyCode::Char('c')), None);
    assert_eq!(from_key_code(KeyCode::Char('k')), None);
    // 数字 / 其他按键也应返回 None
    assert_eq!(from_key_code(KeyCode::Enter), None);
}

#[test]
fn test_slash_commands_unique() {
    let mut seen = std::collections::HashSet::new();
    for m in PANELS {
        // 空命令 = 无 slash 入口（AskUser / SubAgentDetail 等 Enter 分派打开的面板）
        if m.slash_command.is_empty() {
            continue;
        }
        assert!(
            seen.insert(m.slash_command),
            "duplicate slash command {} for {:?}",
            m.slash_command,
            m.kind
        );
        assert_eq!(panel_for_slash_command(m.slash_command), Some(m.kind));
        assert_eq!(slash_command_for_panel(m.kind), m.slash_command);
    }
}

#[test]
fn test_history_aliases_map_to_thread_browser() {
    assert_eq!(
        panel_for_slash_command("history"),
        Some(PanelKind::ThreadBrowser)
    );
    assert_eq!(
        panel_for_slash_command("/history"),
        Some(PanelKind::ThreadBrowser)
    );
    assert_eq!(
        panel_for_slash_command("/his"),
        Some(PanelKind::ThreadBrowser)
    );
}

#[test]
fn test_registry_metadata_has_expected_shape() {
    for m in PANELS {
        assert!(m.priority < PANELS.len() as u8);
    }
    assert_eq!(
        meta(PanelKind::Model).map(|m| m.mutex_group),
        Some(MutexGroup::Settings)
    );
    assert_eq!(
        meta(PanelKind::ThreadBrowser).map(|m| m.scope),
        Some(PanelScope::Session)
    );
    assert_eq!(
        meta(PanelKind::Workflow).map(|m| m.scope),
        Some(PanelScope::Global)
    );
}

#[test]
fn test_panel_constraint_maps_panel_size() {
    assert_eq!(panel_constraint(PanelSize::Fill), Constraint::Fill(1));
    assert_eq!(
        panel_constraint(PanelSize::Length(42)),
        Constraint::Length(42)
    );
}

#[test]
fn test_render_all_registered_panels_constructs_element() {
    for m in PANELS {
        let _panel = render(m.kind).expect("registered panel should render");
    }
}

#[test]
fn test_from_shortcut_round_trip() {
    for m in PANELS {
        // '\0' 表示无快捷键，跳过 round-trip 检查（多个面板共用 '\0'）
        if m.shortcut_letter == '\0' {
            continue;
        }
        assert_eq!(from_shortcut(m.shortcut_letter), Some(m.kind));
    }
}

/// 所有 PanelKind 变体的常量数组（测试辅助）。
const ALL_PANEL_KINDS: &[PanelKind] = &[
    PanelKind::Model,
    PanelKind::Login,
    PanelKind::Agent,
    PanelKind::Hooks,
    PanelKind::Config,
    PanelKind::ThreadBrowser,
    PanelKind::Mcp,
    PanelKind::Plugin,
    PanelKind::Cron,
    PanelKind::Status,
    PanelKind::Memory,
    PanelKind::Tasks,
    PanelKind::Betas,
    PanelKind::Workflow,
    PanelKind::AskUser,
    PanelKind::Theme,
    PanelKind::SubAgentDetail,
];

/// 编译期断言：MutexGroup 实现了 PartialEq（测试需要）。
#[test]
fn test_mutex_group_partial_eq() {
    fn assert_eq<T: PartialEq>() {}
    assert_eq::<MutexGroup>();
}
