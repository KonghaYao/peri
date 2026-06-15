use std::{
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    sync::{LazyLock, Mutex},
};

use lru::LruCache;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::{compute_diff, DiffInput, DiffLine, DiffWordType};
use crate::theme::Theme;

/// 渲染缓存���量
const RENDER_CACHE_CAPACITY: usize = 64;

/// 全局渲染结果缓存（单例）
static RENDER_CACHE: LazyLock<Mutex<LruCache<RenderCacheKey, Vec<Line<'static>>>>> =
    LazyLock::new(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(RENDER_CACHE_CAPACITY).unwrap(),
        ))
    });

/// 渲染缓存 key
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RenderCacheKey {
    old_hash: u64,
    new_hash: u64,
    flags: u8,
    width: usize,
}

impl RenderCacheKey {
    fn from_input(input: &DiffInput, width: usize) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        input.old_content.hash(&mut hasher);
        let old_hash = hasher.finish();

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        input.new_content.hash(&mut hasher);
        let new_hash = hasher.finish();

        let mut flags = 0u8;
        if input.is_new_file {
            flags |= 1;
        }
        if input.is_deleted_file {
            flags |= 2;
        }
        if input.is_binary {
            flags |= 4;
        }

        RenderCacheKey {
            old_hash,
            new_hash,
            flags,
            width,
        }
    }
}

/// Unicode-safe truncation：按显示宽度截断，不会在多字节字符中间切割
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut end = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_width {
            break;
        }
        width += cw;
        end = i + ch.len_utf8();
    }
    s[..end].to_string()
}

/// 计算 max_line_number 的十进制位数（至少 1）
fn digit_count(max_num: u32) -> usize {
    if max_num == 0 {
        return 1;
    }
    let mut n = max_num;
    let mut digits = 0;
    while n > 0 {
        n /= 10;
        digits += 1;
    }
    digits
}

/// 主渲染入口（带缓存）
pub fn render_diff_impl(input: &DiffInput, width: usize, theme: &dyn Theme) -> Vec<Line<'static>> {
    let key = RenderCacheKey::from_input(input, width);

    // 查缓存
    if let Ok(mut cache) = RENDER_CACHE.lock() {
        if let Some(cached) = cache.get(&key) {
            return cached.clone();
        }
    }

    // 未命中，执行渲染
    let lines = render_diff_uncached(input, width, theme);

    // 写入缓存
    if let Ok(mut cache) = RENDER_CACHE.lock() {
        cache.put(key, lines.clone());
    }

    lines
}

