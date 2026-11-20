use super::button_label_width;
use super::content_rows;
use super::oauth_wrap_width;

/// CJK 标签按显示列宽计算：`[ 打开浏览器 ]` = 方括号 2 + 空格 2 + 5 个双宽字符 10 = 14。
/// 旧实现用 `chars().count()` 只得到 9，点击会落到相邻按钮。
#[test]
fn button_label_width_counts_cjk_as_two_columns() {
    assert_eq!(button_label_width("打开浏览器"), 14);
}

/// ASCII 标签下显示列宽等于字符数：`[ Open Browser ]` = 1 + 1 + 12 + 1 + 1。
#[test]
fn button_label_width_matches_char_count_for_ascii() {
    assert_eq!(button_label_width("Open Browser"), 16);
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
