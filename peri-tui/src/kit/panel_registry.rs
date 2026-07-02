//! Panel registry——`PanelKind` 元数据 + open/close/toggle 操作。
//!
//! 这是 kit 路径"面板系统"的入口——所有 14 种面板的快捷键映射、标题、互斥组
//! 规则、原子操作都集中在这里。
//!
//! ## 互斥组语义
//!
//! 同 `MutexGroup` 面板不可同时打开（参见 `panel_types.rs::mutex_group`）。
//! `open_panel(kind)` 在打开新面板前会关闭同组其他面板——这保证栈中
//! `Vec<PanelKind>` 不会同时含两个同组面板。

use ratatui_kit::crossterm::event::KeyCode;
use std::borrow::Cow;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};

/// Panel 元数据——编译期穷举。
#[derive(Debug, Clone, Copy)]
pub struct PanelMeta {
    pub kind: PanelKind,
    pub title: &'static str,
    /// 触发面板的快捷键字母（小写）。`KeyCode::Char(letter)` + Ctrl。
    pub shortcut_letter: char,
    pub description: &'static str,
}

/// 所有 14 面板的元数据。
///
/// 快捷键分配（避开 Ctrl+C 全局 quit 和 Ctrl+K cycle permission mode）：
/// - Ctrl+M = Model（替代 legacy cycle model）
/// - Ctrl+T = ThreadBrowser
/// - Ctrl+R = Cron
/// - Ctrl+S = Status
/// - Ctrl+L = Login
/// - Ctrl+H = Hooks
/// - Ctrl+J = Tasks
/// - Ctrl+B = Betas
/// - Ctrl+P = Plugin
/// - Ctrl+G = Agent
/// - Ctrl+F = Config
/// - Ctrl+W = Workflow
/// - Ctrl+N = Memory
/// - Ctrl+X = Mcp
pub const PANELS: &[PanelMeta] = &[
    PanelMeta {
        kind: PanelKind::Model,
        title: "Model",
        shortcut_letter: 'm',
        description: "Model alias selection",
    },
    PanelMeta {
        kind: PanelKind::Login,
        title: "Login",
        shortcut_letter: 'l',
        description: "Provider credentials",
    },
    PanelMeta {
        kind: PanelKind::Agent,
        title: "Agent",
        shortcut_letter: 'g',
        description: "Subagent definitions",
    },
    PanelMeta {
        kind: PanelKind::Hooks,
        title: "Hooks",
        shortcut_letter: 'h',
        description: "Hook events",
    },
    PanelMeta {
        kind: PanelKind::Config,
        title: "Config",
        shortcut_letter: 'f',
        description: "PeriConfig editor",
    },
    PanelMeta {
        kind: PanelKind::ThreadBrowser,
        title: "Threads",
        shortcut_letter: 't',
        description: "Thread history browser",
    },
    PanelMeta {
        kind: PanelKind::Mcp,
        title: "MCP",
        shortcut_letter: 'x',
        description: "MCP server pool",
    },
    PanelMeta {
        kind: PanelKind::Plugin,
        title: "Plugin",
        shortcut_letter: 'p',
        description: "Installed plugins",
    },
    PanelMeta {
        kind: PanelKind::Cron,
        title: "Cron",
        shortcut_letter: 'r',
        description: "Scheduled tasks",
    },
    PanelMeta {
        kind: PanelKind::Status,
        title: "Status",
        shortcut_letter: 's',
        description: "Service snapshot",
    },
    PanelMeta {
        kind: PanelKind::Memory,
        title: "Memory",
        shortcut_letter: 'n',
        description: "Persisted memory",
    },
    PanelMeta {
        kind: PanelKind::Tasks,
        title: "Tasks",
        shortcut_letter: 'j',
        description: "Background tasks",
    },
    PanelMeta {
        kind: PanelKind::Betas,
        title: "Betas",
        shortcut_letter: 'b',
        description: "Feature flags",
    },
    PanelMeta {
        kind: PanelKind::Workflow,
        title: "Workflow",
        shortcut_letter: 'w',
        description: "Workflow runs",
    },
];

pub fn slash_command_for_panel(kind: PanelKind) -> Cow<'static, str> {
    match kind {
        PanelKind::ThreadBrowser => Cow::Borrowed("threads"),
        other => Cow::Owned(
            meta(other)
                .map(|m| m.title.to_ascii_lowercase())
                .unwrap_or_default(),
        ),
    }
}

