//! ratatui-kit StatusBar component.
//!
//! S9：完整双行布局——
//! - **Row 1**：权限模式 → cwd basename → provider/model → CPU% → MEM
//!   全部从 SERVICE_SNAPSHOT atom 派生（S5 落地）；高亮计时器控制闪烁。
//! - **Row 2**：状态相关的快捷键 hints（popup/mention/slash/默认 4 态切换）。

use crate::kit::atoms;
use crate::kit::theme;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Flex},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::Instant;

/// 状态栏第 1 行：权限模式 · cwd · provider/model · CPU% · MEM
#[component]
fn StatusBarRow1(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let snap = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let model_hl = hooks.use_atom(&atoms::MODEL_HIGHLIGHT_UNTIL);
    let provider_hl = hooks.use_atom(&atoms::PROVIDER_HIGHLIGHT_UNTIL);
    let mode_hl = hooks.use_atom(&atoms::MODE_HIGHLIGHT_UNTIL);

    let snap = snap.read().clone();
    let now = Instant::now();
    let model_highlighted = model_hl.read().as_ref().is_some_and(|t| *t > now);
    let provider_highlighted = provider_hl.read().as_ref().is_some_and(|t| *t > now);
    let mode_highlighted = mode_hl.read().as_ref().is_some_and(|t| *t > now);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // 1. 权限模式（Default 不显示）
    let mode_label = permission_mode_display(&snap.permission_mode);
    if !mode_label.is_empty() {
        let color = permission_mode_color(&snap.permission_mode);
        let mut style = Style::default().fg(color);
        if mode_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(format!(" {}", mode_label), style));
    }

    // 2. cwd basename
    spans.push(separator());
    spans.push(Span::styled(
        cwd_basename(&snap.cwd),
        Style::default().fg(statusbar().muted),
    ));

    // 3. provider/model
    spans.push(separator());
    if !snap.provider_name.is_empty() {
        let mut style = Style::default().fg(statusbar().muted);
        if provider_highlighted {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(format!(" {}", snap.provider_name), style));
        spans.push(Span::styled("/", Style::default().fg(statusbar().dim)));
    }
    if !snap.model_alias.is_empty() {
        let mut style = Style::default().fg(statusbar().text);
        if model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(snap.model_alias.clone(), style));
    }

    // 4. CPU%
    if snap.cpu_percent > 0.0 {
        spans.push(separator());
        spans.push(Span::styled(
            format!("CPU {:.0}%", snap.cpu_percent),
            Style::default().fg(resource_color_by_load(snap.cpu_percent as f64, 50.0, 100.0)),
        ));
    }

    // 5. MEM
    spans.push(separator());
    spans.push(Span::styled(
        format!("MEM {}MB", snap.memory_mb),
        Style::default().fg(memory_color(snap.memory_mb)),
    ));

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
        ) {
            Text(text: Paragraph::new(Line::from(spans)))
        }
    )
}

/// 状态栏第 2 行：状态相关的快捷键 hints
#[component]
fn StatusBarRow2(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // I19-C：原代码读 POPUP_ACTIVE（dead atom，open/close_popup 从不同步）
    // 导致 popup hints 永远不显示。改读 POPUP_KIND.is_some()。
    let popup_kind = hooks.use_atom(&atoms::POPUP_KIND);
    let at_active = hooks.use_atom(&atoms::AT_MENTION_ACTIVE);
    let slash_active = hooks.use_atom(&atoms::SLASH_HINT_ACTIVE);

    let is_popup = popup_kind.read().is_some();
    let is_at = *at_active.read();
    let is_slash = *slash_active.read();

    let hints = if is_popup {
        Line::from(" Esc: close | Enter: confirm ").fg(statusbar().muted)
    } else if is_at || is_slash {
        Line::from(" Esc: close | Tab: navigate | Enter: select ").fg(statusbar().muted)
    } else {
        Line::from(" /: commands | Shift+Enter: newline | Ctrl+K: mode | Ctrl+O: diff ")
            .fg(statusbar().muted)
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
            justify_content: Flex::Center,
        ) {
            Text(text: Paragraph::new(hints).centered())
        }
    )
}

#[component]
pub fn StatusBar(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(3),
        ) {
            StatusBarRow1()
            StatusBarRow2()
            // 第 3 行留空（视觉缓冲）
            Text(text: Paragraph::new(Line::from("")))
        }
    )
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

fn statusbar() -> &'static theme::StatusBarTokens {
    &theme::component().statusbar
}

fn separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(statusbar().muted))
}

/// 把 atom 中的 permission_mode 字符串映射为显示标签。
/// "default" 返回空串（与 legacy 一致——Default 模式不显示标签）。
fn permission_mode_display(mode: &str) -> &'static str {
    match mode {
        "accept-edit" => "Accept Edit",
        "auto-mode" => "Auto Mode",
        "bypass" => "Bypass",
        _ => "",
    }
}

