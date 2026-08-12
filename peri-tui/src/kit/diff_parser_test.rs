//! diff_parser 测试矩阵（§6.5，G-Diff）：
//! 完整/多 hunk/截断（>8 change 行）/新文件/rename/binary/无 newline/非法输入。

use super::*;
use crate::kit::tui_render_unit::{TuiDiffBlock, TuiHunkLineKind};

/// 标准单 hunk diff 文本（真实 Edit 输出形态）。
const BASIC_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,6 +10,7 @@ pub fn main() {
 fn main() {
-    let x = 1;
+    let x = 2;
     println!(\"{}\", x);
 }
";

/// 解析成功 + 结构正确（hunk 头 / 行号 / kind）。
#[test]
fn parse_basic_diff() {
    let block = parse_unified_diff(BASIC_DIFF, Some("src/main.rs")).expect("标准 diff 应解析成功");
    assert_eq!(block.path, "src/main.rs", "path_hint 优先");
    assert!(!block.is_binary);
    assert!(!block.is_too_large);
    assert!(!block.is_new_file);
    assert_eq!(block.more_change_lines, 0, "单 hunk 无剩余");
    assert_eq!(block.hunks.len(), 1);

    let hunk = &block.hunks[0];
    assert_eq!(hunk.old_range, "-10,6");
    assert_eq!(hunk.new_range, "+10,7");
    assert_eq!(hunk.truncated_lines, 0);

    let lines = &hunk.lines;
    assert_eq!(lines.len(), 5, "context + del + add + context + context");
    assert_eq!(lines[0].kind, TuiHunkLineKind::Context);
    assert_eq!(lines[0].text, "fn main() {");
    assert_eq!(lines[0].old_no, Some(10));
    assert_eq!(lines[0].new_no, Some(10));
    assert_eq!(lines[1].kind, TuiHunkLineKind::Del);
    assert_eq!(lines[1].text, "    let x = 1;");
    assert_eq!(lines[1].old_no, Some(11), "del 行只有旧行号");
    assert_eq!(lines[1].new_no, None);
    assert_eq!(lines[2].kind, TuiHunkLineKind::Add);
    assert_eq!(lines[2].text, "    let x = 2;");
    assert_eq!(lines[2].old_no, None, "add 行只有新行号");
    assert_eq!(lines[2].new_no, Some(11));
    assert_eq!(lines[3].kind, TuiHunkLineKind::Context);
    assert_eq!(lines[3].old_no, Some(12), "context 双行号继续递增");
    assert_eq!(lines[3].new_no, Some(12));
}

/// 无 path_hint 时从 `+++ b/…` 头提取路径（剥 b/ 前缀）。
#[test]
fn parse_path_from_diff_header() {
    let block = parse_unified_diff(BASIC_DIFF, None).expect("无 hint 时应从 +++ 头提取路径");
    assert_eq!(block.path, "src/main.rs", "+++ b/ 路径提取");
}

/// CRLF（Windows 生成）diff：逐行剥 `\r` 后再判定——残留 `\r` 会让空行落入
/// Context 兜底、路径/内容带上尾部 `\r`（review LOW：CRLF 统一 diff 误解析）。
#[test]
fn parse_crlf_diff() {
    let text = "--- a/src/main.rs\r\n+++ b/src/main.rs\r\n@@ -10,3 +10,3 @@\r\n fn main() {\r\n-    let x = 1;\r\n+    let x = 2;\r\n}\r\n";
    let block = parse_unified_diff(text, Some("src/main.rs")).expect("CRLF diff 应解析成功");
    assert_eq!(block.path, "src/main.rs", "CRLF 路径无尾部 \\r");
    assert_eq!(block.hunks.len(), 1);
    let hunk = &block.hunks[0];
    assert_eq!(hunk.lines.len(), 4, "context + del + add + context");
    assert_eq!(hunk.lines[0].kind, TuiHunkLineKind::Context);
    assert_eq!(
        hunk.lines[0].text, "fn main() {",
        "context 行剥前导空格且无尾部 \\r"
    );
    assert_eq!(hunk.lines[0].old_no, Some(10));
    assert_eq!(hunk.lines[1].kind, TuiHunkLineKind::Del);
    assert_eq!(hunk.lines[1].text, "    let x = 1;", "del 行无尾部 \\r");
    assert_eq!(hunk.lines[1].old_no, Some(11), "CRLF 下 del 行号仍递增");
    assert_eq!(hunk.lines[2].kind, TuiHunkLineKind::Add);
    assert_eq!(hunk.lines[2].text, "    let x = 2;", "add 行无尾部 \\r");
    assert_eq!(hunk.lines[2].new_no, Some(11), "CRLF 下 add 行号仍递增");
    assert_eq!(hunk.lines[3].kind, TuiHunkLineKind::Context);
    assert_eq!(hunk.lines[3].text, "}", "context 行无尾部 \\r");
}

/// 删除行内容以 `--` 开头（`---old`）仍是 Del，不误判为 `--- ` 头行
/// （review LOW：`count_change_lines` 曾跳过任何 `---` 开头的行，计数偏低）。
#[test]
fn parse_deleted_line_starting_with_dashdash() {
    let text = "\
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,1 @@
---old
+ new
";
    let block = parse_unified_diff(text, Some("x.txt")).expect("应解析成功");
    let hunk = &block.hunks[0];
    assert_eq!(
        hunk.lines[0].kind,
        TuiHunkLineKind::Del,
        "`---old` 是删除行"
    );
    assert_eq!(hunk.lines[0].text, "--old");
}

/// 多 hunk：全部解析（≤4），行号按 hunk 头重新锚定。
#[test]
fn parse_multi_hunk() {
    let text = "\
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
- a
+ b
@@ -5,1 +5,2 @@
 keep
+ added
";
    let block = parse_unified_diff(text, None).expect("多 hunk 应解析成功");
    assert_eq!(block.hunks.len(), 2, "两个 hunk 都保留");
    let first = &block.hunks[0];
    assert_eq!(first.old_range, "-1,2");
    assert_eq!(first.lines.len(), 2);
    assert_eq!(first.lines[0].old_no, Some(1), "首个 hunk 行号从 1 起");
    let second = &block.hunks[1];
    assert_eq!(second.old_range, "-5,1");
    assert_eq!(second.new_range, "+5,2");
    assert_eq!(second.lines[0].old_no, Some(5), "第二 hunk 行号重新锚定");
    assert_eq!(second.lines[0].new_no, Some(5));
    assert_eq!(second.lines[1].kind, TuiHunkLineKind::Add);
    assert_eq!(second.lines[1].new_no, Some(6));
    // 渲染只展示首个 hunk（§6.5）——第二 hunk 的 change 行（`+ added`）计数进
    // more_change_lines（`… +N more lines` 指示）。
    assert_eq!(block.more_change_lines, 1);
}

/// 截断：单 hunk 超过 8 个 change 行 → 前 8 行 + truncated 计数；
/// 渲染层据此显示 `… +N more lines`（§6.5）。
#[test]
fn parse_truncates_over_eight_change_lines() {
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,20 +1,20 @@\n");
    // 10 个 del 行 + 10 个 add 行（> 8 change）
    for i in 0..10 {
        text.push_str(&format!("- old {i}\n"));
    }
    for i in 0..10 {
        text.push_str(&format!("+ new {i}\n"));
    }
    let block = parse_unified_diff(&text, Some("x")).expect("超限 change 行应截断而非降级");
    let hunk = &block.hunks[0];
    let change_count = hunk
        .lines
        .iter()
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Add | TuiHunkLineKind::Del))
        .count();
    assert_eq!(
        change_count, MAX_CHANGE_LINES_PER_HUNK,
        "最多 8 个 change 行"
    );
    // 20 change 行 - 8 展示 = 12 截断
    assert_eq!(hunk.truncated_lines, 12, "截断数 = 剩余 change 行");
    assert_eq!(block.more_change_lines, 0);
}

