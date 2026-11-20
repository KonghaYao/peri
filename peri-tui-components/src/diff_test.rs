//! 有预算的 unified diff 解析矩阵：完整/多 hunk/截断/新文件/二进制/CRLF/摘要回落。

use super::MAX_CHANGE_LINES_NEW_FILE;
use super::MAX_CHANGE_LINES_PER_HUNK;
use super::MAX_HUNKS;
use super::MAX_PARSE_LINES;
use super::{DiffBudget, DiffHunkLineKind};

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

fn default_parse(text: &str, path_hint: Option<&str>) -> Option<super::DiffBlock> {
    super::parse_unified_diff(text, path_hint, &DiffBudget::default())
}

/// 解析成功 + 结构正确（hunk 头 / 行号 / kind）。
#[test]
fn parse_basic_diff() {
    let block = default_parse(BASIC_DIFF, Some("src/main.rs")).expect("标准 diff 应解析成功");
    assert_eq!(block.path, "src/main.rs", "path_hint 优先");
    assert!(!block.is_binary);
    assert!(!block.is_too_large);
    assert!(!block.is_new_file);
    assert_eq!(block.more_change_lines, 0, "单 hunk 无剩余");
    assert_eq!(block.adds, 1);
    assert_eq!(block.dels, 1);
    assert_eq!(block.hunks.len(), 1);

    let hunk = &block.hunks[0];
    assert_eq!(hunk.old_range, "-10,6");
    assert_eq!(hunk.new_range, "+10,7");
    assert_eq!(hunk.truncated_lines, 0);

    let lines = &hunk.lines;
    assert_eq!(lines.len(), 5, "context + del + add + context + context");
    assert_eq!(lines[0].kind, DiffHunkLineKind::Context);
    assert_eq!(lines[0].text, "fn main() {");
    assert_eq!(lines[0].old_no, Some(10));
    assert_eq!(lines[0].new_no, Some(10));
    assert_eq!(lines[1].kind, DiffHunkLineKind::Del);
    assert_eq!(lines[1].text, "    let x = 1;");
    assert_eq!(lines[1].old_no, Some(11), "del 行只有旧行号");
    assert_eq!(lines[1].new_no, None);
    assert_eq!(lines[2].kind, DiffHunkLineKind::Add);
    assert_eq!(lines[2].text, "    let x = 2;");
    assert_eq!(lines[2].old_no, None, "add 行只有新行号");
    assert_eq!(lines[2].new_no, Some(11));
    assert_eq!(lines[3].kind, DiffHunkLineKind::Context);
    assert_eq!(lines[3].old_no, Some(12), "context 双行号继续递增");
    assert_eq!(lines[3].new_no, Some(12));
}

/// 无 path_hint 时从 `+++ b/…` 头提取路径（剥 b/ 前缀）。
#[test]
fn parse_path_from_diff_header() {
    let block = default_parse(BASIC_DIFF, None).expect("无 hint 时应从 +++ 头提取路径");
    assert_eq!(block.path, "src/main.rs", "+++ b/ 路径提取");
}

/// CRLF（Windows 生成）diff：逐行剥 `\r` 后再判定——残留 `\r` 会让空行落入
/// Context 兜底、路径/内容带上尾部 `\r`。
#[test]
fn parse_crlf_diff() {
    let text = "--- a/src/main.rs\r\n+++ b/src/main.rs\r\n@@ -10,3 +10,3 @@\r\n fn main() {\r\n-    let x = 1;\r\n+    let x = 2;\r\n}\r\n";
    let block = default_parse(text, Some("src/main.rs")).expect("CRLF diff 应解析成功");
    assert_eq!(block.path, "src/main.rs", "CRLF 路径无尾部 \\r");
    assert_eq!(block.hunks.len(), 1);
    let hunk = &block.hunks[0];
    assert_eq!(hunk.lines.len(), 4, "context + del + add + context");
    assert_eq!(hunk.lines[0].kind, DiffHunkLineKind::Context);
    assert_eq!(
        hunk.lines[0].text, "fn main() {",
        "context 行剥前导空格且无尾部 \\r"
    );
    assert_eq!(hunk.lines[0].old_no, Some(10));
    assert_eq!(hunk.lines[1].kind, DiffHunkLineKind::Del);
    assert_eq!(hunk.lines[1].text, "    let x = 1;", "del 行无尾部 \\r");
    assert_eq!(hunk.lines[1].old_no, Some(11), "CRLF 下 del 行号仍递增");
    assert_eq!(hunk.lines[2].kind, DiffHunkLineKind::Add);
    assert_eq!(hunk.lines[2].text, "    let x = 2;", "add 行无尾部 \\r");
    assert_eq!(hunk.lines[2].new_no, Some(11), "CRLF 下 add 行号仍递增");
    assert_eq!(hunk.lines[3].kind, DiffHunkLineKind::Context);
    assert_eq!(hunk.lines[3].text, "}", "context 行无尾部 \\r");
}

