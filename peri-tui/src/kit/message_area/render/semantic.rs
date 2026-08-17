use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{FoldState, TuiRenderUnit, TuiToolCard};
use unicode_width::UnicodeWidthStr;

use super::group::{SUBAGENT_TOOL_INDENT, SUBAGENT_TOOL_LINES};
use super::tool_card::completed_header_suffix;

/// §9 语义复制（D3）：按 VM 变体从渲染行提取「语义文本」——复制内容而非
/// 屏幕像素。供 `extract_visual_range`（selection.rs）在复制时调用（事件时点，
/// 非渲染路径）；md 复制按钮路径（复制原始 markdown）不动。
///
/// 传入**已渲染行**（复制路径持有 VmCacheSlot 的 Arc<Vec<Line>>）而非重新
/// 渲染——旧实现每行新建 MarkdownRenderCache 全量重渲染 VM（N 行选区 = N 次
/// 全量 markdown 解析，§15 线性度违背）；渲染行与缓存行同源，结果等价。
///
/// 变体分派：
/// - 普通行（user/reasoning/assistant md/tool 输出/system/divider 等）：剥离
///   `outer + accent + gap` 前缀列（§3.1 网格）——`line_to_plain_text` 无符号；
/// - tool header 行：`{Verb} {summary}{suffix}`（label + summary + 完成后缀，
///   无符号、无 duration——§9「Read header 复制 path，Bash header 复制
///   command」）；
/// - Bash 展开 `$ cmd` 行：保留 `$ {command}`（§9）；
/// - diff 行：剥离行号 gutter 列，保留 `+`/`-` patch 标记与正文（§9）；
/// - code block 行：再剥离 `│ ` gutter（现状无语言标签行/行号——§9 已确认）。
///
/// 未命中变体回退前缀剥离结果（保留现有语义）。
pub(crate) fn semantic_line_text(
    vm: &TuiRenderUnit,
    local_idx: usize,
    line: &ratatui_kit::ratatui::text::Line<'static>,
    grid: &GridSpec,
) -> Option<String> {
    let plain = crate::kit::text_selection::line_to_plain_text(line);
    let stripped = strip_visual_prefix(line, &plain, grid, local_idx);
    match vm {
        TuiRenderUnit::TuiToolCard(card) => {
            if local_idx == 0 {
                // header 行：label + summary + suffix（§9）。
                return Some(tool_header_semantic(card));
            }
            // Bash 展开 `$ cmd` 行（§9 保留 command）。
            if let Some(rest) = stripped.strip_prefix("$ ") {
                return Some(format!("$ {rest}"));
            }
            // diff 行：数字行号列 + 符号 → 剥行号保留 patch 标记（§9）。
            if let Some(sem) = strip_diff_gutter(&stripped) {
                return Some(sem);
            }
            Some(stripped)
        }
        TuiRenderUnit::TuiAssistantBubble(_) => {
            // code block 行：再剥 `│ ` gutter（语言标签/行号现状无——§9）。
            if let Some(rest) = stripped.strip_prefix("\u{2502} ") {
                return Some(rest.to_string());
            }
            Some(stripped)
        }
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            // 子工具行/原因行是 cont_prefix + 2 格缩进形态（设计文档 §3）——
            // `[outer 空][│][gap][2 空格][符号] {Verb}  {summary}` /
            // `[outer 空][│][gap][2 空格]{错误正文}`；strip_visual_prefix 已剥
            // `[outer][│][gap]`，剩余部分带缩进。组只渲染工具行/原因行，**没有
            // 顶层组头**（render_subagent_group_lines 无工具时整组留空），行序：
            // 最近工具最新在前 → 原因行。
            let recent_tool_lines = data
                .view_models
                .iter()
                .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
                .count()
                .min(SUBAGENT_TOOL_LINES);
            // 工具行索引 [0, recent_tool_lines)：渲染行序 = view_models 反向工具序，
            // 所以第 local_idx 行对应反向第 local_idx 个工具（nth(local_idx)）。
            if local_idx < recent_tool_lines {
                // 工具行语义直接由 VM 数据构造 `{Verb} {summary}`（§8 与主时间线
                // tool header 同口径，无符号/duration/缩进竖线）——不解析符号
                // 字符集（ASCII 降级 `x`/`*` 与正文歧义），且与渲染截断无关。
                let tool = data
                    .view_models
                    .iter()
                    .rev()
                    .filter_map(|vm| match vm {
                        TuiRenderUnit::TuiToolCard(t) => Some(t),
                        _ => None,
                    })
                    .nth(local_idx);
                if let Some(tool) = tool {
                    let mut text = crate::kit::tool_display::format_tool_name(&tool.tool_name);
                    if !tool.input_summary.is_empty() {
                        text.push(' ');
                        text.push_str(&tool.input_summary);
                    }
                    return Some(text);
                }
            }
            // 原因行：剥 2 格缩进 → 纯错误正文（§8）。
            Some(
                stripped
                    .strip_prefix(&" ".repeat(SUBAGENT_TOOL_INDENT))
                    .unwrap_or(&stripped)
                    .to_string(),
            )
        }
        _ => Some(stripped),
    }
}

