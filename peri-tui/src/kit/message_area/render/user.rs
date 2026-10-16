use crate::i18n;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{FoldState, UserAttachment};
use crate::truncate::{truncate_by_width, wrap_by_width};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::ImageLineInfo;
use super::helpers::{
    cont_prefix, first_prefix, prefixed_cont_line, process_cont_prefix, process_first_prefix, sym,
};

/// 用户 prompt 正文最多展示的视觉行数（§6.1），超出显示 `… +N lines`。
const USER_BODY_MAX_LINES: usize = 6;

// ── 各变体渲染函数（Slice 3：统一网格 + 无气泡 + 垂直节奏）──────────────

/// 系统提醒（§7：过程区 entry）——首行 `[outer 空][indent 留空][标记][gap]`
/// 后接折叠标记与标题，续行与首行标签同列（§3/§4/§5）；折行预算取
/// `process_content_width()`（§8 右缘不动）。
pub(super) fn render_system_reminder_lines(
    data: &crate::kit::tui_render_unit::TuiSystemReminder,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let marker = if data.legacy {
        i18n::tr("reminder-legacy-marker")
    } else if data.required {
        i18n::tr("reminder-required-marker")
    } else {
        i18n::tr("reminder-structured-marker")
    };
    let compact_label = data
        .wire
        .as_ref()
        .and_then(|reminder| match reminder.kind.as_str() {
            "compact_file" => Some("reminder-compact-file"),
            "compact_skill" => Some("reminder-compact-skill"),
            "compact_summary" => Some("reminder-compact-summary"),
            _ => None,
        });
    let heading = if let Some(label) = compact_label {
        format!("{} · {}", marker, i18n::tr(label))
    } else {
        format!(
            "{} · {} · {} · {:?}",
            marker, data.category, data.source, data.severity
        )
    };
    let symbol = if data.fold == FoldState::Expanded {
        sym().expanded
    } else {
        sym().collapsed
    };
    let mut spans = process_first_prefix(grid, symbol, Style::default().fg(sem.text.dim));
    spans.push(Span::styled(
        truncate_by_width(&heading, grid.process_content_width()),
        Style::default().fg(sem.text.dim),
    ));
    let mut lines = vec![Line::from(spans)];
    if data.fold == FoldState::Expanded {
        if let Some(relay) = &data.relay {
            // 子 Agent 转发展示归属：仅展开时显示，便于区分“本会话提醒”与“子任务提醒”。
            let text = i18n::tr_args(
                "reminder-relayed-from",
                &[("name".into(), FluentValue::from(relay.label()))],
            );
            let mut spans = process_cont_prefix(grid, sem.accents.reasoning);
            spans.push(Span::styled(text, Style::default().fg(sem.text.dim)));
            lines.push(Line::from(spans));
        }
        for text in data
            .body
            .lines()
            .flat_map(|line| wrap_by_width(line, grid.process_content_width()))
        {
            let mut spans = process_cont_prefix(grid, sem.accents.reasoning);
            spans.push(Span::styled(text, Style::default().fg(sem.text.muted)));
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// §6.1 User prompt：去全宽 bg 与 `❯`；无 role label（`You`），正文直接开始。
///
/// - 用户正文在所有宽度下直接开始；Compact/Narrow 只保留尾部 1 个空行
///   分隔后续内容，空行带竖线前缀。正文行 `[│][gap]` 与其余 entry 同起点。
/// - 保留用户换行；长 prompt 最多 `USER_BODY_MAX_LINES` 个视觉行，
///   超出显示 `… +N lines`（§6.1）。`USER_BODY_MAX_LINES` 计数包含
///   附件记录行与 @image meta 行（§4.4）。
/// - 正文左侧竖线使用 text.secondary；slash command / `@mention` 局部强调（accent.user）。
/// - T4（image-p0-p1-spec §4）：`@image <path>` 行识别为 meta 行
///   `[Image: {文件名} · {大小}]`（不解析像素），返回 [`ImageLineInfo`]
///   列表供点击/hover 屏幕映射。
/// - 上传式附件（粘贴图片）：正文之前逐张渲染记录行
///   `[图片] 图片 {序号} · {大小}`——附件不落盘、无 `@image` 文本，这是发送后
///   唯一可见的「这条消息带了图片」记录（不提供预览/命中映射）。
pub(super) fn render_user_bubble_lines(
    data: &crate::kit::tui_render_unit::TuiUserBubble,
    grid: &GridSpec,
) -> (Vec<Line<'static>>, Vec<ImageLineInfo>) {
    let sem = THEME_ATOM.state().read().semantic;
    // 空文本 user（rewind/重放路径的 thinking 回传消息建模为 user role，
    // 提取文本为空）→ 渲染 0 行——不产生 turn 节拍空行，避免 thinking
    // 底下出现悬空空行（§3.2 节拍只属于真实 user prompt）。带附件的空文本
    // 提交（图片-only）是真实 prompt，仍要留下附件记录。
    if data.text.is_empty() && data.attachments.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 正文：保留用户换行；每行按 display width 折行成「视觉行」（§6.1 口径），
    // 最多 USER_BODY_MAX_LINES 个视觉行，超出显示 `… +N lines`（§12：grapheme
    // + display width，CJK/emoji 不被从中间切开）。@image 行在 wrap **之前**
    // 识别（§4.4），渲染为 meta 行（文件名 · 大小），同样计入行数上限。
    // [stat 时机] metadata 只在 rebuild 路径执行（本函数仅 rebuild 时调用）；
    // 文件大小变化不触发 rebuild（hash 不含 stat），下次内容变化自然刷新（§4.4）。
    let mut image_lines: Vec<ImageLineInfo> = Vec::new();
    let mut visual_count = 0usize; // 已渲染视觉行
    let mut total = 0usize; // 全部视觉行（含超限部分，用于 `… +N lines`）
    // 附件记录行先于正文：一条一张，`[图片] 图片 1 · 128.0 KB`——发送后仍能看出
    // 这条消息带了图片（上传式附件没有可复制的路径文本，只有这里的记录）。
    for (index, attachment) in data.attachments.iter().enumerate() {
        push_meta_body_lines(
            &mut lines,
            grid,
            attachment_meta_text(attachment, index + 1),
            sem.text.secondary,
            Style::default().fg(sem.text.primary),
            &mut visual_count,
            &mut total,
        );
    }
    for raw in data.text.lines() {
        let is_image = parse_image_line(raw);
        if let Some(path) = &is_image {
            let (display_path, size_text, managed, meta_text) = build_image_meta_info(path, None);
            let (logical_idx, take, meta_len) = push_meta_body_lines(
                &mut lines,
                grid,
                meta_text,
                sem.text.secondary,
                Style::default().fg(sem.text.primary),
                &mut visual_count,
                &mut total,
            );
            // 仅完整渲染的 meta 行进入命中映射（被截断的行点击区域与渲染
            // 位置错位——与 footer keepgoing 超宽跳过同一原则）。
            if take == meta_len {
                image_lines.push(ImageLineInfo {
                    logical_idx,
                    path: display_path,
                    managed,
                    size_text,
                });
            }
            continue;
        }
        let wrapped = wrap_by_width(raw, grid.content_width());
        total += wrapped.len();
        if visual_count >= USER_BODY_MAX_LINES {
            continue;
        }
        let take = (USER_BODY_MAX_LINES - visual_count).min(wrapped.len());
        for w in &wrapped[..take] {
            let mut spans = cont_prefix(grid, sem.text.secondary);
            spans.extend(emphasize_user_line(w, grid, &sem));
            lines.push(Line::from(spans));
        }
        visual_count += take;
    }
    if total > USER_BODY_MAX_LINES {
        let more = i18n::tr_args(
            "render-more-lines",
            &[(
                "count".to_string(),
                FluentValue::from((total - USER_BODY_MAX_LINES) as u64),
            )],
        );
        let mut spans = cont_prefix(grid, sem.text.secondary);
        spans.push(Span::styled(more, Style::default().fg(sem.text.dim)));
        lines.push(Line::from(spans));
    }
    // §3.2：user 尾部 1 空行——Compact/Narrow 用空行分隔后续
    // thinking/tool；Wide/Standard 由转场台阶承担分隔，不再保留。尾随空行带
    // 竖线前缀，左缘时间线连续。
    if grid.indent == 0 {
        lines.push(prefixed_cont_line(
            grid,
            sem.text.secondary,
            Line::default(),
        ));
    }
    (lines, image_lines)
}

/// 追加一条 meta 文本（按网格宽度折行）为正文视觉行，并计入 §6.1 行数上限。
///
/// 返回 `(首个视觉行的 logical index, 实际渲染的视觉行数, 该 meta 的视觉行总数)`——
/// 被截断（`take < 总数`）的调用方据此跳过命中映射登记。
fn push_meta_body_lines(
    lines: &mut Vec<Line<'static>>,
    grid: &GridSpec,
    text: String,
    prefix_color: ratatui_kit::ratatui::style::Color,
    text_style: Style,
    visual_count: &mut usize,
    total: &mut usize,
) -> (usize, usize, usize) {
    let wrapped = wrap_by_width(&text, grid.content_width());
    *total += wrapped.len();
    let logical_idx = lines.len();
    if *visual_count >= USER_BODY_MAX_LINES {
        return (logical_idx, 0, wrapped.len());
    }
    let take = (USER_BODY_MAX_LINES - *visual_count).min(wrapped.len());
    for segment in &wrapped[..take] {
        let mut spans = cont_prefix(grid, prefix_color);
        spans.push(Span::styled(segment.clone(), text_style));
        lines.push(Line::from(spans));
    }
    *visual_count += take;
    (logical_idx, take, wrapped.len())
}

/// 附件记录行文本：`[图片] 图片 {序号} · {大小}`。
///
/// 大小复用 `@image` meta 行的 [`human_size`] 三档；不解析图片内容（无缩略预览）——
/// 载荷不在视图模型里（见 `UserAttachment`）。
fn attachment_meta_text(attachment: &UserAttachment, index: usize) -> String {
    i18n::tr_args(
        "user-attachment-image-meta",
        &[
            (
                "name".to_string(),
                FluentValue::from(crate::kit::tui_render_unit::user_attachment_name(index)),
            ),
            (
                "size".to_string(),
                FluentValue::from(human_size(attachment.bytes as u64)),
            ),
        ],
    )
}

/// @image 行识别（§4.4）：原始行（wrap 之前）`trim_start` 后以 `@image ` 开头
/// 且剩余路径非空 → 图片行。返回路径（trim 后）；非图片行返回 None
/// （`@image` 无路径 / `@imagefoo` 普通文本 / `@image ` 中间空格 → 走原
/// `emphasize_user_line` 路径）。
///
/// `pub(crate)` 供 T7（image_overlay.rs focus 触发源）复用同一判定，避免漂移。
pub(crate) fn parse_image_line(raw: &str) -> Option<String> {
    let rest = raw.trim_start().strip_prefix("@image ")?;
    let path = rest.trim();
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

/// 人类可读文件大小（§4.4）：B/KB/MB（1024 进制，KB/MB 一位小数）。
/// 自实现辅助（spec §4.4 约定放 render.rs 私有 fn）。
fn human_size(bytes: u64) -> String {
    let (key, value): (&str, FluentValue<'_>) = if bytes < 1024 {
        ("user-image-size-bytes", FluentValue::from(bytes))
    } else if bytes < 1024 * 1024 {
        (
            "user-image-size-kb",
            FluentValue::from(format!("{:.1}", bytes as f64 / 1024.0)),
        )
    } else {
        (
            "user-image-size-mb",
            FluentValue::from(format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))),
        )
    };
    i18n::tr_args(key, &[("count".to_string(), value)])
}

/// @image 行 meta 信息构建（§4.4）：
/// - meta 文本 `[Image: {文件名} · {大小}]`——文件名 = path 的 `file_name()`，
///   大小 = `std::fs::metadata` 的 `len()`（人类可读 B/KB/MB，**不解析像素**）；
/// - `metadata` 失败（文件被删）→ `missing` 文案（i18n key）；
/// - 显示层级（§6.1 Q6）：受管理与手工路径**显示一致**（文件名 + 大小，不暴露
///   绝对路径——§6.2-5 路径泄漏约束）；managed 标志仅记录供 T7 预览资格判定；
/// - 路径进终端前过 T5 `sanitize_for_terminal`（控制字符过滤，§6.2-4）。
///
/// 返回 `(display_path, size_text, managed, meta_text)`——`size_text` 供 hover
/// 渲染复用（hover 时不再 stat，§4.4 stat 时机取舍）。
///
/// `managed_root` 为受管理根注入版（生产传 None 用 `~/.peri/images`；测试用
/// tempdir 模拟）。
pub(crate) fn build_image_meta_info(
    path: &str,
    managed_root: Option<&std::path::Path>,
) -> (String, String, bool, String) {
    let path_buf = std::path::Path::new(path);
    let (grade, canonical) = match managed_root {
        Some(root) => crate::kit::image_safety::grade_path_with_root(path_buf, root),
        None => crate::kit::image_safety::grade_path(path_buf),
    };
    let display_path = canonical
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let name = path_buf
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let size_text = match std::fs::metadata(path) {
        Ok(md) if md.is_file() => human_size(md.len()),
        _ => i18n::tr("user-image-missing"),
    };
    let meta_text = i18n::tr_args(
        "user-image-meta",
        &[
            ("name".to_string(), FluentValue::from(name)),
            ("size".to_string(), FluentValue::from(size_text.clone())),
        ],
    );
    let meta_text = crate::kit::image_safety::sanitize_for_terminal(&meta_text).into_owned();
    (
        display_path,
        size_text,
        grade == crate::kit::image_safety::PathGrade::Managed,
        meta_text,
    )
}

/// @image 行 hover 渲染（§4.4）：`[Image: {绝对路径} · {size}]` + accent 高亮
/// （复用 `emphasize_user_line` 的 `sem.accents.user` 样式）。视口 post-pass
/// 每帧调用（仅 hover 行构建）；单行截断（truncate 而非 wrap）——布局稳定，
/// 不因路径变长改变行高。
pub(crate) fn render_image_hover_line(
    hover: &crate::kit::message_area::ImageHoverState,
    grid: &GridSpec,
    sem: &peri_theme::semantic::SemanticTokens,
) -> Line<'static> {
    let text = i18n::tr_args(
        "user-image-meta",
        &[
            ("name".to_string(), FluentValue::from(hover.path.as_str())),
            (
                "size".to_string(),
                FluentValue::from(hover.size_text.as_str()),
            ),
        ],
    );
    // §4.6 path 显示注入：路径进终端前过 T5 sanitize（控制字符过滤）。
    let text = crate::kit::image_safety::sanitize_for_terminal(&text);
    let clipped = truncate_by_width(&text, grid.content_width());
    let mut spans = cont_prefix(grid, sem.text.secondary);
    spans.push(Span::styled(clipped, Style::default().fg(sem.accents.user)));
    Line::from(spans)
}

/// 用户正文行的局部强调（§6.1）：`@mention` / `/command` token 用 accent.user，
/// 其余正文 text.primary。
fn emphasize_user_line(
    raw: &str,
    grid: &GridSpec,
    sem: &peri_theme::semantic::SemanticTokens,
) -> Vec<Span<'static>> {
    let primary = Style::default().fg(sem.text.primary);
    let emphasis = Style::default().fg(sem.accents.user);
    let budget = grid.content_width();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut width = 0usize;
    let mut truncated = false;
    for token in raw.split_inclusive(' ') {
        let w = token.trim_end().width().max(1);
        if width + w > budget {
            truncated = true;
            break;
        }
        let is_emph = token.starts_with('@') || token.starts_with('/');
        spans.push(Span::styled(
            token.to_string(),
            if is_emph { emphasis } else { primary },
        ));
        width += w;
    }
    if truncated {
        // 截断行尾部补省略号（预算 1 列）
        if width >= budget {
            spans.pop();
        }
        spans.push(Span::styled("\u{2026}", primary));
    }
    spans
}

/// 用户消息内的 system-reminder（§6.1/§6.6）：按来源型 system event 渲染——
/// 首行 `[!][gap]{来源 label}`，续行 `[│][gap]{摘要}`。
///
/// 位置属**文本区**（在用户气泡内，随 `TuiUserBubble` 的文本区 zone）：走基准前缀与
/// `content_width()`，不参与过程区缩进/分叉——独立成 entry 的提醒才走过程区
/// （[`render_system_reminder_lines`]）。
pub(super) fn render_reminder_condensed(
    info: &crate::kit::tui_render_unit::ReminderInfo,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = vec![Line::from({
        let mut spans = first_prefix(grid, sym().warning, Style::default().fg(sem.status.warning));
        spans.push(Span::styled(
            info.reminder_type.label(),
            Style::default()
                .fg(sem.text.muted)
                .add_modifier(Modifier::ITALIC),
        ));
        spans
    })];
    if !info.summary.is_empty() {
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&info.summary, grid.content_width()),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    lines
}
