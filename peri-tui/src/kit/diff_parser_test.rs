//! 应用 adapter：默认预算、超限/二进制回落与 Edit/Write 摘要路径。
//!
//! 解析矩阵在 `peri-tui-components::diff` 的 `diff_test.rs`；这里只锁定应用
//! 公开签名与默认预算没有漂移。

use peri_tui_components::diff::DiffBudget;

use crate::kit::tui_render_unit::TuiHunkLineKind;

const BASIC_DIFF: &str = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
";

/// 应用入口沿用组件默认预算（300 行 / 4 hunk / 32 行每 hunk / 8 与 6 change）。
#[test]
fn wrapper_uses_default_budget() {
    let block =
        super::parse_unified_diff(BASIC_DIFF, Some("src/main.rs")).expect("标准 diff 应解析成功");
    assert_eq!(block.path, "src/main.rs");
    assert_eq!(block.hunks.len(), 1);
    assert_eq!(block.hunks[0].old_range, "-10,3");
    assert_eq!(block.hunks[0].new_range, "+10,3");
    assert_eq!(block.hunks[0].lines[1].kind, TuiHunkLineKind::Del);
    assert_eq!(block.hunks[0].lines[1].old_no, Some(11));
    assert_eq!((block.adds, block.dels), (1, 1));
    assert_eq!(DiffBudget::default().max_lines, super::MAX_PARSE_LINES);
    assert_eq!(
        DiffBudget::default().max_change_lines,
        super::MAX_CHANGE_LINES_PER_HUNK
    );
}

/// CRLF 输入在应用入口同样剥除行尾 `\r`。
#[test]
fn wrapper_parses_crlf() {
    let text = "--- a/x\r\n+++ b/x\r\n@@ -1,1 +1,1 @@\r\n-old\r\n+new\r\n";
    let block = super::parse_unified_diff(text, Some("x")).expect("CRLF diff 应解析成功");
    assert_eq!(block.hunks[0].lines[0].text, "old");
    assert_eq!(block.hunks[0].lines[1].text, "new");
}

/// 超限（默认 300 行）与二进制输入静默回落 None。
#[test]
fn wrapper_falls_back_for_oversized_and_binary() {
    let mut huge = String::from("--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n");
    for _ in 0..peri_tui_components::diff::MAX_PARSE_LINES {
        huge.push_str(" context\n");
    }
    assert_eq!(super::parse_unified_diff(&huge, Some("x")), None);

    assert_eq!(
        super::parse_unified_diff(
            "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n",
            Some("x.png")
        ),
        None
    );
}

/// Edit/Write 摘要回落：真实事件流无 unified diff 时仍产出 header 计数。
#[test]
fn wrapper_falls_back_to_edit_write_summary() {
    let added = super::parse_edit_write_summary("Added 2 lines to src/x.rs", None)
        .expect("Added 摘要应解析成功");
    assert_eq!(added.path, "src/x.rs");
    assert_eq!((added.adds, added.dels), (2, 0));
    assert!(added.hunks.is_empty());

    let wrote = super::parse_edit_write_summary("Wrote 1 line to /tmp/new.rs", Some("/abs/new.rs"))
        .expect("Wrote 摘要应解析成功");
    assert_eq!(wrote.path, "/abs/new.rs");
    assert!(wrote.is_new_file);
    assert_eq!((wrote.adds, wrote.dels), (1, 0));

    assert_eq!(
        super::parse_edit_write_summary("Replaced text (same line count) to x.rs", None),
        None,
        "无计数摘要保持回落"
    );
}