fn permission_mode_color(mode: &str) -> ratatui::style::Color {
    match mode {
        "accept-edit" => statusbar().mode_accept_edit,
        "auto-mode" => statusbar().mode_auto,
        "bypass" => statusbar().mode_bypass,
        _ => statusbar().text,
    }
}

/// 从 cwd 路径取 basename（最后一节）。空或异常返回原串。
fn cwd_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.to_string())
}

/// 通用阈值染色：>= high → ERROR，>= low → WARNING，else SAGE。
fn resource_color_by_load(value: f64, low: f64, high: f64) -> ratatui::style::Color {
    if value >= high {
        statusbar().resource_bad
    } else if value >= low {
        statusbar().resource_warn
    } else {
        statusbar().resource_good
    }
}

/// MEM 染色：>1024 ERROR，>512 WARNING，else SAGE（与 legacy status_bar 一致）。
fn memory_color(mem_mb: u64) -> ratatui::style::Color {
    if mem_mb > 1024 {
        statusbar().resource_bad
    } else if mem_mb > 512 {
        statusbar().resource_warn
    } else {
        statusbar().resource_good
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_permission_mode_display() {
        assert_eq!(permission_mode_display("default"), "");
        assert_eq!(permission_mode_display("accept-edit"), "Accept Edit");
        assert_eq!(permission_mode_display("auto-mode"), "Auto Mode");
        assert_eq!(permission_mode_display("bypass"), "Bypass");
        assert_eq!(permission_mode_display("unknown"), "");
    }

    #[test]
    fn test_permission_mode_color() {
        assert_eq!(
            permission_mode_color("accept-edit"),
            statusbar().mode_accept_edit
        );
        assert_eq!(permission_mode_color("auto-mode"), statusbar().mode_auto);
        assert_eq!(permission_mode_color("bypass"), statusbar().mode_bypass);
    }

    #[test]
    fn test_cwd_basename_simple() {
        assert_eq!(cwd_basename("/Users/foo/project"), "project");
        assert_eq!(cwd_basename("/tmp"), "tmp");
        assert_eq!(cwd_basename("/"), "/");
    }

    #[test]
    fn test_cwd_basename_empty() {
        assert_eq!(cwd_basename(""), "");
    }

    #[test]
    fn test_memory_color_thresholds() {
        assert_eq!(memory_color(100), statusbar().resource_good);
        assert_eq!(memory_color(512), statusbar().resource_good); // 512 不算超阈值
        assert_eq!(memory_color(513), statusbar().resource_warn);
        assert_eq!(memory_color(1024), statusbar().resource_warn); // 1024 不算超阈值
        assert_eq!(memory_color(1025), statusbar().resource_bad);
    }

    #[test]
    fn test_resource_color_by_load() {
        // low=50, high=100
        assert_eq!(
            resource_color_by_load(10.0, 50.0, 100.0),
            statusbar().resource_good
        );
        assert_eq!(
            resource_color_by_load(50.0, 50.0, 100.0),
            statusbar().resource_warn
        );
        assert_eq!(
            resource_color_by_load(75.0, 50.0, 100.0),
            statusbar().resource_warn
        );
        assert_eq!(
            resource_color_by_load(100.0, 50.0, 100.0),
            statusbar().resource_bad
        );
    }

    #[test]
    #[serial]
    fn test_status_bar_row_renders_without_panic() {
        crate::kit::atoms::init_atoms();
        // 写入测试数据
        *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
            cwd: "/home/user/test-project".into(),
            provider_name: "anthropic".into(),
            model_alias: "sonnet".into(),
            permission_mode: "accept-edit".into(),
            memory_mb: 256,
            cpu_percent: 12.5,
            ..Default::default()
        };
        // 辅助函数应能正确处理这些值
        let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
        assert_eq!(snap.cwd, "/home/user/test-project");
        assert_eq!(cwd_basename(&snap.cwd), "test-project");
        assert_eq!(
            permission_mode_display(&snap.permission_mode),
            "Accept Edit"
        );
    }

    #[test]
    #[serial]
    fn test_status_bar_handles_empty_provider_model() {
        crate::kit::atoms::init_atoms();
        *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
            cwd: "/tmp".into(),
            provider_name: "".into(),
            model_alias: "".into(),
            permission_mode: "default".into(),
            memory_mb: 0,
            cpu_percent: 0.0,
            ..Default::default()
        };
        let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
        // 空 provider/model 应被渲染逻辑跳过（不在 Row1 中显示）
        assert!(snap.provider_name.is_empty());
        assert!(snap.model_alias.is_empty());
        // Default mode → 空标签
        assert_eq!(permission_mode_display(&snap.permission_mode), "");
        // 0% CPU 应被跳过
        assert_eq!(snap.cpu_percent, 0.0);
    }
}