/// 新文件（Write）：`new file mode` 头 → is_new_file，change 上限 6 行。
#[test]
fn parse_new_file_caps_at_six() {
    let text = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..abcdef0
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,12 @@
+line 1
+line 2
+line 3
+line 4
+line 5
+line 6
+line 7
+line 8
+line 9
+line 10
";
    let block = parse_unified_diff(text, Some("new.txt")).expect("新文件应解析成功");
    assert!(block.is_new_file, "new file mode 头判定");
    let hunk = &block.hunks[0];
    let change_count = hunk
        .lines
        .iter()
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Add | TuiHunkLineKind::Del))
        .count();
    assert_eq!(change_count, MAX_CHANGE_LINES_NEW_FILE, "新文件上限 6 行");
    assert_eq!(hunk.truncated_lines, 4, "10 add - 6 展示 = 4 截断");
    assert_eq!(hunk.lines[0].old_no, None, "新文件无旧行号");
    assert_eq!(hunk.lines[0].new_no, Some(1));
}

/// rename 头行跳过；`\ No newline at end of file` 跳过。
#[test]
fn parse_rename_and_no_newline_markers() {
    let text = "\
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1,2 +1,2 @@
 x
\\ No newline at end of file
- old
\\ No newline at end of file
+ new
\\ No newline at end of file
";
    let block = parse_unified_diff(text, None).expect("rename + 无 newline 标记应解析成功");
    assert_eq!(block.path, "new.txt", "diff --git 第二路径");
    assert!(!block.is_new_file, "rename 不是新文件");
    let hunk = &block.hunks[0];
    // `\ No newline` 标记行不产生 hunk 行
    assert_eq!(
        hunk.lines.len(),
        3,
        "context + del + add（无 newline 标记跳过）"
    );
}

