use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiToolCard};
use unicode_width::UnicodeWidthStr;

use super::group::SUBAGENT_TOOL_LINES;
use super::helpers::is_step_line;
use super::tool_card::projected_tool_header_text;

/// §9 语义复制（D3）：按 VM 变体从渲染行提取「语义文本」——复制内容而非
/// 屏幕像素。供 `extract_visual_range`（selection.rs）在复制时调用（事件时点，
/// 非渲染路径）；md 复制按钮路径（复制原始 markdown）不动。
///
/// 传入**已渲染行**（复制路径持有 VmCacheSlot 的 Arc<Vec<Line>>）而非重新
/// 渲染——旧实现每行新建 MarkdownRenderCache 全量重渲染 VM（N 行选区 = N 次
/// 全量 markdown 解析，§15 线性度违背）；渲染行与缓存行同源，结果等价。
///
/// 变体分派：
/// - 转场台阶行（§6 ` ╰─╮` / ` ╭─╯`）：纯结构 chrome，无用户内容 → 空语义
///   （提取层 [`super::is_step_row`] 对该行整行跳过，不进入复制文本）；
/// - 普通行（user/reasoning/assistant md/tool 输出/system/divider 等）：剥离行首
///   网格前缀——文本区 `outer + accent + gap`，过程区 `outer + indent 留空 +
///   标记 + gap`（标记为状态符号或 `│`，§3/§4）；`line_to_plain_text` 无符号；
/// - tool header 行：`{Verb} {summary}{suffix}`（label + summary + 完成后缀，
///   无符号、无 duration——§9「Read header 复制 path，Bash header 复制
///   command」）；
/// - Bash 展开 `$ cmd` 行：保留 `$ {command}`（§9）；
/// - diff 行：剥离行号 gutter 列，保留 `+`/`-` patch 标记与正文（§9）；
/// - code block 行：再剥离 `│ ` gutter（现状无语言标签行/行号——§9 已确认）。
///
/// [`local_idx`] 按「VM 自身行序」计（不含 render 层在 slot 行首插入的入口台阶
/// 行）——提取层已按台阶占位顺延，变体分派（tool header / 子 agent 工具行）据此
/// 定位；未命中变体回退前缀剥离结果（保留现有语义）。
pub(crate) fn semantic_line_text(
    vm: &TuiRenderUnit,
    local_idx: usize,
    line: &ratatui_kit::ratatui::text::Line<'static>,
    grid: &GridSpec,
) -> Option<String> {
    if is_step_line(line) {
        return Some(String::new());
    }
    let plain = crate::kit::text_selection::line_to_plain_text(line);
    let stripped = strip_visual_prefix(line, &plain, grid, local_idx);
    match vm {
        TuiRenderUnit::TuiToolCard(card) => {
            if local_idx == 0 {
                // header 行：label + summary + suffix（§9）。
                return Some(tool_header_semantic(card, grid));
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
            // code block 行：剥 convert 层为铺满背景追加的独立尾 padding span，
            // 再剥可能保留的 `│ ` gutter。代码正文 span 内的尾随空格必须保留。
            let stripped = strip_code_block_visual_padding(line, stripped);
            if let Some(rest) = stripped.strip_prefix("\u{2502} ") {
                return Some(rest.to_string());
            }
            Some(stripped)
        }
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            // 组内工具行/原因行与组头同列（§5 合并为总 `indent`）——`[outer 空]
            // [indent 留空][│][gap]{符号} {Verb}  {summary}` / `... [符号/对齐] {错误正文}`；
            // strip_visual_prefix 已剥过程区前缀，剩余部分带「符号/对齐」标记 2 格。
            // 有工具时行序为最近工具最新在前 → 原因行；无工具时首行是可打开详情的
            // Agent 入口。
            let recent_tool_lines = data
                .view_models
                .iter()
                .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
                .count()
                .min(SUBAGENT_TOOL_LINES);
            if recent_tool_lines == 0 && local_idx == 0 {
                return Some(if data.agent_name.trim().is_empty() {
                    data.agent_id.clone()
                } else {
                    data.agent_name.clone()
                });
            }
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
            // 原因行：前缀已剥，再剥「符号/对齐」标记 2 格 → 纯错误正文（§8）。
            Some(strip_reason_marker(&stripped))
        }
        _ => Some(stripped),
    }
}

fn strip_code_block_visual_padding(
    line: &ratatui_kit::ratatui::text::Line<'static>,
    mut text: String,
) -> String {
    let Some(padding) = line.spans.last() else {
        return text;
    };
    if padding.style.bg.is_none()
        || padding.style.fg.is_some()
        || padding.content.is_empty()
        || !padding.content.chars().all(|ch| ch == ' ')
        || !text.ends_with(padding.content.as_ref())
    {
        return text;
    }
    text.truncate(text.len() - padding.content.len());
    text
}

/// §9 tool header 行语义与视觉 header 共用 `ToolRenderPlan` 投影；仅去除状态符号、
/// duration 和视觉截断。
fn tool_header_semantic(data: &TuiToolCard, grid: &GridSpec) -> String {
    projected_tool_header_text(data, grid)
}

