use super::button_label_width;
use super::button_row;
use super::content_rows;
use super::oauth_wrap_width;

use ratatui_kit::ratatui::style::Modifier;

fn dark_theme() -> peri_theme::theme::ThemeDefinition {
    peri_theme::builtin::dark_theme()
}

/// CJK 标签按显示列宽计算：Filled ` 打开浏览器 ` = 空格 2 + 5 个双宽字符 10 = 12。
/// 旧实现用 `chars().count()` 只得到 9，点击会落到相邻按钮。
#[test]
fn button_label_width_counts_cjk_as_two_columns() {
    assert_eq!(button_label_width("打开浏览器", &dark_theme()), 12);
}

/// ASCII 标签下显示列宽等于字符数：Filled ` Open Browser ` = 1 + 12 + 1。
#[test]
fn button_label_width_matches_char_count_for_ascii() {
    assert_eq!(button_label_width("Open Browser", &dark_theme()), 14);
}

/// 回归：按钮行不再输出 `[ ]`，每个按钮 span 都带 `REVERSED`（Filled 反色块），
/// 选中项另外加 BOLD。
#[test]
fn button_row_is_filled_without_brackets() {
    let theme = dark_theme();
    let line = button_row(true, 0, &theme);
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        !text.contains('[') && !text.contains(']'),
        "Filled 按钮不得再有方括号：{text}"
    );
    let buttons: Vec<_> = line
        .spans
        .iter()
        .filter(|span| !span.content.trim().is_empty())
        .collect();
    assert_eq!(buttons.len(), 3, "按钮行必须渲染三个按钮：{text}");
    for span in &buttons {
        assert!(
            span.style.add_modifier.contains(Modifier::REVERSED),
            "Filled 按钮必须反色：{:?}",
            span.content
        );
    }
    assert!(
        buttons[0].style.add_modifier.contains(Modifier::BOLD),
        "选中项必须是 Focused 态（BOLD）：{:?}",
        buttons[0].content
    );
    assert!(
        !buttons[1].style.add_modifier.contains(Modifier::BOLD),
        "非选中项是 Normal 态：{:?}",
        buttons[1].content
    );
}

/// 宽弹窗保持 52 列版式；窄弹窗按可用宽度收窄，绘制与按钮行号同源。
#[test]
fn url_wraps_to_popup_width_and_rows_follow() {
    assert_eq!(oauth_wrap_width(90), 52);
    assert_eq!(oauth_wrap_width(24), 22);
    assert_eq!(oauth_wrap_width(1), 1);

    let url = format!("https://example.com/{}", "p".repeat(40));
    let wide = content_rows(&url, 90);
    let narrow = content_rows(&url, 24);
    assert!(narrow.0 > wide.0, "窄屏换行行数必须增加");
    let expected = crate::kit::text_util::wrap_text(&url, 22).len() as u16;
    assert_eq!(narrow, (6 + expected, 8 + expected));
}

/// CJK 链接按显示列宽换行（每字符 2 列），不是按码点。
#[test]
fn cjk_url_wraps_by_display_columns() {
    let url = "你好世界".repeat(6); // 24 个字符 / 48 列
    let rows = crate::kit::text_util::wrap_text(&url, oauth_wrap_width(24));
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|line| {
        unicode_width::UnicodeWidthStr::width(line.as_str()) <= oauth_wrap_width(24)
    }));
}
