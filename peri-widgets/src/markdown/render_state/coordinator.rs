// coordinator.rs —— RenderState 渲染协调器
//
// 集中处理 pulldown-cmark 的 Event 路由：
//   - 字段定义 + new/with_max_width builder
//   - flush_line / push_span / ensure_blank_line 低级 spans 操作
//   - handle_event 顶层 match（按 Event 类型分发到字段级状态变更 + 主题着色）
//
// [生命周期] theme: &'a dyn MarkdownTheme 的生命周期参数必须挂在 RenderState 上，
//            不能下放到 table/ 子树独立持有（子模块通过 &self.theme 临时借用即可），
//            避免生命周期污染扩散到 table/ 子树。
// [不变量] 代码块缓冲语义：code_lines 在 Start(CodeBlock) 清空、Text 事件累积、
//          End(CodeBlock) 统一输出。禁止改为流式输出——
//          highlight_code_block 需要完整内容才能高亮。
// [不变量] 空行去重：ensure_blank_line 依赖 lines.last()，跨多个事件分支调用。
//          必须保留为 RenderState 方法，子模块通过 &mut self 调用，
//          不能改为各分支自行判断（会丢失去重语义）。
// [不变量] inline_style patch 顺序：push_span 用 self.inline_style.patch(extra)，
//          inline_style 必须是 RenderState 字段（被 Start/End 事件修改 + 被 Text 事件读取）。
// [feature flag] markdown-highlight 的 #[cfg]/#[cfg(not)] 双分支必须整体迁移，
//          禁止拆散到两个文件，否则 cfg 作用域错乱。

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::link::wrap_osc8;
#[cfg(feature = "markdown-highlight")]
use crate::markdown::highlight::highlight_code_block;
use crate::markdown::MarkdownTheme;

use super::table::TableBuilder;

// ── 列表类型与状态 ─────────────────────────────────────────────────────────
//
// ListType / ListState 仅服务于 RenderState.list_stack（嵌套列表缩进 + 有序编号递增）。
// 原在独立 list.rs 文件中，因仅 15 行且仅被本文件使用，内联至此减少导航成本。
// [TRAP] 拆出后不可再独立成文件，除非同时有其他模块引用（当前无）。

#[derive(Debug, Clone)]
pub(in crate::markdown) enum ListType {
    Ordered(u64),
    Unordered,
}

#[derive(Debug, Clone)]
pub(in crate::markdown) struct ListState {
    pub(super) list_type: ListType,
}

pub(in crate::markdown) struct RenderState<'a> {
    pub lines: Vec<Line<'static>>,
    pub current_spans: Vec<Span<'static>>,
    pub inline_style: Style,
    pub list_stack: Vec<ListState>,
    pub quote_depth: u32,
    pub in_code_block: bool,
    pub code_block_lang: String,
    /// 缓冲多行代码块的所有行，在 TagEnd::CodeBlock 时统一输出
    code_lines: Vec<String>,
    table: Option<TableBuilder>,
    theme: &'a dyn MarkdownTheme,
    max_width: usize,
    pending_link_url: Option<String>, // 当前正在处理的 link URL
}

impl<'a> RenderState<'a> {
    pub fn new(theme: &'a dyn MarkdownTheme) -> Self {
        Self {
            lines: Vec::new(),
            current_spans: Vec::new(),
            inline_style: Style::default(),
            list_stack: Vec::new(),
            quote_depth: 0,
            in_code_block: false,
            code_block_lang: String::new(),
            code_lines: Vec::new(),
            table: None,
            theme,
            max_width: 80, // 默认宽度
            pending_link_url: None,
        }
    }

    pub fn with_max_width(mut self, width: usize) -> Self {
        self.max_width = width;
        self
    }

    pub fn flush_line(&mut self) {
        let mut spans = std::mem::take(&mut self.current_spans);

        if self.quote_depth > 0 && !spans.is_empty() {
            let prefix = "▍ ".repeat(self.quote_depth as usize);
            spans.insert(
                0,
                Span::styled(prefix, Style::default().fg(self.theme.quote_prefix())),
            );
        }

        if spans.is_empty() {
            self.lines.push(Line::default());
        } else {
            self.lines.push(Line::from(spans));
        }
    }

    /// 确保与上一个输出行之间有一个空行（去重：如果上一行已是空行则跳过）
    fn ensure_blank_line(&mut self) {
        if !self.lines.is_empty() && !self.lines.last().unwrap().spans.is_empty() {
            self.lines.push(Line::default());
        }
    }

    pub fn push_span(&mut self, text: String, extra: Style) {
        let style = self.inline_style.patch(extra);
        let text = if let Some(ref url) = self.pending_link_url {
            wrap_osc8(&text, url)
        } else {
            text
        };
        self.current_spans.push(Span::styled(text, style));
    }