/// §9 tool header 行语义：`{Verb} {summary}{suffix}`——无符号、无 duration。
/// Bash 展开态 summary 移到 `$ ` 行（render_generic_tool_card_lines 口径），
/// header 语义只留 label；suffix 复用 `completed_header_suffix`（Read `— N
/// lines` / Glob/Grep `— N matches` / Edit/Write `· +N −M`——§6.4 口径，
/// Edit/Write 不重复输出含路径的摘要文本）。
fn tool_header_semantic(data: &TuiToolCard) -> String {
    let mut text = crate::kit::tool_display::format_tool_name(&data.tool_name);
    let bash_expanded = data.tool_name == "Bash"
        && !data.is_running
        && !data.is_error
        && data.fold == FoldState::Expanded;
    if !bash_expanded && !data.input_summary.is_empty() {
        text.push(' ');
        text.push_str(&data.input_summary);
    }
    let suffix = completed_header_suffix(data);
    if !suffix.is_empty() {
        text.push_str(&suffix);
    }
    text
}

/// 从渲染行剥离网格前缀列（§3.1：`outer + accent + gap`），返回内容列文本。
///
/// 前缀由行首 spans 结构确认（渲染层约定：首行 `[outer " ", 符号, gap " "]`
/// = `first_prefix`，续行 `[outer " ", │, gap " "]` = `cont_prefix`）；
/// 结构不符（无前缀行，如 md 复制按钮行的前导空格）不剥离——兜底保留原样。
pub(super) fn strip_visual_prefix(
    line: &ratatui_kit::ratatui::text::Line<'static>,
    plain: &str,
    grid: &GridSpec,
    local_idx: usize,
) -> String {
    if plain.is_empty() {
        return String::new();
    }
    let expect = if local_idx == 0 {
        grid.first_prefix_width()
    } else {
        grid.cont_prefix_width()
    };
    let mut w = 0usize;
    let mut byte_skip = 0usize;
    let mut hit = false;
    for span in &line.spans {
        let sw = span.content.width();
        if sw == 0 {
            byte_skip += span.content.len();
            continue;
        }
        if w + sw > expect {
            break; // 结构不符（前缀在 expect 内断掉）——视为无前缀行
        }
        w += sw;
        byte_skip += span.content.len();
        if w == expect {
            hit = true;
            break;
        }
    }
    if hit {
        plain[byte_skip..].to_string()
    } else {
        plain.to_string()
    }
}

/// §9 diff 行语义：`{行号列} {符号} {正文}` → `{符号} {正文}`（剥行号 gutter，
/// 保留 `+`/`-` patch 标记）；context 行（符号为空格）→ 纯正文。
///
/// 模式 `^\s*\d+ [+ -] `（gutter 右对齐可能含前导空格；符号位后必须跟空格）
/// ——普通输出行（如 `42  foo`）不匹配（符号后无空格）→ `None` 回退原样。
fn strip_diff_gutter(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    // 前导空格（gutter 右对齐填充）
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    // 数字行号
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    // 分隔空格（恰好一个）
    if bytes.get(i) != Some(&b' ') {
        return None;
    }
    let sym_i = i + 1;
    let sym = *bytes.get(sym_i)?;
    if !matches!(sym, b'+' | b'-' | b' ') {
        return None;
    }
    if bytes.get(sym_i + 1) != Some(&b' ') {
        return None;
    }
    let body = &text[sym_i + 2..];
    match sym {
        b'+' => Some(format!("+ {body}")),
        b'-' => Some(format!("- {body}")),
        _ => Some(body.to_string()),
    }
}
