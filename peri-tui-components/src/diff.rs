//! 有上限的 unified diff 解析与展示模型。
//!
//! 调用方传入 [`DiffBudget`]；解析过程只在预算内保留结构，超限部分折叠为
//! 计数（`truncated_lines` / `more_change_lines`），非法、二进制或超限输入
//! 返回 `None`，由宿主回落到自己的摘要展示。
//!
//! 截断语义（§6.5）：
//! - 单 hunk 最多保留 `max_change_lines` 个 change（`+`/`-`）行，其余计数进
//!   [`DiffHunk::truncated_lines`]；
//! - 新文件（Write / 空 old_string 的 Edit）使用 `max_change_lines_new_file`；
//! - 首个 hunk 之后的全部 change 行计数进 [`DiffBlock::more_change_lines`]
//!   （渲染层只展示首个 hunk，其余显示 `… +N more lines`）；
//! - 每个 hunk 的总行数与 hunk 数也受预算限制。
//!
//! 解析只做一次纯转换，不持有缓存或全局状态。

/// 单 hunk 内最多保留的 change（`+`/`-`）行数。
pub const MAX_CHANGE_LINES_PER_HUNK: usize = 8;
/// 新文件（Write / 空 old_string 的 Edit）change 行上限。
pub const MAX_CHANGE_LINES_NEW_FILE: usize = 6;
/// 单 hunk 内总行数上限（含 context）。
pub const MAX_LINES_PER_HUNK: usize = 32;
/// 解析的总行数上限；超过视为超限并返回 `None`。
pub const MAX_PARSE_LINES: usize = 300;
/// 最多解析的 hunk 数（渲染只展示首个；后续 hunk 的 change 行计数进
/// `more_change_lines`，超出部分继续扫描计数但不建 hunk）。
pub const MAX_HUNKS: usize = 4;

/// unified diff 解析预算。默认值即上述保留上限，宿主可用更小的预算测试或
/// 收紧展示量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffBudget {
    pub max_lines: usize,
    pub max_hunks: usize,
    pub max_lines_per_hunk: usize,
    pub max_change_lines: usize,
    pub max_change_lines_new_file: usize,
}

impl Default for DiffBudget {
    fn default() -> Self {
        Self {
            max_lines: MAX_PARSE_LINES,
            max_hunks: MAX_HUNKS,
            max_lines_per_hunk: MAX_LINES_PER_HUNK,
            max_change_lines: MAX_CHANGE_LINES_PER_HUNK,
            max_change_lines_new_file: MAX_CHANGE_LINES_NEW_FILE,
        }
    }
}

/// Inline diff preview (for Write / Edit tool results).
#[derive(Debug, Clone, PartialEq)]
pub struct DiffBlock {
    /// File path the diff applies to.
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    /// Binary file -- cannot display diff.
    pub is_binary: bool,
    /// Diff content exceeded safe size limit.
    pub is_too_large: bool,
    /// New file (Write, or Edit with empty old_string) -- cap at 6 lines.
    pub is_new_file: bool,
    /// 首个 hunk 之后所有未展示 hunk 的 change（`+`/`-`）行总数——渲染层在
    /// 首个 hunk 后显示 `… +N more lines`。
    pub more_change_lines: usize,
    /// 顶层 `+`/`-` 总计数（header `+N −M` 渲染与宿主 hash 共用）。unified diff
    /// = 全部 hunk 内 change 行数；摘要解析（无 hunk）= 摘要提取的行数。
    pub adds: usize,
    pub dels: usize,
}

/// A single diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffHunk {
    /// Header range string for the old side (e.g. `-1,3`).
    pub old_range: String,
    /// Header range string for the new side.
    pub new_range: String,
    pub lines: Vec<DiffHunkLine>,
    /// 本 hunk 内超出上限被截断的 change 行数——渲染层追加 `… +N more lines`。
    pub truncated_lines: usize,
}

/// One line inside a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffHunkLine {
    pub kind: DiffHunkLineKind,
    /// Content text (without the leading +/- or space prefix).
    pub text: String,
    /// Line number on the old side (None for pure-add lines).
    pub old_no: Option<u32>,
    /// Line number on the new side (None for pure-delete lines).
    pub new_no: Option<u32>,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffHunkLineKind {
    /// Unchanged context line.
    Context,
    /// Added line.
    Add,
    /// Deleted line.
    Del,
}