/// 实际渲染逻辑（无缓存）
fn render_diff_uncached(input: &DiffInput, width: usize, theme: &dyn Theme) -> Vec<Line<'static>> {
    let result = compute_diff(input);

    // 特殊情况：二进制文件
    if result.is_binary {
        return vec![Line::from(vec![Span::styled(
            format!("  Binary file {} - cannot display diff", input.file_path),
            Style::default().fg(theme.dim()),
        )])];
    }

    // 特殊情况：截断
    if result.is_truncated {
        return vec![Line::from(vec![Span::styled(
            format!(
                "  Diff too large for {} - changes not displayed",
                input.file_path
            ),
            Style::default().fg(theme.dim()),
        )])];
    }

    // 空结果 + 非新建/删除 → 返回空
    if result.hunks.is_empty() && !result.is_new_file && !result.is_deleted_file {
        return Vec::new();
    }

    let mut lines = Vec::new();

    // 标题行
    let (prefix, fg, bg) = if result.is_new_file {
        ("+", theme.diff_add(), Some(theme.diff_add_bg()))
    } else if result.is_deleted_file {
        ("-", theme.diff_remove(), Some(theme.diff_remove_bg()))
    } else {
        (" ", theme.muted(), None)
    };
    let title_style = match bg {
        Some(bg) => Style::default().fg(fg).bg(bg),
        None => Style::default().fg(fg),
    };
    lines.push(Line::from(vec![Span::styled(
        format!("{} {}", prefix, input.file_path),
        title_style,
    )]));

    // 计算最大行号
    let mut max_line_num: u32 = 0;
    for hunk in &result.hunks {
        for dl in &hunk.lines {
            match dl {
                DiffLine::Context {
                    old_line_num,
                    new_line_num,
                    ..
                } => {
                    max_line_num = max_line_num.max(*old_line_num).max(*new_line_num);
                }
                DiffLine::Add { line_num, .. } | DiffLine::Remove { line_num, .. } => {
                    max_line_num = max_line_num.max(*line_num);
                }
                DiffLine::HunkHeader { .. } => {}
            }
        }
    }

    let n = digit_count(max_line_num).max(1);
    // gutter_width = 1(marker) + N(old_num) + 1(space) + N(new_num) + 3(" │ ")
    let gutter_width = 1 + n + 1 + n + 3;

    for hunk in &result.hunks {
        // 新文件场景：限制最多显示 6 行内容（不含 hunk header 和省略提示）
        const NEW_FILE_MAX_LINES: usize = 6;
        let new_file_limit = result.is_new_file.then_some(NEW_FILE_MAX_LINES);

        // 计算本 hunk 内所有内容行的公共前导空格数，然后裁剪
        let common_indent = hunk
            .lines
            .iter()
            .filter_map(|dl| match dl {
                DiffLine::Context { text, .. }
                | DiffLine::Add { text, .. }
                | DiffLine::Remove { text, .. } => {
                    let trimmed = text.trim_end_matches('\n');
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.chars().take_while(|c| *c == ' ').count())
                    }
                }
                DiffLine::HunkHeader { .. } => None,
            })
            .min()
            .unwrap_or(0);

        let mut content_count: usize = 0;
        let mut truncated_new_file = false;

        for dl in &hunk.lines {
            // 新文件截断：超过限制后停止渲染内容行
            if let Some(limit) = new_file_limit {
                if content_count >= limit && !matches!(dl, DiffLine::HunkHeader { .. }) {
                    truncated_new_file = true;
                    break;
                }
            }

            match dl {
                DiffLine::HunkHeader { text } => {
                    let truncated = truncate_to_width(text.trim_end_matches('\n'), width);
                    lines.push(Line::from(vec![Span::styled(
                        truncated,
                        Style::default().fg(theme.diff_hunk()),
                    )]));
                }
                DiffLine::Context {
                    text,
                    old_line_num,
                    new_line_num,
                } => {
                    content_count += 1;
                    let gutter = format!(" {:>n$} {:>n$} │ ", old_line_num, new_line_num, n = n);
                    let dedented = dedent_line(text, common_indent);
                    let content = truncate_to_width(&dedented, width.saturating_sub(gutter_width));
                    lines.push(Line::from(vec![
                        Span::styled(gutter, Style::default().fg(theme.dim())),
                        Span::styled(content, Style::default()),
                    ]));
                }
                DiffLine::Add {
                    text,
                    line_num,
                    word_diff,
                } => {
                    content_count += 1;
                    let marker = "+";
                    let gutter = format!("{}{:>n$} {:>n$} │ ", marker, "", line_num, n = n);
                    let fg = theme.diff_add();
                    let bg = theme.diff_add_bg();
                    let gutter_style = Style::default().fg(fg).bg(bg);
                    let content_width = width.saturating_sub(gutter_width);

                    if let Some(wd) = word_diff {
                        let dedented = dedent_line(text, common_indent);
                        let spans = render_word_diff_spans(
                            wd,
                            &dedented,
                            fg,
                            bg,
                            theme.diff_add_word_bg(),
                            content_width,
                        );
                        let mut result_spans = vec![Span::styled(gutter, gutter_style)];
                        result_spans.extend(spans);
                        lines.push(Line::from(result_spans));
                    } else {
                        let dedented = dedent_line(text, common_indent);
                        let content = truncate_to_width(&dedented, content_width);
                        let padded = pad_to_width(&content, content_width);
                        lines.push(Line::from(vec![
                            Span::styled(gutter, gutter_style),
                            Span::styled(padded, Style::default().fg(fg).bg(bg)),
                        ]));
                    }
                }
                DiffLine::Remove {
                    text,
                    line_num,
                    word_diff,
                } => {
                    content_count += 1;
                    let marker = "-";
                    let gutter = format!("{}{:>n$} {:>n$} │ ", marker, line_num, "", n = n);
                    let fg = theme.diff_remove();
                    let bg = theme.diff_remove_bg();
                    let gutter_style = Style::default().fg(fg).bg(bg);
                    let content_width = width.saturating_sub(gutter_width);

                    if let Some(wd) = word_diff {
                        let dedented = dedent_line(text, common_indent);
                        let spans = render_word_diff_spans(
                            wd,
                            &dedented,
                            fg,
                            bg,
                            theme.diff_remove_word_bg(),
                            content_width,
                        );
                        let mut result_spans = vec![Span::styled(gutter, gutter_style)];
                        result_spans.extend(spans);
                        lines.push(Line::from(result_spans));
                    } else {
                        let dedented = dedent_line(text, common_indent);
                        let content = truncate_to_width(&dedented, content_width);
                        let padded = pad_to_width(&content, content_width);
                        lines.push(Line::from(vec![
                            Span::styled(gutter, gutter_style),
                            Span::styled(padded, Style::default().fg(fg).bg(bg)),
                        ]));
                    }
                }
            }
        }

        // 新文件截断提示
        if truncated_new_file {
            let total_lines: usize = result
                .hunks
                .iter()
                .map(|h| {
                    h.lines
                        .iter()
                        .filter(|l| !matches!(l, DiffLine::HunkHeader { .. }))
                        .count()
                })
                .sum();
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "  ... {} more lines not shown",
                    total_lines.saturating_sub(NEW_FILE_MAX_LINES)
                ),
                Style::default().fg(theme.dim()),
            )]));
            break;
        }
    }

    lines
}