/// 删除行内容以 `--` 开头（`---old`）仍是 Del，不误判为 `--- ` 头行。
#[test]
fn parse_deleted_line_starting_with_dashdash() {
    let text = "\
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,1 @@
---old
+ new
";
    let block = default_parse(text, Some("x.txt")).expect("应解析成功");
    let hunk = &block.hunks[0];
    assert_eq!(
        hunk.lines[0].kind,
        DiffHunkLineKind::Del,
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
    let block = default_parse(text, None).expect("多 hunk 应解析成功");
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
    assert_eq!(second.lines[1].kind, DiffHunkLineKind::Add);
    assert_eq!(second.lines[1].new_no, Some(6));
    // 渲染只展示首个 hunk——第二 hunk 的 change 行计数进 more_change_lines。
    assert_eq!(block.more_change_lines, 1);
}

/// 截断：单 hunk 超过 8 个 change 行 → 前 8 行 + truncated 计数。
#[test]
fn parse_truncates_over_eight_change_lines() {
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,20 +1,20 @@\n");
    for i in 0..10 {
        text.push_str(&format!("- old {i}\n"));
    }
    for i in 0..10 {
        text.push_str(&format!("+ new {i}\n"));
    }
    let block = default_parse(&text, Some("x")).expect("超限 change 行应截断而非降级");
    let hunk = &block.hunks[0];
    let change_count = hunk
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffHunkLineKind::Add | DiffHunkLineKind::Del))
        .count();
    assert_eq!(
        change_count, MAX_CHANGE_LINES_PER_HUNK,
        "最多 8 个 change 行"
    );
    assert_eq!(hunk.truncated_lines, 12, "截断数 = 剩余 change 行");
    assert_eq!(block.more_change_lines, 0);
    assert_eq!(
        (block.adds, block.dels),
        (10, 10),
        "顶层计数含被截断的 change 行"
    );
}

/// 截断点之后的 context 不再保留（避免携带错误行号）。
#[test]
fn parse_truncation_drops_following_context() {
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,10 +1,10 @@\n");
    for i in 0..9 {
        text.push_str(&format!("+ new {i}\n"));
    }
    text.push_str(" context after truncation\n");
    let block = default_parse(&text, Some("x")).expect("超限 change 行应截断而非降级");
    let hunk = &block.hunks[0];
    assert_eq!(
        hunk.lines.len(),
        MAX_CHANGE_LINES_PER_HUNK,
        "只保留预算内行"
    );
    assert_eq!(hunk.truncated_lines, 1, "第 9 个 change 行被截断");
    assert!(
        hunk.lines.iter().all(|l| l.kind == DiffHunkLineKind::Add),
        "截断点之后的 context 不保留"
    );
    assert_eq!(block.adds, 9, "被截断的 change 仍计入顶层总数");
}

/// 超出 hunk 预算的 `--- ` / `+++ ` 行是 hunk 内容，不能当文件头跳过。
#[test]
fn parse_overflow_hunk_counts_header_like_changes() {
    let mut text = String::from("--- a/x\n+++ b/x\n");
    for i in 0..4 {
        text.push_str(&format!("@@ -{i},1 +{i},1 @@\n"));
        text.push_str("-old\n");
    }
    text.push_str("@@ -5,1 +5,1 @@\n--- old\n+++ new\n");
    let block = default_parse(&text, Some("x")).expect("多 hunk 应解析成功");
    assert_eq!(block.hunks.len(), MAX_HUNKS);
    assert_eq!(
        block.more_change_lines, 5,
        "第二至第四个 hunk 各 1 行 + 溢出 hunk 2 行"
    );
    assert_eq!((block.adds, block.dels), (1, 5), "顶层计数含溢出 hunk");
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
@@ -0,0 +1,10 @@
+one
+two
+three
+four
+five
+six
+seven
+eight
+nine
+ten
";
    let block = default_parse(text, None).expect("新文件 diff 应解析成功");
    assert!(block.is_new_file);
    let hunk = &block.hunks[0];
    let changes = hunk
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffHunkLineKind::Add))
        .count();
    assert_eq!(changes, MAX_CHANGE_LINES_NEW_FILE, "新文件最多 6 行");
    assert_eq!(hunk.truncated_lines, 4);
}