/// 解析 unified diff 文本。`path_hint` 优先（Edit/Write 的 `file_path`），
/// 缺失时从 `+++ b/…` / `diff --git a/… b/…` 头提取。
pub fn parse_unified_diff(
    text: &str,
    path_hint: Option<&str>,
    budget: &DiffBudget,
) -> Option<DiffBlock> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 只数到超限为止：超长输出的其余部分不进入解析。
    if text.lines().take(budget.max_lines + 1).count() > budget.max_lines {
        return None;
    }

    let mut path: Option<String> = path_hint.map(str::to_string);
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut is_new_file = false;
    // 首个 hunk 之后的 change 行总数（含预算截断部分）。
    let mut more_change_lines: usize = 0;
    let mut cur_hunk: Option<DiffHunk> = None;
    // 当前 hunk 的 old/new 行号游标（由 hunk 头起始值初始化）。
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;
    let mut change_cap: usize = budget.max_change_lines;

    // 逐行迭代，rest 维护当前行之后的剩余文本（count_change_lines 需要从
    // hunk 超限点开始的全文——不能只按行长度偏移 text 起点）。
    // [注意] 空行时 line.len()==0 不推进——跳过换行符（+1，clamp 到末尾）。
    // [Fix CRLF] `str::lines()` 只剥 `\n` 不剥 `\r`：CRLF 输入每行尾残留 `\r`，
    // 会让空行落入 Context 兜底、路径/内容带上尾部 `\r`——按行剥除后再判定
    // （rest 推进仍用原始行长度，保证行边界正确）。
    let mut rest = text;
    while let Some(line_raw) = rest.lines().next() {
        let skip = (line_raw.len() + 1).min(rest.len());
        rest = &rest[skip..];
        let line = line_raw.strip_suffix('\r').unwrap_or(line_raw);

        // 头扫描阶段（@@ 之前）——路径 / binary / new file 判定。
        if hunks.is_empty() && cur_hunk.is_none() && !line.starts_with("@@") {
            if line.contains("Binary files") || line.contains("GIT binary patch") {
                // 二进制 diff 无法展示 → 静默降级。
                return None;
            }
            if line.starts_with("new file mode") {
                is_new_file = true;
            }
            if line.starts_with("--- ") && line[4..].starts_with("/dev/null") {
                is_new_file = true;
            }
            if let Some(p) = extract_path_from_line(line)
                && path.is_none()
            {
                path = Some(p);
            }
            continue;
        }

        // ── hunk 头 ──
        if line.starts_with("@@") {
            // 收尾上一个 hunk。
            if let Some(h) = cur_hunk.take() {
                hunks.push(h);
            }
            // 超出 hunk 预算的部分不再建结构——仅计数其 change 行。
            if hunks.len() >= budget.max_hunks {
                more_change_lines += count_change_lines(rest);
                break;
            }
            let Some((o, n)) = parse_hunk_header(line) else {
                // 非法 hunk 头 → 整块降级（无有效结构可展示）。
                return None;
            };
            old_no = o;
            new_no = n;
            let (old_range, new_range) = split_hunk_ranges(line);
            cur_hunk = Some(DiffHunk {
                old_range: old_range.to_string(),
                new_range: new_range.to_string(),
                lines: Vec::new(),
                truncated_lines: 0,
            });
            change_cap = if is_new_file {
                budget.max_change_lines_new_file
            } else {
                budget.max_change_lines
            };
            continue;
        }

        // ── hunk 行 ──
        let Some(h) = cur_hunk.as_mut() else {
            // 无 hunk 头就出现的内容行（如纯 "Wrote N lines" 文本）→ 非 diff。
            return None;
        };
        // `\ No newline at end of file` 标记行——跳过（不参与行号计数）。
        if line.starts_with("\\ ") || line.is_empty() {
            continue;
        }
        let (kind, content) = match line.as_bytes().first() {
            Some(b'+') => (DiffHunkLineKind::Add, &line[1..]),
            Some(b'-') => (DiffHunkLineKind::Del, &line[1..]),
            Some(b' ') => (DiffHunkLineKind::Context, &line[1..]),
            // 制表符/其他前缀（罕见）——按 context 处理保底。
            _ => (DiffHunkLineKind::Context, line),
        };
        let is_change = matches!(kind, DiffHunkLineKind::Add | DiffHunkLineKind::Del);
        if is_change {
            let change_so_far = h
                .lines
                .iter()
                .filter(|l| matches!(l.kind, DiffHunkLineKind::Add | DiffHunkLineKind::Del))
                .count();
            if change_so_far >= change_cap || h.lines.len() >= budget.max_lines_per_hunk {
                // 超上限：hunk 停止收集（含后续 context——截断点之后的行不展示）；
                // 剩余 change 行计数进 truncated。
                h.truncated_lines += 1;
                continue;
            }
        } else if h.lines.len() >= budget.max_lines_per_hunk {
            // context 行超总行数上限——停止收集（change 计数已完成，不再增加）。
            continue;
        }
        let (old_line_no, new_line_no) = match kind {
            DiffHunkLineKind::Add => (None, Some(new_no)),
            DiffHunkLineKind::Del => (Some(old_no), None),
            DiffHunkLineKind::Context => (Some(old_no), Some(new_no)),
        };
        if matches!(kind, DiffHunkLineKind::Add) {
            new_no += 1;
        } else if matches!(kind, DiffHunkLineKind::Del) {
            old_no += 1;
        } else {
            old_no += 1;
            new_no += 1;
        }
        h.lines.push(DiffHunkLine {
            kind,
            text: content.to_string(),
            old_no: old_line_no,
            new_no: new_line_no,
        });
    }

    if let Some(h) = cur_hunk.take() {
        hunks.push(h);
    }
    // 没有解析到任何 hunk → 非法输入降级。
    if hunks.is_empty() {
        return None;
    }
    // 首个 hunk 之后的 change 行总数（含截断部分）。
    more_change_lines += hunks[1..]
        .iter()
        .map(|h| {
            h.lines
                .iter()
                .filter(|l| matches!(l.kind, DiffHunkLineKind::Add | DiffHunkLineKind::Del))
                .count()
                + h.truncated_lines
        })
        .sum::<usize>();

    // 顶层 `+`/`−` 总计数 = 全部 hunk 内 change 行数（含截断部分——
    // 截断行也是真实变更，header 计数不应少于实际）。
    let (adds, dels) = hunks.iter().fold((0usize, 0usize), |(a, d), h| {
        let add = h
            .lines
            .iter()
            .filter(|l| matches!(l.kind, DiffHunkLineKind::Add))
            .count();
        let del = h
            .lines
            .iter()
            .filter(|l| matches!(l.kind, DiffHunkLineKind::Del))
            .count();
        (a + add, d + del)
    });

    Some(DiffBlock {
        path: path.unwrap_or_default(),
        hunks,
        is_binary: false,
        is_too_large: false,
        is_new_file,
        more_change_lines,
        adds,
        dels,
    })
}