/// 裁剪行的前 N 个空格字符（公共缩进去重）
fn dedent_line(text: &str, indent: usize) -> String {
    let trimmed = text.trim_end_matches('\n');
    if indent == 0 {
        return trimmed.to_string();
    }
    let mut chars = trimmed.chars();
    let mut skipped = 0;
    while skipped < indent {
        match chars.next() {
            Some(' ') => skipped += 1,
            _ => break,
        }
    }
    chars.as_str().to_string()
}

/// Pad string to exact display width (CJK-safe), filling with spaces
fn pad_to_width(s: &str, target_width: usize) -> String {
    let current = unicode_width::UnicodeWidthStr::width(s);
    if current >= target_width {
        s.to_string()
    } else {
        let mut out = s.to_string();
        out.extend(std::iter::repeat_n(' ', target_width - current));
        out
    }
}

/// 渲染 word diff 段：未变部分用 base bg，变更部分用 word_bg（更深背景）
fn render_word_diff_spans(
    wd: &super::WordDiff,
    full_text: &str,
    fg: Color,
    bg: Color,
    word_bg: Color,
    max_width: usize,
) -> Vec<Span<'static>> {
    // 先判断是否所有段都是 Unchanged
    let has_change = wd
        .segments
        .iter()
        .any(|(_, t)| !matches!(t, DiffWordType::Unchanged));
    if !has_change {
        let content = truncate_to_width(full_text.trim_end_matches('\n'), max_width);
        let padded = pad_to_width(&content, max_width);
        return vec![Span::styled(padded, Style::default().fg(fg).bg(bg))];
    }

    let mut spans = Vec::new();
    let mut used_width = 0usize;

    for (text, word_type) in &wd.segments {
        if used_width >= max_width {
            break;
        }
        let remaining = max_width - used_width;
        let truncated = truncate_to_width(text, remaining);
        used_width += unicode_width::UnicodeWidthStr::width(truncated.as_str());

        let is_changed = !matches!(word_type, DiffWordType::Unchanged);
        let span_bg = if is_changed { word_bg } else { bg };
        spans.push(Span::styled(truncated, Style::default().fg(fg).bg(span_bg)));
    }

    // 充满剩余宽度，保证整行背景色
    if used_width < max_width {
        let padding = " ".repeat(max_width - used_width);
        spans.push(Span::styled(padding, Style::default().bg(bg)));
    }

    spans
}

#[cfg(test)]
#[path = "renderer_test.rs"]
mod tests;
