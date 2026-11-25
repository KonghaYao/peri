//! vm_to_lines + 各变体渲染函数 + 辅助函数。
//!
//! [Slice 3] 统一水平网格（§3.1）：所有 entry 共享 `outer + accent + gap + content`
//! 前缀结构——块首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][dim 竖线][gap]`；
//! Narrow 断点 accent 符号退化为 bullet（§11）。正文全部从 content 列起点对齐。

use super::{error, group, helpers, interaction, reasoning, system, tool_card, user};

use std::sync::Arc;

use crate::i18n;
use peri_tui_components::genui::GenUiButtonInfo;

use crate::kit::entry_render_cache::{EntryRenderOptions, ImageMode, MermaidMode};
use crate::kit::message_area::grid::{GridSpec, ZoneTransition};
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::button::{
    ButtonChrome, ButtonSpec, ButtonState, ButtonTone, button_visual,
};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Color, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use self::helpers::STEP_ENTER;
use self::helpers::STEP_EXIT;
use self::helpers::format_completed_duration;
use self::interaction::InteractionLayout;

// ── vm_to_lines：TuiRenderUnit → Vec<Line<'static>> ───────────────────────

/// md 复制按钮信息：按钮行在 VM lines 内的逻辑索引 + 行内列范围（相对消息区）。
/// 由 [`vm_to_lines_cached`] 在渲染按钮行时返回，供 MessageArea 构建屏幕点击区域。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CopyButtonInfo {
    /// 按钮行在 VM lines 中的逻辑索引。
    pub(in super::super) logical_idx: usize,
    /// 按钮文本起始列（行内，相对消息区 x=0）。
    pub(in super::super) x_start: u16,
    /// 按钮文本结束列（不含）。
    pub(in super::super) x_end: u16,
}

/// @image 行的渲染期信息（image-p0-p1-spec §4 T4；VmCacheSlot 内持有，
/// rebuild 时随 lines 重建）。供 MessageArea 构建点击/hover 屏幕映射；
/// `inline` 非空时该行是内联像素图片的占位首行（无路径/大小语义）。
#[derive(Debug, Clone)]
pub(crate) struct ImageLineInfo {
    /// slot 内逻辑行索引（wrap_map 中该 meta 行的 visual_start）。
    pub(in super::super) logical_idx: usize,
    /// 展示路径（T5 canonicalize 后；失败时为原始文本）——open 目标 + hover 显示。
    pub(crate) path: String,
    /// 受管理目录内（~/.peri/images）→ 自动预览候选；手工路径 → 仅文本
    /// （差异在 T7 预览，本任务显示层两者一致，§6.1 Q6）。
    pub(in super::super) managed: bool,
    /// 重建期算好的大小文案（B/KB/MB 或 missing）——hover 渲染复用，
    /// hover 时不再 stat（§4.4 stat 时机取舍）。
    pub(crate) size_text: String,
    /// 内联像素图片放置（消息区 draw hook 绘制）；meta 行为 `None`。
    pub(in super::super) inline: Option<InlineImagePlacement>,
}

/// 内联像素图片的渲染期放置：像素协议句柄 + 占用的单元格 + 正文列起点。
/// `logical_idx`（首行）由所在的 [`ImageLineInfo`] 承载。
#[derive(Debug, Clone)]
pub(crate) struct InlineImagePlacement {
    /// 图片列数（等比适配后真实编码宽度）。
    pub(in super::super) cols: u16,
    /// 图片行数（等比适配后真实编码高度，等于预留的占位行数）。
    pub(in super::super) rows: u16,
    /// 正文列起点（相对消息区 x=0；随行前缀形态变化）。
    pub(in super::super) x_offset: u16,
    /// 已编码的协议句柄；render body 只 clone `Arc`，不触 IO。
    pub(in super::super) image: std::sync::Arc<crate::kit::image_preview::InlineImage>,
}

impl PartialEq for ImageLineInfo {
    fn eq(&self, other: &Self) -> bool {
        self.logical_idx == other.logical_idx
            && self.path == other.path
            && self.managed == other.managed
            && self.size_text == other.size_text
            && match (&self.inline, &other.inline) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.cols == b.cols
                        && a.rows == b.rows
                        && a.x_offset == b.x_offset
                        && std::sync::Arc::ptr_eq(&a.image, &b.image)
                }
                _ => false,
            }
    }
}