    pub fn handle_event(&mut self, event: Event<'_>) {
        match event {
            // ── 标题 ─────────────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                let color = match level {
                    HeadingLevel::H1 | HeadingLevel::H2 | HeadingLevel::H3 => self.theme.heading(),
                    _ => self.theme.muted(),
                };
                self.inline_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                // 标题前添加空行分隔（去重）
                self.flush_line();
                self.ensure_blank_line();
            }
            Event::End(TagEnd::Heading(_)) => {
                self.inline_style = Style::default();
                self.flush_line();
                // 标题后添加空行分隔（去重）
                self.ensure_blank_line();
            }

            // ── 段落 ─────────────────────────────────────────────────────────
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                self.flush_line();
                self.ensure_blank_line();
            }

            // ── 代码块 ────────────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                // 先清理前一段落可能残留的 spans，避免首行粘黏
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                self.in_code_block = true;
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_lines.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                let lines = std::mem::take(&mut self.code_lines);

                // 过滤尾部空行
                let mut end = lines.len();
                while end > 0 && lines[end - 1].is_empty() {
                    end -= 1;
                }

                if end == 1 {
                    // 单行代码块：只改颜色，不要 [lang] 和 │ 前缀，简洁
                    self.current_spans.push(Span::styled(
                        lines[0].clone(),
                        Style::default().fg(self.theme.code()),
                    ));
                    self.flush_line();
                } else if end > 1 {
                    // 多行代码块：前后各加一个空行（去重）
                    self.ensure_blank_line();

                    #[cfg(feature = "markdown-highlight")]
                    if let Some(highlighted) =
                        highlight_code_block(&self.code_block_lang, &lines[..end])
                    {
                        self.lines.extend(highlighted);
                    } else {
                        // syntect 未识别语言，回退到统一颜色
                        for line_text in &lines[..end] {
                            self.current_spans.push(Span::styled(
                                line_text.clone(),
                                Style::default().fg(self.theme.text()),
                            ));
                            self.flush_line();
                        }
                    }

                    #[cfg(not(feature = "markdown-highlight"))]
                    for line_text in &lines[..end] {
                        self.current_spans.push(Span::styled(
                            line_text.clone(),
                            Style::default().fg(self.theme.text()),
                        ));
                        self.flush_line();
                    }

                    self.ensure_blank_line();
                }
            }

            // ── 列表 ─────────────────────────────────────────────────────────
            Event::Start(Tag::List(start)) => {
                let list_type = match start {
                    Some(n) => ListType::Ordered(n),
                    None => ListType::Unordered,
                };
                self.list_stack.push(ListState { list_type });
                // 列表整体前加空行（去重，仅最外层列表加）
                if self.list_stack.len() == 1 {
                    self.ensure_blank_line();
                }
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
                // 列表整体后加空行（去重，仅最外层列表加）
                if self.list_stack.is_empty() {
                    self.ensure_blank_line();
                }
            }
            Event::Start(Tag::Item) => {
                let depth = self.list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let bullet = if let Some(state) = self.list_stack.last_mut() {
                    match &mut state.list_type {
                        ListType::Unordered => format!("{}• ", indent),
                        ListType::Ordered(n) => {
                            let s = format!("{}{}. ", indent, n);
                            *n += 1;
                            s
                        }
                    }
                } else {
                    format!("{}• ", indent)
                };
                self.current_spans.push(Span::styled(
                    bullet,
                    Style::default().fg(self.theme.list_bullet()),
                ));
            }
            Event::End(TagEnd::Item) if !self.current_spans.is_empty() => {
                self.flush_line();
            }

            // ── 引用块 ────────────────────────────────────────────────────────
            Event::Start(Tag::BlockQuote(_)) => {
                self.quote_depth += 1;
                // 引用块前加空行（去重，仅最外层加）
                if self.quote_depth == 1 {
                    self.ensure_blank_line();
                }
            }
            Event::End(TagEnd::BlockQuote(_)) if self.quote_depth > 0 => {
                self.quote_depth -= 1;
                // 引用块后加空行（去重，仅最外层加）
                if self.quote_depth == 0 {
                    self.ensure_blank_line();
                }
            }

            // ── 水平线 ────────────────────────────────────────────────────────
            Event::Rule => {
                self.ensure_blank_line();
                let rule = "─".repeat(60);
                self.current_spans.push(Span::styled(
                    rule,
                    Style::default().fg(self.theme.separator()),
                ));
                self.flush_line();
                self.ensure_blank_line();
            }

            // ── 文本（含代码块内容） ───────────────────────────────────────────
            Event::Text(text) => {
                let text_str = text.into_string();
                if self.in_code_block {
                    // 缓冲所有行，等 TagEnd::CodeBlock 时统一输出
                    for line in text_str.split('\n') {
                        self.code_lines.push(line.to_string());
                    }
                } else if self.table.is_some() {
                    let style = self.inline_style;
                    let text_str = if let Some(ref url) = self.pending_link_url {
                        wrap_osc8(&text_str, url)
                    } else {
                        text_str
                    };
                    self.table
                        .as_mut()
                        .unwrap()
                        .current_cell
                        .push(Span::styled(text_str, style));
                } else {
                    self.push_span(text_str, Style::default());
                }
            }

            // ── 行内代码 ──────────────────────────────────────────────────────
            Event::Code(text) => {
                let style = Style::default().fg(self.theme.code());
                let span = Span::styled(text.into_string(), style);
                if let Some(table) = &mut self.table {
                    table.current_cell.push(span);
                } else {
                    self.current_spans.push(span);
                }
            }

            // ── Strong / Emphasis / Strikethrough ────────────────────────────
            Event::Start(Tag::Strong) => {
                self.inline_style = self.inline_style.add_modifier(Modifier::BOLD);
            }
            Event::End(TagEnd::Strong) => {
                self.inline_style = self.inline_style.remove_modifier(Modifier::BOLD);
            }
            Event::Start(Tag::Emphasis) => {
                self.inline_style = self.inline_style.add_modifier(Modifier::ITALIC);
            }
            Event::End(TagEnd::Emphasis) => {
                self.inline_style = self.inline_style.remove_modifier(Modifier::ITALIC);
            }
            Event::Start(Tag::Strikethrough) => {
                self.inline_style = self.inline_style.add_modifier(Modifier::CROSSED_OUT);
            }
            Event::End(TagEnd::Strikethrough) => {
                self.inline_style = self.inline_style.remove_modifier(Modifier::CROSSED_OUT);
            }

            // ── 链接 ─────────────────────────────────────────────────────────
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.pending_link_url = Some(dest_url.to_string());
                self.inline_style = self
                    .inline_style
                    .fg(self.theme.link())
                    .add_modifier(Modifier::UNDERLINED);
            }
            Event::End(TagEnd::Link) => {
                self.inline_style = Style::default();
                self.pending_link_url = None;
            }

            // ── 表格 ─────────────────────────────────────────────────────────
            Event::Start(Tag::Table(alignments)) => {
                self.table = Some(TableBuilder::new(alignments));
            }
            Event::End(TagEnd::Table) => {
                if let Some(tb) = self.table.take() {
                    let table_lines = tb.render_with_wrap(self.max_width, self.theme);
                    self.lines.extend(table_lines);
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(tb) = self.table.as_mut() {
                    tb.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(tb) = self.table.as_mut() {
                    tb.push_cell();
                    tb.push_row();
                    tb.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableRow) => {
                if let Some(tb) = self.table.as_mut() {
                    tb.push_cell();
                    tb.push_row();
                }
            }
            Event::Start(Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => {
                if let Some(tb) = self.table.as_mut() {
                    tb.push_cell();
                }
            }

            // ── 换行 ─────────────────────────────────────────────────────────
            Event::SoftBreak => {
                self.push_span(" ".to_string(), Style::default());
            }
            Event::HardBreak => {
                self.flush_line();
            }

            // ── HTML 块 / 行内 HTML ──────────────────────────────────────────
            //
            // pulldown-cmark 启用 ENABLE_HTML 后，`<system-reminder>...</system-reminder>`
            // 等自定义标签被解析为 HTML 块（Event::Html）或行内 HTML（Event::InlineHtml）。
            // 整个块（包括内部文本）以 Event::Html 事件到达。
            //
            // [BUGFIX] 之前 `_ => {}` 静默丢弃这些事件，导致 goal steering、连续失败
            // 警告等 `<system-reminder>` 包裹的消息在 TUI 中渲染为空白——用户看到
            // "消息消失"。现在剥离 HTML 标签后渲染内部文本。
            Event::Html(html) | Event::InlineHtml(html) => {
                let raw = html.into_string();
                let stripped = strip_html_tags(&raw);
                if !stripped.trim().is_empty() {
                    for line in stripped.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            self.push_span(trimmed.to_string(), Style::default());
                            self.flush_line();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

/// 剥离 HTML 标签，保留标签之间的文本内容。
///
/// 例：`"<system-reminder>\nHello\n</system-reminder>"` → `"\nHello\n"`
///
/// 简单状态机：在 `>` 和 `<` 之间的内容为可见文本。
fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}
