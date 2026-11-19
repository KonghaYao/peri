use super::button_label_width;

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