/// rename + `\ No newline at end of file` 标记行：路径取新侧，标记不计数。
#[test]
fn parse_rename_and_no_newline_markers() {
    let text = "\
diff --git a/old.txt b/new.txt
similarity index 90%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1,1 +1,1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
    let block = default_parse(text, None).expect("rename diff 应解析成功");
    assert_eq!(block.path, "new.txt");
    assert_eq!(block.hunks[0].lines.len(), 2);
    assert_eq!(block.hunks[0].truncated_lines, 0);
}

/// 二进制 diff 无法展示 → 静默降级。
#[test]
fn parse_binary_returns_none() {
    assert_eq!(
        default_parse(
            "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n",
            Some("x.png")
        ),
        None
    );
    assert_eq!(default_parse("GIT binary patch\nliteral 123\n", None), None);
}

/// 非法/空/非 diff 输入降级。
#[test]
fn parse_invalid_inputs_return_none() {
    for text in [
        "",
        "   ",
        "just some text",
        "@@ malformed @@\n+x\n",
        "Wrote 3 lines to src/x.rs",
    ] {
        assert_eq!(default_parse(text, None), None, "input: {text:?}");
    }
}

/// 纯 add hunk（old range 0,0）。
#[test]
fn parse_add_only_hunk() {
    let text = "--- a/x\n+++ b/x\n@@ -0,0 +1,2 @@\n+a\n+b\n";
    let block = default_parse(text, None).expect("纯新增 hunk 应解析成功");
    assert!(!block.is_new_file);
    let hunk = &block.hunks[0];
    assert_eq!(hunk.old_range, "-0,0");
    assert_eq!(hunk.new_range, "+1,2");
    assert_eq!(hunk.lines[0].old_no, None);
    assert_eq!(hunk.lines[0].new_no, Some(1));
    assert_eq!(hunk.lines[1].new_no, Some(2));
}

/// 超过 MAX_HUNKS：前 4 个解析，其余 change 行计数进 more_change_lines。
#[test]
fn parse_more_than_max_hunks_counts_remaining() {
    let mut text = String::from("--- a/x\n+++ b/x\n");
    for i in 0..6 {
        text.push_str(&format!("@@ -{i},1 +{i},1 @@\n"));
        text.push_str(&format!("+ change {i}\n"));
    }
    let block = default_parse(&text, Some("x")).expect("多 hunk 应解析成功");
    assert_eq!(block.hunks.len(), MAX_HUNKS, "最多解析 MAX_HUNKS 个 hunk");
    assert_eq!(block.more_change_lines, 5, "剩余 hunk 的 change 行计数");
    assert_eq!(block.adds, 6, "顶层计数覆盖未建结构的 hunk");
}

/// 总行数超限 → 静默降级（不构造卡片）。
#[test]
fn parse_over_line_budget_returns_none() {
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n");
    for _ in 0..MAX_PARSE_LINES {
        text.push_str(" context\n");
    }
    assert!(text.lines().count() > MAX_PARSE_LINES);
    assert_eq!(default_parse(&text, Some("x")), None);
}

/// 调用方预算真正生效：限制 hunk 数、每 hunk 行数与 change 行数。
#[test]
fn caller_budget_limits_retention() {
    let text = "\
--- a/x
+++ b/x
@@ -1,4 +1,4 @@
 ctx
-old
+new
 ctx
@@ -10,1 +10,2 @@
+extra
";
    let budget = DiffBudget {
        max_lines: 300,
        max_hunks: 1,
        max_lines_per_hunk: 2,
        max_change_lines: 1,
        max_change_lines_new_file: 1,
    };
    let block =
        super::parse_unified_diff(text, None, &budget).expect("自定义预算下仍应解析首个 hunk");
    assert_eq!(block.hunks.len(), 1, "预算只允许 1 个 hunk");
    assert_eq!(block.hunks[0].lines.len(), 2, "每 hunk 最多 2 行");
    assert_eq!(block.hunks[0].truncated_lines, 1, "首个 change 后截断");
    assert_eq!(block.more_change_lines, 1, "第二 hunk 的 change 计数保留");
    assert_eq!(
        (block.adds, block.dels),
        (2, 1),
        "保留上限不减少顶层 change 总数"
    );
}

/// 预算把总行数压到低于输入时，直接回落 None（调用方可用更小预算禁用大卡片）。
#[test]
fn caller_budget_rejects_input_over_line_cap() {
    let budget = DiffBudget {
        max_lines: 2,
        ..DiffBudget::default()
    };
    assert_eq!(super::parse_unified_diff(BASIC_DIFF, None, &budget), None);
}

// ── 摘要解析（parse_edit_write_summary）矩阵 ─────────────────────────────
// 真实 Edit/Write 工具输出为摘要文本（无 unified diff）；摘要解析提取
// `+N`/`−M` 计数构造无 hunk 的 DiffBlock。