/// 解析 Edit/Write 工具的真实摘要输出：
///
/// ```text
/// Wrote 3 lines to src/main.rs      → +3（新文件，is_new_file）
/// Wrote 3 lines /tmp/x              → +3（Write 实际无 `to` 分隔）
/// Added 1 line to src/x.rs          → +1
/// Removed 2 lines to src/y.rs       → −2
/// Replaced 1 line to src/x.rs       → +1 −1（同行数内容替换）
/// Replaced text (same line count)   → None（旧格式无计数，兼容降级）
/// ```
///
/// 返回无 hunk 的 [`DiffBlock`]（渲染层仅展示 header `path +N −M`）。
/// 要求行数单位与路径齐备（无路径/宽松变体不解析，避免误伤非标准文本）。
/// `path_hint` 优先（`raw_input.file_path` 绝对路径），缺失时回退摘要文本路径。
pub fn parse_edit_write_summary(text: &str, path_hint: Option<&str>) -> Option<DiffBlock> {
    let trimmed = text.trim();
    let (kind, rest) = if let Some(r) = trimmed.strip_prefix("Wrote ") {
        (SummaryKind::Wrote, r)
    } else if let Some(r) = trimmed.strip_prefix("Added ") {
        (SummaryKind::Added, r)
    } else if let Some(r) = trimmed.strip_prefix("Removed ") {
        (SummaryKind::Removed, r)
    } else {
        // 旧格式 "Replaced text (same line count)" 及其余文本——无计数信息。
        (SummaryKind::Replaced, trimmed.strip_prefix("Replaced ")?)
    };
    // 行数数字（`1 line` / `3 lines`）。
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let count: usize = digits.parse().ok()?;
    if count == 0 {
        return None;
    }
    let after = rest[digits.len()..].trim_start();
    // 先匹配更长形式 `lines`（"lines" 也以 "line" 开头，顺序不可反）。
    let after = after
        .strip_prefix("lines")
        .or_else(|| after.strip_prefix("line"))?;
    let after = after.trim_start();
    // Edit 输出含 ` to ` 分隔（"Added 2 lines to P"）；Write 输出无 `to`
    // （"Wrote 2 lines /path"）。`to` 可选；两侧均需存在路径文本。
    let after = after.strip_prefix("to").map(str::trim).unwrap_or(after);
    let path = after.trim().to_string();
    if path.is_empty() {
        // 无路径信息（如 "Wrote 3 lines"）——不构造块，保持可合并。
        return None;
    }
    let path = path_hint
        .map(str::to_string)
        .filter(|p| !p.is_empty())
        .unwrap_or(path);
    let (adds, dels) = match kind {
        SummaryKind::Wrote | SummaryKind::Added => (count, 0),
        SummaryKind::Removed => (0, count),
        // 同行数替换：被替换的 N 行既删又增（`· +N · -N`）。
        SummaryKind::Replaced => (count, count),
    };
    Some(DiffBlock {
        path,
        hunks: Vec::new(),
        is_binary: false,
        is_too_large: false,
        // Write 即新文件写入（输出恒为 `Wrote N lines` 形态）。
        is_new_file: kind == SummaryKind::Wrote,
        more_change_lines: 0,
        adds,
        dels,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryKind {
    Wrote,
    Added,
    Removed,
    Replaced,
}

/// 从 `+++ b/xxx` / `diff --git a/xxx b/xxx` 头提取路径（剥 `a/`/`b/` 前缀）。
fn extract_path_from_line(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("+++ ") {
        return Some(strip_ab_prefix(rest));
    }
    if let Some(rest) = line.strip_prefix("diff --git ") {
        // `a/x b/x` 形式——取第二个（新文件侧）。
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() >= 2 {
            return Some(strip_ab_prefix(parts[1]));
        }
        return parts.first().map(|p| strip_ab_prefix(p));
    }
    None
}

#[must_use]
fn strip_ab_prefix(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("a/") {
        rest.to_string()
    } else if let Some(rest) = p.strip_prefix("b/") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

/// 解析 `@@ -l[,count] +l[,count] @@` → (old_start, new_start)。
/// 两侧 range 分别解析（`-0,0` 的 count 不能与 `+1,1` 的 start 混淆）。
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@")?;
    let rest = rest.split("@@").next()?;
    let mut ranges = rest.split_whitespace();
    let old_start = ranges
        .next()?
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()?
        .parse::<u32>()
        .ok()?;
    let new_start = ranges
        .next()?
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()?
        .parse::<u32>()
        .ok()?;
    Some((old_start, new_start))
}

/// 提取 hunk 头的两侧 range 原文（如 `-1,3` / `+1,4`）。
fn split_hunk_ranges(line: &str) -> (&str, &str) {
    let inner = line
        .strip_prefix("@@")
        .and_then(|s| s.split("@@").next())
        .unwrap_or("");
    let mut it = inner.split_whitespace();
    let old = it.next().unwrap_or("-0");
    let new = it.next().unwrap_or("+0");
    (old, new)
}

/// 统计文本中 change（`+`/`-` 开头）行数（跳过 `--- ` / `+++ ` / `@@` 头行）。
/// 头行判定要求 `---`/`+++` 后带空格（git 头格式 `--- a/…`）——否则删除行内容
/// 以 `--` 开头（行文本 `---foo`）会被误跳过、计数偏低。
fn count_change_lines(text: &str) -> usize {
    text.lines()
        .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@"))
        .filter(|l| l.starts_with('+') || l.starts_with('-'))
        .count()
}

#[cfg(test)]
#[path = "diff_test.rs"]
mod tests;