/// 子 Agent 失败原因行（§6.7）的「符号/对齐」标记：首行 `× `（错误符号 + 空格）、
/// 续行 `  `（与首行正文对齐）——恒 2 格，前缀剥净后剥掉，只留错误正文。
///
/// [Why] 按格判定而非定宽跳过：前缀宽度随 `indent` 变（§5），标记宽度恒 2 格，
/// 二者无耦合——跳过宽度只能由标记本身决定，不能从前缀宽推导。
fn strip_reason_marker(text: &str) -> String {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(' '), Some(' ')) => chars.as_str().to_string(),
        (Some(symbol), Some(' ')) if symbol != ' ' => chars.as_str().to_string(),
        _ => text.to_string(),
    }
}

/// 从渲染行剥离网格前缀列，返回内容列文本。
///
/// 前缀形态按 span 结构识别（与 `helpers` 的前缀构造同源）：
/// - 文本区：`[outer 空][符号或竖线 1 格][gap 空格]`，宽 `outer + accent + gap`；
/// - 过程区（§3/§4）：`[outer 空][indent 格留空][标记 1 格][gap 空格]`，宽
///   `process_prefix_width()`——首行标记为状态符号、续行为 `│`，同构同宽。
///
/// [Why] 结构识别而非「按 local_idx 取首行/续行定宽」：气泡内 reasoning 段
/// （过程区）与正文段（文本区）同属一个 VM 逐行混排，折叠组展开还把嵌套 entry
/// 的行拼进同一 VM——首行/续行与「过程区/文本区」并非一一对应，宽度只能按行首
/// span 边界与内容形状判定。文本区与过程区前缀总宽同为 6（`indent = gap` 时），
/// 按 span 结构区分（区分判据见 `process_prefix_len`），故先试过程区形态；
/// `indent == 0`（Compact/Narrow）渲染层回退基准形态（§9），过程区形态不参与匹配。
///
/// 结构不符（无前缀行，如 md 复制按钮行的前导空格）不剥离——兜底保留原样；
/// 入参 `local_idx` 因此不再参与判定（调用点签名保持：行索引仍是提取层的坐标）。
pub(super) fn strip_visual_prefix(
    line: &ratatui_kit::ratatui::text::Line<'static>,
    plain: &str,
    grid: &GridSpec,
    _local_idx: usize,
) -> String {
    if plain.is_empty() {
        return String::new();
    }
    match prefix_byte_len(line, grid) {
        Some(byte_len) => plain[byte_len..].to_string(),
        None => plain.to_string(),
    }
}

/// 行首前缀占用的 byte 长度（内容起始 byte）——按 span 结构识别；
/// 结构不符（无前缀行）返回 `None`。
fn prefix_byte_len(
    line: &ratatui_kit::ratatui::text::Line<'static>,
    grid: &GridSpec,
) -> Option<usize> {
    // 零宽 span（空行 / 空段）不承载前缀，先过滤；前缀最多 4 段
    // （outer / indent 留空 / 标记 / gap），取前 4 段即可判定。
    let head: Vec<&ratatui_kit::ratatui::text::Span<'static>> = line
        .spans
        .iter()
        .filter(|span| !span.content.is_empty())
        .take(4)
        .collect();
    process_prefix_len(&head, grid).or_else(|| base_prefix_len(&head, grid))
}

/// 过程区前缀（§3/§4）的 byte 长度：`[outer 空][indent 格留空][标记 1 格]
/// [gap 空格]`——首行标记为状态符号、续行为 `│`（标记列同为
/// `outer + indent`），两形态同构同宽，剥离口径合一。
///
/// [Why] 结构识别而非字形比对：标记是随内容变化的符号集（`✓`/`×`/braille 帧/
/// 折叠符/`│`），逐一枚举既易漏又随字符集降级漂移；而「`indent` 格留空 + 单格
/// 非空标记 + `gap` 空格」的 span 结构在网格里唯一——文本区前缀的 accent 是单格
/// 符号（宽度 1，不等于 `indent` 格留空），两形态不会互认。`indent == 0`
/// （Compact/Narrow）渲染层回退基准形态（§9），过程区形态不参与匹配。
fn process_prefix_len(
    head: &[&ratatui_kit::ratatui::text::Span<'static>],
    grid: &GridSpec,
) -> Option<usize> {
    let indent = grid.indent as usize;
    if indent == 0 {
        return None;
    }
    let [outer, pad, marker, gap, ..] = head else {
        return None;
    };
    if outer.content != " "
        || !is_spaces(&pad.content, indent)
        || marker.content.width() != 1
        || marker.content.trim().is_empty()
        || !is_spaces(&gap.content, grid.gap as usize)
    {
        return None;
    }
    Some(outer.content.len() + pad.content.len() + marker.content.len() + gap.content.len())
}

/// 基准前缀（文本区）的 byte 长度：`[outer 空][符号或竖线 1 格][gap 空格]`——
/// 首行（符号）与续行（竖线）同宽，形状差异不影响剥离宽度。
fn base_prefix_len(
    head: &[&ratatui_kit::ratatui::text::Span<'static>],
    grid: &GridSpec,
) -> Option<usize> {
    let [outer, accent, gap, ..] = head else {
        return None;
    };
    if outer.content != " " || accent.content.width() != 1 || accent.content.trim().is_empty() {
        return None;
    }
    if !is_spaces(&gap.content, grid.gap as usize) {
        return None;
    }
    Some(outer.content.len() + accent.content.len() + gap.content.len())
}

/// `text` 是否恰为 `width` 个 ASCII 空格——前缀间隔列（gap / 过程区缩进留空）口径。
fn is_spaces(text: &str, width: usize) -> bool {
    text.len() == width && text.bytes().all(|byte| byte == b' ')
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