/// binary diff → None（静默降级到 diff_change_summary 兜底）。
#[test]
fn parse_binary_returns_none() {
    assert_eq!(
        parse_unified_diff(
            "Binary files a/foo.png and b/foo.png differ\n",
            Some("foo.png")
        ),
        None,
        "Binary files 头 → 降级"
    );
    assert_eq!(
        parse_unified_diff("GIT binary patch\nliteral 42\n", Some("x.bin")),
        None,
        "GIT binary patch → 降级"
    );
}

/// 非法输入矩阵 → None（静默降级，不 panic）。
#[test]
fn parse_invalid_inputs_return_none() {
    // 空文本
    assert_eq!(parse_unified_diff("", Some("x")), None);
    assert_eq!(parse_unified_diff("   \n  ", Some("x")), None);
    // 无 hunk 头的普通文本（非 diff 输出）
    assert_eq!(
        parse_unified_diff("Wrote 3 lines to x.txt\n", Some("x")),
        None
    );
    assert_eq!(
        parse_unified_diff("test result: ok. 895 passed\n", None),
        None
    );
    // 非法 hunk 头
    assert_eq!(
        parse_unified_diff("@@ -x,y +a,b @@\n+ line\n", Some("x")),
        None,
        "非法 hunk 头行号 → 降级"
    );
    // 超长文本（> MAX_PARSE_LINES 行）
    let huge = (0..MAX_PARSE_LINES + 1)
        .map(|i| format!("- line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        parse_unified_diff(&huge, Some("x")),
        None,
        "超长 → 降级（防撑爆卡片）"
    );
}

/// 纯 add 无 context 的 hunk（Edit 替换常见形态）。
#[test]
fn parse_add_only_hunk() {
    let text = "--- a/x\n+++ b/x\n@@ -0,0 +1,1 @@\n+ brand new\n";
    let block = parse_unified_diff(text, None).expect("纯 add hunk 应解析成功");
    let hunk = &block.hunks[0];
    assert_eq!(hunk.lines.len(), 1);
    assert_eq!(hunk.lines[0].kind, TuiHunkLineKind::Add);
    assert_eq!(hunk.lines[0].new_no, Some(1));
}

/// 超过 MAX_HUNKS 的 hunk：前 4 个解析，其余 change 行计数进 more_change_lines。
#[test]
fn parse_more_than_max_hunks_counts_remaining() {
    let mut text = String::from("--- a/x\n+++ b/x\n");
    // 6 个 hunk，每个 1 change 行
    for i in 0..6 {
        text.push_str(&format!("@@ -{i},1 +{i},1 @@\n"));
        text.push_str(&format!("+ change {i}\n"));
    }
    let block = parse_unified_diff(&text, Some("x")).expect("多 hunk 应解析成功");
    assert_eq!(block.hunks.len(), MAX_HUNKS, "最多解析 MAX_HUNKS 个 hunk");
    // 未展示 hunk 的 change 计数：已解析 hunk 2-4（3 行）+ 超限 hunk 5-6（2 行）
    assert_eq!(block.more_change_lines, 5, "剩余 hunk 的 change 行计数");
}

/// TuiDiffBlock 完整字段矩阵（供 hash/render 消费的稳定摘要）。
#[test]
fn diff_block_fields_stable() {
    let block: Option<TuiDiffBlock> = parse_unified_diff(BASIC_DIFF, Some("src/main.rs"));
    let block = block.unwrap();
    assert_eq!(block.path, "src/main.rs");
    assert!(!block.is_binary);
    assert!(!block.is_too_large);
    assert!(!block.is_new_file);
    assert_eq!(block.more_change_lines, 0);
}

// ── [Slice 5] 摘要解析（parse_edit_write_summary）矩阵 ──────────────────
// 真实 Edit/Write 工具输出为摘要文本（无 unified diff）；摘要解析提取
// `+N`/`−M` 计数构造无 hunk 的 TuiDiffBlock（§6.4 口径，Slice 5 适配）。

/// Write 新文件：`Wrote N lines to P` → +N + is_new_file。
#[test]
fn summary_write_new_file() {
    let block = parse_edit_write_summary("Wrote 3 lines to src/new.rs", Some("/abs/src/new.rs"))
        .expect("Wrote 摘要应解析成功");
    assert_eq!(block.path, "/abs/src/new.rs", "path_hint 优先");
    assert!(block.is_new_file, "Write 即新文件");
    assert!(block.hunks.is_empty(), "摘要无 hunk 行");
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&block);
    assert_eq!((adds, dels), (3, 0));
}