/// `vm_to_lines_with_options` 的渲染产物：行、复制按钮、交互布局、
/// 图片行、GenUI 按钮、Mermaid 围栏范围、展开组成员标题命中表。元组过长，
/// 用别名避免调用方被类型签名淹没。
type RenderOutput = (
    Vec<Line<'static>>,
    Option<CopyButtonInfo>,
    Option<InteractionLayout>,
    Vec<ImageLineInfo>,
    Vec<GenUiButtonInfo>,
    Vec<crate::kit::mermaid::FenceSpan>,
    Vec<(usize, crate::kit::tui_render_unit::FoldKey)>,
);

/// md 复制按钮的最小内容长度（字符数，`chars().count()` 口径——与复制反馈
/// `mark_copy_message` 同一计数方式）。短消息直接选中复制即可，不渲染按钮，
/// 避免每条消息都出现一行按钮造成视觉噪音。
pub(super) const MD_COPY_MIN_CHARS: usize = 400;

/// 生成 md 复制按钮行（AssistantBubble 内容末尾）。
///
/// 布局：行首接正文续行前缀（`[outer 空][竖线][gap]`，竖线色 = 正文竖线色，
/// 左缘时间线不断链），按钮块右对齐在 content 列右缘；竖线与按钮之间是无样式
/// 空格（继承消息区背景，视觉上不留痕迹）；点击区域与反色块完全重合
/// （[x_start, x_end) = 按钮块本身，不含前缀与填充空格）。
/// 宽度不足（按钮块与竖线前缀重叠，或按钮行会被折行）时返回 None——调用方
/// 不渲染按钮行，避免点击区域与实际渲染位置错位（同 footer keepgoing 的 m4 fix）。
pub(super) fn copy_button_line(
    grid: &GridSpec,
    line_color: Color,
) -> Option<(Line<'static>, u16, u16)> {
    let theme = THEME_ATOM.state();
    let button = button_visual(
        ButtonSpec {
            label: &i18n::tr("msg-copy-md"),
            chrome: ButtonChrome::Filled,
            tone: ButtonTone::Primary,
            state: ButtonState::Normal,
        },
        &theme.read(),
    );
    let btn_span = Span::styled(button.text, button.style);
    let btn_width = btn_span.width();
    // 按钮行宽度 = 前缀（outer+accent+gap）+ content——按钮右对齐在 content 列右缘。
    let line_width = grid.first_prefix_width() + grid.content_width();
    let prefix_width = grid.cont_prefix_width();
    if prefix_width + btn_width > line_width {
        return None;
    }
    // 右对齐：竖线前缀之后的填充空格为默认样式（渲染时继承消息区背景）。
    let mut spans = self::helpers::cont_prefix(grid, line_color);
    spans.push(Span::raw(" ".repeat(line_width - prefix_width - btn_width)));
    spans.push(btn_span);
    let line = Line::from(spans);
    let x_start = (line_width - btn_width) as u16;
    let x_end = line_width as u16;
    Some((line, x_start, x_end))
}

/// 正文行是否为只有网格前缀的空白分隔行（空行经 `body_line` 后仍带竖线前缀）。
fn is_blank_body_line(line: &Line<'static>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.trim().is_empty() || span.content == "\u{2502}")
}

/// 渲染一处 Mermaid 围栏并记录它在消息行里的逻辑范围；渲染本身不排队。
/// 正文列宽与前缀宽度、围栏身份/帧都由渲染上下文决定，拆参数结构体没有收益。
#[allow(clippy::too_many_arguments)]
fn mermaid_block(
    segment: &crate::kit::markdown::MermaidSegment,
    scope: &crate::kit::mermaid::PreviewScope,
    width: usize,
    prefix: usize,
    frozen: bool,
    foreground: ratatui_kit::ratatui::style::Color,
    start: usize,
    schedule: bool,
    surface: crate::kit::mermaid::MermaidSurface,
) -> (Vec<Line<'static>>, crate::kit::mermaid::FenceSpan) {
    let (rendered, layout_bytes) = if schedule {
        let rendered =
            crate::kit::mermaid::display_lines(segment, scope, width, frozen, foreground, surface);
        (rendered, 0)
    } else {
        crate::kit::mermaid::display_lines_peek(segment, scope, width, frozen, foreground)
    };
    let span = crate::kit::mermaid::FenceSpan {
        key: scope.key(segment.fence.open_start),
        fence: Arc::new(segment.fence.clone()),
        width,
        frozen,
        logical: start..start.saturating_add(rendered.lines.len()),
        layout_bytes,
        prefix,
        h_view: rendered.h_view,
    };
    (rendered.lines, span)
}

