//! vm_to_lines + 各变体渲染函数 + 辅助函数。
//!
//! [Slice 3] 统一水平网格（§3.1）：所有 entry 共享 `outer + accent + gap + content`
//! 前缀结构——块首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][dim 竖线][gap]`；
//! Narrow 断点 accent 符号退化为 bullet（§11）。正文全部从 content 列起点对齐。

use crate::i18n;
#[cfg(test)]
use crate::kit::message_area::grid::Breakpoint;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

mod group;
mod helpers;
mod interaction;
mod reasoning;
mod semantic;
mod system;
mod tool_card;
mod user;

#[cfg(test)]
use self::group::SubAgentSummary;
use self::group::{render_collapsed_group_lines, render_subagent_group_lines};
use self::helpers::{format_completed_duration, place_meta, prefixed_cont_line};
pub(crate) use self::interaction::InteractionLayout;
use self::interaction::render_ask_user_block_lines;
use self::reasoning::render_reasoning_block;
pub(crate) use self::semantic::semantic_line_text;
#[cfg(test)]
use self::semantic::strip_visual_prefix;
use self::system::{render_divider_lines, render_system_note_lines, render_todo_summary_lines};
use self::tool_card::render_tool_card_lines;
#[cfg(test)]
use self::user::build_image_meta_info;
pub(crate) use self::user::{parse_image_line, render_image_hover_line};
use self::user::{render_reminder_condensed, render_user_bubble_lines};

// ── vm_to_lines：TuiRenderUnit → Vec<Line<'static>> ───────────────────────

/// md 复制按钮信息：按钮行在 VM lines 内的逻辑索引 + 行内列范围（相对消息区）。
/// 由 [`vm_to_lines_cached`] 在渲染按钮行时返回，供 MessageArea 构建屏幕点击区域。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CopyButtonInfo {
    /// 按钮行在 VM lines 中的逻辑索引。
    pub(super) logical_idx: usize,
    /// 按钮文本起始列（行内，相对消息区 x=0）。
    pub(super) x_start: u16,
    /// 按钮文本结束列（不含）。
    pub(super) x_end: u16,
}

/// @image 行的渲染期信息（image-p0-p1-spec §4 T4；VmCacheSlot 内持有，
/// rebuild 时随 lines 重建）。供 MessageArea 构建点击/hover 屏幕映射。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageLineInfo {
    /// slot 内逻辑行索引（wrap_map 中该 meta 行的 visual_start）。
    pub(super) logical_idx: usize,
    /// 展示路径（T5 canonicalize 后；失败时为原始文本）——open 目标 + hover 显示。
    pub(super) path: String,
    /// 受管理目录内（~/.peri/images）→ 自动预览候选；手工路径 → 仅文本
    /// （差异在 T7 预览，本任务显示层两者一致，§6.1 Q6）。
    pub(super) managed: bool,
    /// 重建期算好的大小文案（B/KB/MB 或 missing）——hover 渲染复用，
    /// hover 时不再 stat（§4.4 stat 时机取舍）。
    pub(super) size_text: String,
}

/// md 复制按钮的最小内容长度（字符数，`chars().count()` 口径——与复制反馈
/// `mark_copy_message` 同一计数方式）。短消息直接选中复制即可，不渲染按钮，
/// 避免每条消息都出现一行按钮造成视觉噪音。
const MD_COPY_MIN_CHARS: usize = 400;

/// 生成 md 复制按钮行（AssistantBubble 内容末尾）。
///
/// 布局：行尾右对齐的 ` Copy `——整个反色块（含左右 1 空格）即按钮视觉，
/// 前导无样式空格把按钮推到 content 列右缘（继承消息区背景，视觉上不留痕迹）；
/// 点击区域与反色块完全重合（[x_start, x_end) = 按钮块本身，不含前导空格）。
/// 宽度不足（按钮行会被折行）时返回 None——调用方不渲染按钮行，避免
/// 点击区域与实际渲染位置错位（同 footer keepgoing 的 m4 fix）。
fn copy_button_line(grid: &GridSpec) -> Option<(Line<'static>, u16, u16)> {
    let semantic = THEME_ATOM.state().read().semantic;
    let btn_text = i18n::tr("msg-copy-md");
    let btn_span = Span::styled(
        format!(" {btn_text} "),
        // 反色：accent 前景 + REVERSED → 终端
        Style::default()
            .fg(semantic.accent)
            .add_modifier(Modifier::REVERSED),
    );
    let btn_width = btn_span.width();
    // 按钮行宽度 = 前缀（outer+accent+gap）+ content——按钮右对齐在 content 列右缘。
    let line_width = grid.first_prefix_width() + grid.content_width();
    if btn_width > line_width {
        return None;
    }
    // 右对齐：前导无样式空格（默认样式，渲染时继承消息区背景）占满按钮左侧。
    let line = Line::from(vec![
        Span::raw(" ".repeat(line_width - btn_width)),
        btn_span,
    ]);
    let x_start = (line_width - btn_width) as u16;
    let x_end = line_width as u16;
    Some((line, x_start, x_end))
}