/// Write 无 path_hint：从摘要文本提取路径。
#[test]
fn summary_path_fallback_from_text() {
    let block = parse_edit_write_summary("Wrote 1 line to src/new.rs", None)
        .expect("无 hint 时从文本提取路径");
    assert_eq!(block.path, "src/new.rs");
}

/// Write 无 `to` 分隔的真实输出（middleware `Wrote N lines {rel}` 形态）：
/// `Wrote 2 lines /path` → +2。
#[test]
fn summary_write_without_to_separator() {
    let block = parse_edit_write_summary("Wrote 2 lines /private/tmp/x.txt", None)
        .expect("Write 无 to 分隔也应解析成功");
    assert_eq!(block.path, "/private/tmp/x.txt");
    assert!(block.is_new_file);
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&block);
    assert_eq!((adds, dels), (2, 0));
}

/// Edit 加行：`Added N lines to P` → +N（非新文件）。
#[test]
fn summary_edit_added() {
    let block = parse_edit_write_summary("Added 2 lines to src/x.rs", None).unwrap();
    assert!(!block.is_new_file);
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&block);
    assert_eq!((adds, dels), (2, 0));
}

/// Edit 删行：`Removed N lines to P` → −N。
#[test]
fn summary_edit_removed() {
    let block = parse_edit_write_summary("Removed 4 lines to src/y.rs", None).unwrap();
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&block);
    assert_eq!((adds, dels), (0, 4));
}

/// 单数单位 `1 line` 与复数 `3 lines` 都接受。
#[test]
fn summary_singular_plural_units() {
    let one = parse_edit_write_summary("Added 1 line to a.rs", None).unwrap();
    let (adds, _) = crate::kit::tui_render_unit::diff_change_counts(&one);
    assert_eq!(adds, 1);
    let many = parse_edit_write_summary("Wrote 10 lines to b.rs", None).unwrap();
    let (adds, _) = crate::kit::tui_render_unit::diff_change_counts(&many);
    assert_eq!(adds, 10);
}

/// Edit 同行数替换（middleware 新形态）：`Replaced N lines to P` → +N −N
/// （被替换的 N 行既删又增，header 展示 `· +N · -N`）。
#[test]
fn summary_edit_replaced() {
    let block = parse_edit_write_summary("Replaced 1 line to src/x.rs", None).unwrap();
    assert!(!block.is_new_file);
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&block);
    assert_eq!((adds, dels), (1, 1));

    let multi = parse_edit_write_summary("Replaced 3 lines to src/x.rs", None).unwrap();
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&multi);
    assert_eq!((adds, dels), (3, 3));
}

/// 旧格式 "Replaced text (same line count)"（无计数）→ None：保持兼容降级
/// （回放老会话摘要仍不解析；回归防线）。
#[test]
fn summary_same_line_count_is_none() {
    assert_eq!(
        parse_edit_write_summary("Replaced text (same line count) to src/x.rs", None),
        None
    );
}

/// 宽松变体/非标准文本不解析（既有分组测试依赖：`Wrote 3 lines` 无 `to`）。
#[test]
fn summary_loose_variants_are_none() {
    assert_eq!(parse_edit_write_summary("Wrote 3 lines", None), None);
    assert_eq!(
        parse_edit_write_summary("Replaced text in src/x.rs", None),
        None
    );
    assert_eq!(parse_edit_write_summary("done", None), None);
    assert_eq!(parse_edit_write_summary("", None), None);
    assert_eq!(
        parse_edit_write_summary("Wrote 0 lines to x.rs", None),
        None,
        "0 行无信息"
    );
}

/// path_hint 为空字符串时回退文本路径。
#[test]
fn summary_empty_hint_falls_back() {
    let block = parse_edit_write_summary("Added 1 line to src/z.rs", Some("")).unwrap();
    assert_eq!(block.path, "src/z.rs");
}