/// 将单个 TuiRenderUnit 变体转换为渲染行。
/// 使用 kit::markdown 进行 markdown 解析（无缓存，每次全量）。
///
/// 仅测试断言使用（[`render_test`]）——生产路径统一走 [`vm_to_lines_cached`]。
#[cfg(test)]
pub(crate) fn vm_to_lines(vm: &TuiRenderUnit, grid: &GridSpec) -> Vec<Line<'static>> {
    vm_to_lines_cached(
        vm,
        grid,
        &mut crate::kit::markdown::MarkdownRenderCache::default(),
        false,
    )
    .0
}

/// 与 [`vm_to_lines`] 同逻辑，但接受 markdown 渲染缓存以支持增量续跑。
///
/// [Phase 2] 流式期间 text 末尾追加 token 时，前缀 blocks（已闭合的 paragraph /
/// list item / code block）完全不变——cache 通过文本前缀比较复用上次处理到
/// stable_state 的累积状态，仅处理新增 block。
///
/// `md_copy` 为 true 时（顶层 MessageArea），在 AssistantBubble 的
/// markdown 内容末尾追加一行 md 复制按钮，并返回按钮的布局信息（供点击检测）。
/// 嵌套渲染（历史 SubAgentGroup 递归 / subagent 详情面板）传 false。
///
/// 返回四元组 `(lines, copy_button, interaction_layout, image_lines)`——第三项仅 pending
/// 的 interaction block（§6.8）有值（选项行布局，供视口 post-pass 高亮与
/// 点击热区）；第四项含 UserBubble 的 @image meta 行（§4 T4，点击/hover 映射）
/// 与消息区内联图片占位（`ImageLineInfo::inline`，绘制由 MessageArea hook 承接）；
/// 其余变体恒 None / 空 Vec。
///
/// 与 [`vm_to_lines_with_options`] 同逻辑，不带 markdown stable overlay；
/// 转场形态固定 [`ZoneTransition::None`]：本入口服务嵌套渲染（折叠组内成员 /
/// 子 Agent 组内成员）与测试，孤立渲染的 entry 没有相邻 entry 可判定转场（§6）。
pub(crate) fn vm_to_lines_cached(
    vm: &TuiRenderUnit,
    grid: &GridSpec,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
    md_copy: bool,
) -> (
    Vec<Line<'static>>,
    Option<CopyButtonInfo>,
    Option<InteractionLayout>,
    Vec<ImageLineInfo>,
) {
    let (lines, copy_button, interaction, image_lines, _, _, _) = vm_to_lines_with_options(
        vm,
        grid,
        md_cache,
        None,
        EntryRenderOptions::nested(md_copy),
    );
    (lines, copy_button, interaction, image_lines)
}

/// 渲染单个 entry 的唯一入口——主时间线 / 详情面板 / 嵌套成员只差
/// [`EntryRenderOptions`]，不再按 surface 分叉入口壳。
///
/// `md_lines` 为 markdown 稳定块缓存（仅消息区增量路径传入；嵌套/面板传
/// `None`）。转场台阶（§6）由 `options.transition` 决定并统一在末尾插入。
pub(crate) fn vm_to_lines_with_options(
    vm: &TuiRenderUnit,
    grid: &GridSpec,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
    md_lines: Option<&mut crate::kit::message_area::vm_cache::MarkdownLineCache>,
    options: EntryRenderOptions,
) -> RenderOutput {
    vm_to_lines_impl(vm, grid, md_cache, md_lines, options)
}