pub fn panel_for_slash_command(command: &str) -> Option<PanelKind> {
    let normalized = command.trim_start_matches('/').to_ascii_lowercase();
    PANELS
        .iter()
        .find(|m| slash_command_for_panel(m.kind) == normalized)
        .map(|m| m.kind)
}

/// 查找面板元数据。未注册返回 None。
pub fn meta(kind: PanelKind) -> Option<&'static PanelMeta> {
    PANELS.iter().find(|m| m.kind == kind)
}

/// 按快捷键字母反查 PanelKind。未注册返回 None。
pub fn from_shortcut(letter: char) -> Option<PanelKind> {
    let lower = letter.to_ascii_lowercase();
    PANELS
        .iter()
        .find(|m| m.shortcut_letter == lower)
        .map(|m| m.kind)
}

/// 将 crossterm 的 Ctrl+Char 事件映射到 PanelKind。
///
/// 调用方已确认 Ctrl 修饰键按下。返回 None 表示该字母未注册任何面板
/// （也可能是 Ctrl+C/K 等保留快捷键）。
pub fn from_key_code(code: KeyCode) -> Option<PanelKind> {
    if let KeyCode::Char(ch) = code {
        from_shortcut(ch)
    } else {
        None
    }
}

// ── 面板栈操作（mutates OPEN_PANELS / ACTIVE_PANEL atoms） ──────────────────

/// 打开面板：应用互斥组规则后压入栈顶并设为 ACTIVE_PANEL。
///
/// - 若面板已在栈中：把它移到栈顶（不重复 push）。
/// - 若同 MutexGroup 有其他面板：先关闭它们。
/// - 若面板不在栈中：push 到栈尾（栈顶）。
pub fn open_panel(kind: PanelKind) {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let group = kind.mutex_group();
    let mut current = open_atom.read().clone();

    // 关闭同 MutexGroup 的其他面板（除 kind 自身）
    current.retain(|k| *k == kind || k.mutex_group() != group);

    // 若 kind 已在栈中，先移除（稍后 push 到栈顶）
    current.retain(|k| *k != kind);

    // push 到栈顶
    current.push(kind);

    *open_atom.write() = current;
    *active_atom.write() = Some(kind);
}

/// 关闭栈顶（ACTIVE_PANEL）面板，弹出后新的栈顶成为 active。
///
/// 返回被关闭的 PanelKind（若有），调用方可用于日志/状态反馈。
pub fn close_active_panel() -> Option<PanelKind> {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let mut current = open_atom.read().clone();
    let closed = current.pop();
    let next_active = current.last().copied();
    *open_atom.write() = current;
    *active_atom.write() = next_active;
    closed
}

/// 关闭指定面板：从栈中移除，若它原本是栈顶则更新 ACTIVE_PANEL。
pub fn close_panel(kind: PanelKind) -> bool {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let mut current = open_atom.read().clone();
    let before_len = current.len();
    current.retain(|k| *k != kind);
    let removed = current.len() < before_len;
    if removed {
        let next_active = current.last().copied();
        *open_atom.write() = current;
        *active_atom.write() = next_active;
    }
    removed
}

/// Toggle：若已打开则关闭，否则打开。返回操作后的最终状态（true=已打开）。
pub fn toggle_panel(kind: PanelKind) -> bool {
    let is_open = OPEN_PANELS.state().read().contains(&kind);
    if is_open {
        close_panel(kind);
        false
    } else {
        open_panel(kind);
        true
    }
}

/// 关闭所有面板（清空栈）。
pub fn close_all_panels() {
    *OPEN_PANELS.state().write() = Vec::new();
    *ACTIVE_PANEL.state().write() = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::panel_types::MutexGroup;
    use serial_test::serial;

    fn setup_atoms() {
        crate::kit::atoms::init_atoms();
        // 重置为空
        *OPEN_PANELS.state().write() = Vec::new();
        *ACTIVE_PANEL.state().write() = None;
    }

    #[test]
    fn test_meta_all_14_panels_present() {
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
    fn test_from_shortcut_round_trip() {
        for m in PANELS {
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
    ];

    /// 编译期断言：MutexGroup 实现了 PartialEq（测试需要）。
    #[test]
    fn test_mutex_group_partial_eq() {
        fn assert_eq<T: PartialEq>() {}
        assert_eq::<MutexGroup>();
    }
}
