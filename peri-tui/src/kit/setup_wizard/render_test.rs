//! Setup wizard provider 列表渲染回归：模型别名截断必须落在字符边界上。

use super::*;
use crate::app::setup_wizard::SetupWizardState;
use ratatui_kit::ratatui::style::Color;

fn render_alias_rows(state: &SetupWizardState) -> Vec<String> {
    let (_, lines) = render_form_step(
        state,
        Color::DarkGray,
        Color::Cyan,
        Color::Yellow,
        Color::White,
        Color::Blue,
        Color::Red,
    );
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn state_with_alias(alias: String) -> SetupWizardState {
    let mut state = SetupWizardState::default();
    state.providers[0].aliases = std::array::from_fn(|_| alias.clone());
    state
}

/// 42 字节 CJK（每个 3 字节）：第 37 字节落在字符内部。
#[test]
fn cjk_alias_crossing_byte_boundary_does_not_panic() {
    let alias = "模".repeat(14);
    assert!(alias.len() > 40 && !alias.is_char_boundary(37));

    let rows = render_alias_rows(&state_with_alias(alias));

    assert!(
        rows.iter().any(|row| row.contains('→')),
        "别名行应正常渲染: {rows:?}"
    );
}

/// emoji 为 4 字节：11 个 = 44 字节，第 37 字节同样落在字符内部。
#[test]
fn emoji_alias_crossing_byte_boundary_does_not_panic() {
    let alias = "🚀".repeat(11);
    assert!(alias.len() > 40 && !alias.is_char_boundary(37));

    let rows = render_alias_rows(&state_with_alias(alias));

    assert!(
        rows.iter().any(|row| row.contains('🚀')),
        "emoji 别名不应被切断: {rows:?}"
    );
}

#[test]
fn long_ascii_alias_is_truncated_with_ellipsis() {
    let alias = "a".repeat(50);

    let rows = render_alias_rows(&state_with_alias(alias.clone()));

    let alias_row = rows
        .iter()
        .find(|row| row.contains('→'))
        .expect("应有别名行");
    assert!(
        alias_row.ends_with('…'),
        "超长别名应带省略号: {alias_row:?}"
    );
    assert!(!alias_row.contains(&alias), "超长别名应被截断");
}

#[test]
fn short_alias_is_displayed_verbatim() {
    let alias = "claude-opus-4-6";

    let rows = render_alias_rows(&state_with_alias(alias.to_string()));

    let alias_row = rows
        .iter()
        .find(|row| row.contains('→'))
        .expect("应有别名行");
    assert!(
        alias_row.ends_with(alias),
        "短别名应原样显示: {alias_row:?}"
    );
}