fn vm_to_lines_impl(
    vm: &TuiRenderUnit,
    grid: &GridSpec,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
    md_lines: Option<&mut crate::kit::message_area::vm_cache::MarkdownLineCache>,
    options: EntryRenderOptions,
) -> RenderOutput {
    let render_copy_button = options.md_copy;
    let schedule_mermaid = options.mermaid == MermaidMode::Schedule;
    let inline_pixels = options.image == ImageMode::Inline;
    let surface = options.surface.mermaid_surface();
    let frame = options.frame;
    let transition = options.transition;

    let mut md_lines = md_lines;
    if let Some(layout) = md_lines.as_deref_mut() {
        layout.genui_ranges.clear();
        layout.genui_blocks.clear();
    }
    let mut genui_buttons = Vec::new();
    let mut fold_hits: Vec<(usize, crate::kit::tui_render_unit::FoldKey)> = Vec::new();
    let rendered = match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut mermaid_fences: Vec<crate::kit::mermaid::FenceSpan> = Vec::new();

            // §3.2 垂直节奏：Wide/Standard 的正文块分隔由转场台阶承担，不再保留
            // 节拍空行；Compact/Narrow 无台阶，正文块上下仍各保留 1 空行（带竖线
            // 前缀，左缘时间线不断链）。短回复并入过程带、与过程 entry 之间不画
            // 台阶，上下也各保留 1 空行（过程带前缀）作分隔。reasoning 是过程
            // entry，不自行增加空行。空 bubble（无 text 无 reasoning）仍返回 0 行。
            if data.text.is_empty() && data.reasoning.is_none() {
                return (
                    lines,
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
            }

            // AI 正文竖线颜色——正文行与前后 turn 节拍空行共用；主题值在
            // guard 生命周期内复制（TUI-THEME-001），不跨渲染路径持有读锁。
            let (md_text_fg, line_color) = {
                let theme_guard = peri_theme::atoms::THEME_ATOM.state();
                let theme = theme_guard.read();
                (theme.component.markdown.text, theme.semantic.accent)
            };

            // 短回复（§6，`TuiAssistantBubble::is_brief`）：正文并入过程带——
            // 行前缀走过程区形态（缩进 + 标记列竖线）、wrap 宽度收窄为
            // `process_content_width`；与相邻过程区不画转场台阶（zone 判定见
            // `transcript::start_zone`）。长回复与 Compact/Narrow（indent == 0，
            // 过程区前缀回退基准形态）走文本区形态。
            let brief = data.is_brief();
            let body_width = if brief {
                grid.process_content_width()
            } else {
                grid.content_width()
            };
            // 围栏行在消息行内的正文前缀宽度；横向条命中从该列起算。
            let mermaid_prefix = if brief {
                grid.process_prefix_width()
            } else {
                grid.cont_prefix_width()
            };
            let mermaid_frozen = data.started_at.is_none();
            let mermaid_scope = crate::kit::mermaid::PreviewScope::for_bubble(
                crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone(),
                data.message_id.as_deref(),
                data.started_at,
                data.content_hash,
            );
            let body_line = |line: Line<'static>| -> Line<'static> {
                if brief {
                    self::helpers::process_prefixed_cont_line(grid, line_color, line)
                } else {
                    self::helpers::prefixed_cont_line(grid, line_color, line)
                }
            };

            // 推理块（§6.3）——视觉独立 entry；入口台阶由本函数统一插入
            if let Some(ref reasoning) = data.reasoning {
                lines.extend(self::reasoning::render_reasoning_block(reasoning, grid));
                // 块尾不加空行（running/completed 一致）——与工具卡片紧凑布局
                // 对齐；正文紧随其后，由 md 渲染自身节奏负责分段。
            }

            // 内部转场（§6）：reasoning（过程区）→ 正文（文本区）的退出台阶由
            // 该 entry 自画——entry 内部事务，无跨 slot 依赖；线色用正文竖线色
            // （accent），与下方正文无缝。纯 reasoning 块（无正文）的退出由后续
            // 文本 entry 承担。短回复正文同属过程区（无 zone 跳变）→ 不画台阶。
            if !brief && data.reasoning.is_some() && !data.text.is_empty() && grid.indent > 0 {
                lines.push(self::helpers::step_line(STEP_EXIT, line_color));
            }

            // 内联图片放置信息（正文图片段命中协议缓存时填充）。
            let mut image_lines: Vec<ImageLineInfo> = Vec::new();

            // Markdown 正文——wrap 在 `body_width`（长回复 content 列宽、短回复
            // 过程带收窄宽度），行级再套统一前缀（`body_line`）。
            if !data.text.is_empty() {
                // 前导空行：Compact/Narrow（无台阶，空行作块分隔）与短回复
                // （过程带内不画台阶，空行承担分隔）保留；Wide/Standard 长回复
                // 由转场台阶分隔。空行带竖线前缀（与正文同色）。
                if grid.indent == 0 || brief {
                    lines.push(body_line(Line::default()));
                }
                let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
                let palette_guard = palette_state.read();
                let rendered = if data.started_at.is_none() {
                    crate::kit::markdown::parse_markdown_terminal(
                        &data.text,
                        body_width,
                        *palette_guard,
                        md_text_fg,
                        md_cache,
                    )
                } else {
                    crate::kit::markdown::parse_markdown_chunks_cached(
                        &data.text,
                        body_width,
                        *palette_guard,
                        md_text_fg,
                        md_cache,
                    )
                };
                debug_assert!(
                    rendered
                        .stable
                        .iter()
                        .all(|chunk| chunk.iter().all(|segment| !matches!(
                            segment,
                            crate::kit::markdown::MarkdownSegment::GenUi(_)
                        ))),
                    "gen-ui must stay in the mutable Markdown tail"
                );
                if let Some(layout) = md_lines.as_deref_mut() {
                    layout.stable_start = Some(lines.len());
                    let stable_lines: Vec<_> = rendered
                        .stable
                        .iter()
                        .enumerate()
                        .map(|(chunk_index, chunk)| {
                            let identity = Arc::as_ptr(chunk) as usize;
                            if layout
                                .stable
                                .get(chunk_index)
                                .is_some_and(|cached| cached.identity == identity)
                            {
                                return (identity, Vec::new(), Vec::new());
                            }
                            let mut chunk_lines = Vec::new();
                            let mut chunk_fences = Vec::new();
                            if chunk_index > 0 {
                                chunk_lines.push(body_line(Line::default()));
                            }
                            for segment in chunk.iter() {
                                if let crate::kit::markdown::MarkdownSegment::Text(seg_lines) =
                                    segment
                                {
                                    chunk_lines.extend(seg_lines.iter().cloned().map(&body_line));
                                } else if let crate::kit::markdown::MarkdownSegment::Mermaid(
                                    segment,
                                ) = segment
                                {
                                    if !chunk_lines.is_empty()
                                        && !chunk_lines.last().is_some_and(is_blank_body_line)
                                    {
                                        chunk_lines.push(body_line(Line::default()));
                                    }
                                    let start = chunk_lines.len();
                                    let (mermaid_lines, span) = mermaid_block(
                                        segment,
                                        &mermaid_scope,
                                        body_width,
                                        mermaid_prefix,
                                        mermaid_frozen,
                                        md_text_fg,
                                        start,
                                        schedule_mermaid,
                                        surface,
                                    );
                                    chunk_fences.push(span);
                                    chunk_lines.extend(mermaid_lines.into_iter().map(&body_line));
                                }
                            }
                            (Arc::as_ptr(chunk) as usize, chunk_lines, chunk_fences)
                        })
                        .collect();
                    layout.retain_and_wrap_with_fences(grid.line_width(), stable_lines);
                    mermaid_fences.extend(layout.stable_fences());
                    for chunk in &layout.stable {
                        lines.resize(
                            lines.len().saturating_add(chunk.lines.len()),
                            Line::default(),
                        );
                    }
                } else {
                    {
                        let iteration_items = rendered.stable.iter().enumerate();
                        for (chunk_index, chunk) in iteration_items {
                            if chunk_index > 0 {
                                lines.push(body_line(Line::default()));
                            }
                            for segment in chunk.iter() {
                                if let crate::kit::markdown::MarkdownSegment::Text(seg_lines) =
                                    segment
                                {
                                    lines.extend(seg_lines.iter().cloned().map(&body_line));
                                } else if let crate::kit::markdown::MarkdownSegment::Mermaid(
                                    segment,
                                ) = segment
                                {
                                    if !lines.is_empty()
                                        && !lines.last().is_some_and(is_blank_body_line)
                                    {
                                        lines.push(body_line(Line::default()));
                                    }
                                    let start = lines.len();
                                    let (mermaid_lines, span) = mermaid_block(
                                        segment,
                                        &mermaid_scope,
                                        body_width,
                                        mermaid_prefix,
                                        mermaid_frozen,
                                        md_text_fg,
                                        start,
                                        schedule_mermaid,
                                        surface,
                                    );
                                    mermaid_fences.push(span);
                                    lines.extend(mermaid_lines.into_iter().map(&body_line));
                                }
                            }
                        }
                    }
                }
                let stable_segment_count: usize =
                    rendered.stable.iter().map(|chunk| chunk.len()).sum();
                let mut prev_seg: Option<&crate::kit::markdown::MarkdownSegment> = None;
                {
                    let iteration_items = rendered.tail.iter().enumerate();
                    for (tail_index, seg) in iteration_items {
                        let seg_idx = stable_segment_count + tail_index;
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
                            lines.push(body_line(Line::default()));
                        }
                        match seg {
                            crate::kit::markdown::MarkdownSegment::Text(seg_lines) => {
                                for line in seg_lines {
                                    lines.push(body_line(line.clone()));
                                }
                            }
                            crate::kit::markdown::MarkdownSegment::Image(img) => {
                                // 本地独占段落图片：逐图只查协议缓存（render body 不做
                                // IO）。命中时按真实编码尺寸预留 rows 行空白占位并把
                                // 放置信息交给消息区 draw hook 画像素；未命中/非独占
                                // 段落保留各自的降级行——同段多图合并时按首图身份整段
                                // 替换会丢掉其余图片的文本。远程/危险来源由查表侧拒绝。
                                // Detail 面板等没有 draw hook 的 surface（inline_pixels
                                // = false）保持文本，避免留下无法填充的空白占位。
                                if !img.standalone || !inline_pixels {
                                    for line in &img.lines {
                                        lines.push(body_line(line.clone()));
                                    }
                                } else {
                                    let cols = u16::try_from(body_width).unwrap_or(u16::MAX);
                                    let x_offset = u16::try_from(if brief {
                                        grid.process_prefix_width()
                                    } else {
                                        grid.cont_prefix_width()
                                    })
                                    .unwrap_or(u16::MAX);
                                    for source in &img.sources {
                                        let ready = crate::kit::inline_image::lookup_ready(
                                            &source.source,
                                            cols,
                                            crate::kit::inline_image::MAX_ROWS,
                                        );
                                        let fallback: &[Line<'static>] = img
                                            .lines
                                            .get(source.line_range.clone())
                                            .map_or(&[], |slice| slice);
                                        if let Some(ready) = ready {
                                            let logical_idx = lines.len();
                                            for _ in 0..ready.rows {
                                                lines.push(body_line(Line::default()));
                                            }
                                            image_lines.push(ImageLineInfo {
                                                logical_idx,
                                                path: String::new(),
                                                managed: false,
                                                size_text: String::new(),
                                                inline: Some(InlineImagePlacement {
                                                    cols: ready.cols,
                                                    rows: ready.rows,
                                                    x_offset,
                                                    image: ready.image,
                                                }),
                                            });
                                        } else {
                                            for line in fallback {
                                                lines.push(body_line(line.clone()));
                                            }
                                        }
                                    }
                                }
                            }
                            crate::kit::markdown::MarkdownSegment::Mermaid(segment) => {
                                // 帧可异步到达；渲染只读当前帧，调度由可见性决定。
                                let start = lines.len();
                                let (mermaid_lines, span) = mermaid_block(
                                    segment,
                                    &mermaid_scope,
                                    body_width,
                                    mermaid_prefix,
                                    mermaid_frozen,
                                    md_text_fg,
                                    start,
                                    schedule_mermaid,
                                    surface,
                                );
                                mermaid_fences.push(span);
                                for line in mermaid_lines {
                                    lines.push(body_line(line));
                                }
                            }
                            crate::kit::markdown::MarkdownSegment::GenUi(segment) => {
                                let theme_state = THEME_ATOM.state();
                                let theme = theme_state.read();
                                let mut rendered =
                                    crate::kit::genui::render(segment, body_width, &theme);
                                let start = lines.len();
                                let prefix = if brief {
                                    grid.process_prefix_width()
                                } else {
                                    grid.cont_prefix_width()
                                }
                                .min(u16::MAX as usize)
                                    as u16;
                                for button in &mut rendered.buttons {
                                    button.line_start = button.line_start.saturating_add(start);
                                    button.line_end = button.line_end.saturating_add(start);
                                    button.x_start = button.x_start.saturating_add(prefix);
                                    button.x_end = button.x_end.saturating_add(prefix);
                                }
                                lines.extend(rendered.lines.into_iter().map(&body_line));
                                genui_buttons.extend(rendered.buttons);
                                if let Some(layout) = md_lines.as_deref_mut() {
                                    layout.genui_ranges.push(start..lines.len());
                                    layout.genui_blocks.push(segment.block_id());
                                }
                            }
                            crate::kit::markdown::MarkdownSegment::Table(data) => {
                                let mut table_theme =
                                    ratatui_kit::components::TableTheme::from_palette(
                                        &palette_guard,
                                    );
                                // 行文字使用终端默认色，而非纯白
                                table_theme.row_style = Style::default();
                                let table_lines = crate::kit::markdown::table_data_to_lines(
                                    data,
                                    &table_theme,
                                    body_width,
                                );
                                for tl in table_lines {
                                    lines.push(body_line(tl));
                                }
                            }
                        }
                        prev_seg = Some(seg);
                    }
                }
            }

            // §6.2 完成时长 meta（G-Tokens 仅 duration）：冻结值在正文末行尾部
            // 三档放置（Wide 右对齐 / Standard 紧跟 / Compact/Narrow 隐藏），
            // 不独占一行。空正文（纯 reasoning 块）无放置点 → 跳过；亚秒极快
            // （< 50ms）无值可显示 → 跳过。
            if let Some(meta) = data.duration_ms.and_then(format_completed_duration) {
                let sem = THEME_ATOM.state().read().semantic;
                let meta_style = Style::default().fg(sem.text.dim);
                if let Some(last_non_empty) = lines.iter_mut().rev().find(|l| !l.spans.is_empty()) {
                    let used: usize = last_non_empty.spans.iter().map(|s| s.content.width()).sum();
                    if let Some(meta_spans) =
                        self::helpers::place_meta(grid, used, &meta, meta_style)
                    {
                        last_non_empty.spans.extend(meta_spans);
                    }
                }
            }

            // md 复制按钮：独立于 markdown 渲染，追加在内容末尾。
            // 仅当内容超过 MD_COPY_MIN_CHARS 字符（chars().count() 口径）才渲染。
            let copy_button = if render_copy_button && data.text.chars().count() > MD_COPY_MIN_CHARS
            {
                if let Some((btn_line, x_start, x_end)) = copy_button_line(grid, line_color) {
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

            if !data.text.is_empty() && (grid.indent == 0 || brief) {
                // 尾随空行：Compact/Narrow 与短回复保留——turn 节拍对称且左缘
                // 时间线不断链（Wide/Standard 长回复由转场台阶承担分隔）。
                lines.push(body_line(Line::default()));
            }

            (lines, copy_button, None, image_lines, mermaid_fences)
        }
        TuiRenderUnit::TuiUserBubble(data) => {
            if let Some(ref info) = data.reminder {
                // 折叠提醒（单行）仍参与入口台阶：不早返回，交给统一包装。
                (
                    self::user::render_reminder_condensed(info, grid),
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                )
            } else {
                let (lines, image_lines) =
                    self::user::render_user_bubble_lines(data, grid, inline_pixels);
                (lines, None, None, image_lines, Vec::new())
            }
        }
        TuiRenderUnit::TuiToolCard(data) => (
            self::tool_card::render_tool_card_lines(data, grid),
            data.is_error.then(|| error::icon_button(grid, 0)),
            None,
            Vec::new(),
            Vec::new(),
        ),
        TuiRenderUnit::TuiSystemNote(data) => (
            self::system::render_system_note_lines(data, grid),
            (data.level == crate::kit::tui_render_unit::TuiNoteLevel::Error)
                .then(|| error::icon_button(grid, 0)),
            None,
            Vec::new(),
            Vec::new(),
        ),
        TuiRenderUnit::TuiSystemReminder(data) => (
            self::user::render_system_reminder_lines(data, grid),
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
        TuiRenderUnit::TuiSubAgentGroup(data) => (
            self::group::render_subagent_group_lines(data, grid),
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            let (lines, hits) = self::group::render_collapsed_group_lines(data, grid, frame);
            fold_hits = hits;
            (lines, None, None, Vec::new(), Vec::new())
        }
        TuiRenderUnit::TuiDivider(data) => (
            self::system::render_divider_lines(data, grid),
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
        TuiRenderUnit::TuiAskUserBlock(data) => {
            let (lines, layout) = self::interaction::render_ask_user_block_lines(data, grid);
            (lines, None, layout, Vec::new(), Vec::new())
        }
        TuiRenderUnit::TuiTodoSummary(data) => (
            self::system::render_todo_summary_lines(data, grid),
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
    };

    // 入口台阶（§6）：由转场后的首个 entry 在行首插入（Compact/Narrow 无缩进
    // 即无台阶，§9）；0 行 entry（空 bubble / 空 prompt）不产生台阶。台阶行
    // 使后续行索引 +1——copy / interaction / image 的 logical_idx 与 markdown
    // stable overlay 起点同步顺延（台阶行由本函数插入，各 renderer 产出的
    // 索引以自身行序为基准）。
    let (mut lines, mut copy_button, mut interaction, mut image_lines, mut mermaid_fences) =
        rendered;
    if grid.indent > 0 && !lines.is_empty() {
        let step = match transition {
            ZoneTransition::EnterProcess(source) => Some(self::helpers::step_line(
                STEP_ENTER,
                self::helpers::text_line_color(source),
            )),
            ZoneTransition::ExitProcess(source) => Some(self::helpers::step_line(
                STEP_EXIT,
                self::helpers::text_line_color(source),
            )),
            ZoneTransition::None => None,
        };
        if let Some(step) = step {
            lines.insert(0, step);
            if let Some(button) = copy_button.as_mut() {
                button.logical_idx += 1;
            }
            if let Some(layout) = interaction.as_mut() {
                for row in &mut layout.option_rows {
                    *row += 1;
                }
            }
            for info in image_lines.iter_mut() {
                info.logical_idx += 1;
            }
            for fence in &mut mermaid_fences {
                fence.logical =
                    fence.logical.start.saturating_add(1)..fence.logical.end.saturating_add(1);
            }
            for button in &mut genui_buttons {
                button.line_start = button.line_start.saturating_add(1);
                button.line_end = button.line_end.saturating_add(1);
            }
            for (row, _) in &mut fold_hits {
                *row = row.saturating_add(1);
            }
            if let Some(layout) = md_lines {
                if let Some(start) = layout.stable_start.as_mut() {
                    *start += 1;
                }
                for range in &mut layout.genui_ranges {
                    range.start = range.start.saturating_add(1);
                    range.end = range.end.saturating_add(1);
                }
            }
        }
    }
    (
        lines,
        copy_button,
        interaction,
        image_lines,
        genui_buttons,
        mermaid_fences,
        fold_hits,
    )
}

/// running reasoning header 在 entry lines 中的索引：入口台阶（§6）占 lines[0]
/// 时 running header 顺延到 lines[1]。Animation 原位刷新（`EntryRenderCache`）
/// 据此定位——按台阶字形判定，替换目标不因转场形态漂移；无行时返回 None。
pub(crate) fn running_header_index(lines: &[Line<'static>]) -> Option<usize> {
    let first = lines.first()?;
    Some(usize::from(helpers::is_step_line(first)))
}

/// 台阶行判定（§6 ` ╰─╮` / ` ╭─╯`）——语义复制在提取层整行跳过纯结构 chrome
/// （`selection.rs`），复制文本里不出现台阶字形、也不为其保留空行。
#[must_use]
pub(crate) fn is_step_row(line: &Line<'static>) -> bool {
    helpers::is_step_line(line)
}