/// Write 新文件：`Wrote N lines to P` → +N + is_new_file。
#[test]
fn summary_write_new_file() {
    let block =
        super::parse_edit_write_summary("Wrote 3 lines to src/new.rs", Some("/abs/src/new.rs"))
            .expect("Wrote 摘要应解析成功");
    assert_eq!(block.path, "/abs/src/new.rs", "path_hint 优先");
    assert!(block.is_new_file, "Write 即新文件");
    assert!(block.hunks.is_empty(), "摘要无 hunk 行");
    assert_eq!((block.adds, block.dels), (3, 0));
}

/// Write 无 path_hint：从摘要文本提取路径。
#[test]
fn summary_path_fallback_from_text() {
    let block = super::parse_edit_write_summary("Wrote 1 line to src/new.rs", None)
        .expect("无 hint 时从文本提取路径");
    assert_eq!(block.path, "src/new.rs");
}

/// Write 无 `to` 分隔的真实输出（middleware `Wrote N lines {rel}` 形态）。
#[test]
fn summary_write_without_to_separator() {
    let block = super::parse_edit_write_summary("Wrote 2 lines /private/tmp/x.txt", None)
        .expect("Write 无 to 分隔也应解析成功");
    assert_eq!(block.path, "/private/tmp/x.txt");
    assert!(block.is_new_file);
    assert_eq!((block.adds, block.dels), (2, 0));
}

/// 无 `to` 且路径以 `to` 开头时不能被当成分隔符吃掉。
#[test]
fn summary_write_path_starting_with_to_is_preserved() {
    let block = super::parse_edit_write_summary("Wrote 2 lines tools/x.rs", None)
        .expect("Write 无 to 分隔也应解析成功");
    assert_eq!(block.path, "tools/x.rs");
}

/// Edit 加行：`Added N lines to P` → +N（非新文件）。
#[test]
fn summary_edit_added() {
    let block = super::parse_edit_write_summary("Added 2 lines to src/x.rs", None)
        .expect("Added 摘要应解析");
    assert!(!block.is_new_file);
    assert_eq!((block.adds, block.dels), (2, 0));
}

/// Edit 删行：`Removed N lines to P` → −N。
#[test]
fn summary_edit_removed() {
    let block = super::parse_edit_write_summary("Removed 4 lines to src/y.rs", None)
        .expect("Removed 摘要应解析");
    assert_eq!((block.adds, block.dels), (0, 4));
}

/// 单数单位 `1 line` 与复数 `3 lines` 都接受。
#[test]
fn summary_singular_plural_units() {
    let one =
        super::parse_edit_write_summary("Added 1 line to a.rs", None).expect("单数摘要应解析");
    assert_eq!(one.adds, 1);
    let many =
        super::parse_edit_write_summary("Wrote 10 lines to b.rs", None).expect("复数摘要应解析");
    assert_eq!(many.adds, 10);
}

/// Edit 同行数替换：`Replaced N lines to P` → +N −N。
#[test]
fn summary_edit_replaced() {
    let block = super::parse_edit_write_summary("Replaced 1 line to src/x.rs", None)
        .expect("Replaced 摘要应解析");
    assert!(!block.is_new_file);
    assert_eq!((block.adds, block.dels), (1, 1));

    let multi = super::parse_edit_write_summary("Replaced 3 lines to src/x.rs", None)
        .expect("Replaced 多行摘要应解析");
    assert_eq!((multi.adds, multi.dels), (3, 3));
}

/// 旧格式 "Replaced text (same line count)"（无计数）→ None。
#[test]
fn summary_same_line_count_is_none() {
    assert_eq!(
        super::parse_edit_write_summary("Replaced text (same line count) to src/x.rs", None),
        None
    );
}

/// 宽松变体/非标准文本不解析。
#[test]
fn summary_loose_variants_are_none() {
    assert_eq!(super::parse_edit_write_summary("Wrote 3 lines", None), None);
    assert_eq!(
        super::parse_edit_write_summary("Replaced text in src/x.rs", None),
        None
    );
    assert_eq!(super::parse_edit_write_summary("done", None), None);
    assert_eq!(super::parse_edit_write_summary("", None), None);
    assert_eq!(
        super::parse_edit_write_summary("Wrote 0 lines to x.rs", None),
        None,
        "0 行无信息"
    );
}

/// path_hint 为空字符串时回退文本路径。
#[test]
fn summary_empty_hint_falls_back() {
    let block = super::parse_edit_write_summary("Added 1 line to src/z.rs", Some(""))
        .expect("空 hint 应回退文本路径");
    assert_eq!(block.path, "src/z.rs");
}