/// 将单个 TuiRenderUnit 变体转换为渲染行。
/// 使用 kit::markdown 进行 markdown 解析（无缓存，每次全量）。
///
/// 仅测试断言使用（[`render_test`]）——生产路径统一走 [`vm_to_lines_cached`]。
#[cfg(test)]
pub(super) fn vm_to_lines(vm: &TuiRenderUnit, grid: &GridSpec) -> Vec<Line<'static>> {
    vm_to_lines_cached(
        vm,
        grid,
        &mut crate::kit::markdown::MarkdownRenderCache::default(),
        false,
    )
    .0
}

/// 与 [`vm_to_lines`] 同逻辑，但接受 markdown 渲染缓存以支持增量续跑。///
/// [Phase 2] 流式期间 text 末尾追加 token 时，前缀 blocks（已闭合的 paragraph /
/// list item / code block）完全不变——cache 通过文本前缀比较复用上次处理到
/// stable_state 的累积状态，仅处理新增 block。
///
/// `render_copy_button` 为 true 时（顶层 MessageArea），在 AssistantBubble 的
/// markdown 内容末尾追加一行 md 复制按钮，并返回按钮的布局信息（供点击检测）。
/// 嵌套渲染（历史 SubAgentGroup 递归 / subagent 详情面板）传 false。
///
/// 返回四元组 `(lines, copy_button, interaction_layout, image_lines)`——第三项仅 pending
/// 的 interaction block（§6.8）有值（选项行布局，供视口 post-pass 高亮与
/// 点击热区）；第四项仅 UserBubble 的 @image 行有值（§4 T4，点击/hover 映射）；
/// 其余变体恒 None / 空 Vec。
pub(crate) fn vm_to_lines_cached(
    vm: &TuiRenderUnit,
    grid: &GridSpec,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
    render_copy_button: bool,
) -> (
    Vec<Line<'static>>,
    Option<CopyButtonInfo>,
    Option<InteractionLayout>,
    Vec<ImageLineInfo>,
) {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let mut lines: Vec<Line<'static>> = Vec::new();

            // §3.2 垂直节奏：user 与 assistant 正文块上下各保留 1 空行；
            // 节拍空行带竖线前缀（左缘时间线不断链）。reasoning 是过程 entry，
            // 不自行增加空行；若后接正文，正文前导空行负责分隔。空 bubble
            // （无 text 无 reasoning）仍返回 0 行。
            if data.text.is_empty() && data.reasoning.is_none() {
                return (lines, None, None, Vec::new());
            }

            // AI 正文竖线颜色——正文行与前后 turn 节拍空行共用；主题值在
            // guard 生命周期内复制（TUI-THEME-001），不跨渲染路径持有读锁。
            let (md_text_fg, line_color) = {
                let theme_guard = peri_theme::atoms::THEME_ATOM.state();
                let theme = theme_guard.read();
                (theme.component.markdown.text, theme.semantic.accent)
            };

            // 推理块（§6.3）——视觉独立 entry
            if let Some(ref reasoning) = data.reasoning {
                lines.extend(render_reasoning_block(reasoning, grid));
                // 块尾不加空行（running/completed 一致）——与工具卡片紧凑布局
                // 对齐；正文紧随其后，由 md 渲染自身节奏负责分段。
            }

            // Markdown 正文——上下各 1 空行；wrap 在 content 列宽，行级再套统一前缀。
            if !data.text.is_empty() {
                // 前导空行带竖线前缀（与正文同色）——turn 节拍空行不断链。
                lines.push(prefixed_cont_line(grid, line_color, Line::default()));
                let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
                let palette_guard = palette_state.read();
                let segments = crate::kit::markdown::parse_markdown_cached(
                    &data.text,
                    grid.content_width(),
                    *palette_guard,
                    md_text_fg,
                    md_cache,
                );
                let mut prev_seg: Option<&crate::kit::markdown::MarkdownSegment> = None;
                for (seg_idx, seg) in segments.iter().enumerate() {
                    // segment 之间加空行（表格 ↔ 文本边界；T3 §3.5 图片间隙规则）：
                    // - 行内图片前后无间隙（拆段后前后片段仍属同一视觉段落）
                    // - 独占图片段连续（同段多图已在 convert 层合并为单段，
                    //   P1-1）仅首段前加；跨段独立图片走默认规则（前后空行）
                    // - 其余保持现有逻辑（首段 / 前段末行为空行时不加）
                    let gap = match (prev_seg, seg) {
                        (_, crate::kit::markdown::MarkdownSegment::Image(img))
                            if !img.standalone =>
                        {
                            false
                        }
                        (Some(crate::kit::markdown::MarkdownSegment::Image(p)), _)
                            if !p.standalone =>
                        {
                            false
                        }
                        _ => seg_idx > 0 && !lines.last().is_some_and(|l| l.spans.is_empty()),
                    };
                    if gap {
                        lines.push(prefixed_cont_line(grid, line_color, Line::default()));
                    }
                    match seg {
                        crate::kit::markdown::MarkdownSegment::Text(seg_lines) => {
                            for line in seg_lines {
                                lines.push(prefixed_cont_line(grid, line_color, line.clone()));
                            }
                        }
                        crate::kit::markdown::MarkdownSegment::Image(img) => {
                            // 降级行已在 convert 阶段折行（wrap_styled_line），
                            // 前缀/空行逻辑与 Text 分支一致。
                            for line in &img.lines {
                                lines.push(prefixed_cont_line(grid, line_color, line.clone()));
                            }
                        }
                        crate::kit::markdown::MarkdownSegment::Table(data) => {
                            let mut table_theme =
                                ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                            // 行文字使用终端默认色，而非纯白
                            table_theme.row_style = Style::default();
                            let table_lines = crate::kit::markdown::table_data_to_lines(
                                data,
                                &table_theme,
                                grid.content_width(),
                            );
                            for tl in table_lines {
                                lines.push(prefixed_cont_line(grid, line_color, tl));
                            }
                        }
                    }
                    prev_seg = Some(seg);
                }
            }

            // §6.2 完成时长 meta（G-Tokens 仅 duration）：冻结值在正文末行尾部
            // 三档放置（Wide 右对齐 / Standard 紧跟 / Compact/Narrow 隐藏），
            // 不独占一行。空正文（纯 reasoning 块）无放置点 → 跳过。
            if let Some(duration_ms) = data.duration_ms {
                let sem = THEME_ATOM.state().read().semantic;
                let meta = format_completed_duration(duration_ms);
                let meta_style = Style::default().fg(sem.text.dim);
                if let Some(last_non_empty) = lines.iter_mut().rev().find(|l| !l.spans.is_empty()) {
                    let used: usize = last_non_empty.spans.iter().map(|s| s.content.width()).sum();
                    if let Some(meta_spans) = place_meta(grid, used, &meta, meta_style) {
                        last_non_empty.spans.extend(meta_spans);
                    }
                }
            }

            // md 复制按钮：独立于 markdown 渲染，追加在内容末尾。
            // 仅当内容超过 MD_COPY_MIN_CHARS 字符（chars().count() 口径）才渲染。
            let copy_button = if render_copy_button && data.text.chars().count() > MD_COPY_MIN_CHARS
            {
                if let Some((btn_line, x_start, x_end)) = copy_button_line(grid) {
                    // 不插空行：按钮紧贴内容末行，视觉上更紧凑
                    let logical_idx = lines.len();
                    lines.push(btn_line);
                    Some(CopyButtonInfo {
                        logical_idx,
                        x_start,
                        x_end,
                    })
                } else {
                    None
                }
            } else {
                None
            };

            if !data.text.is_empty() {
                // 尾随空行同样带竖线前缀——turn 节拍对称且左缘时间线不断链。
                lines.push(prefixed_cont_line(grid, line_color, Line::default()));
            }

            (lines, copy_button, None, Vec::new())
        }
        TuiRenderUnit::TuiUserBubble(data) => {
            if let Some(ref info) = data.reminder {
                return (
                    render_reminder_condensed(info, grid),
                    None,
                    None,
                    Vec::new(),
                );
            }
            let (lines, image_lines) = render_user_bubble_lines(data, grid);
            (lines, None, None, image_lines)
        }
        TuiRenderUnit::TuiToolCard(data) => {
            (render_tool_card_lines(data, grid), None, None, Vec::new())
        }
        TuiRenderUnit::TuiSystemNote(data) => {
            (render_system_note_lines(data, grid), None, None, Vec::new())
        }
        TuiRenderUnit::TuiSubAgentGroup(data) => (
            render_subagent_group_lines(data, grid),
            None,
            None,
            Vec::new(),
        ),
        TuiRenderUnit::TuiCollapsedGroup(data) => (
            render_collapsed_group_lines(data, grid),
            None,
            None,
            Vec::new(),
        ),
        TuiRenderUnit::TuiDivider(data) => {
            (render_divider_lines(data, grid), None, None, Vec::new())
        }
        TuiRenderUnit::TuiAskUserBlock(data) => {
            let (lines, layout) = render_ask_user_block_lines(data, grid);
            (lines, None, layout, Vec::new())
        }
        TuiRenderUnit::TuiTodoSummary(data) => (
            render_todo_summary_lines(data, grid),
            None,
            None,
            Vec::new(),
        ),
    }
}

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;
