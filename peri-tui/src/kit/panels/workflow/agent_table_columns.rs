//! Table layout.

use ratatui_kit::ratatui::style::Style;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::workflow_panel::{
    AGENT_NAME_COL_WIDTH, AGENT_NAME_MIN_WIDTH, AGENT_ROW_LEADING_COLS, MODEL_COL_MIN_WIDTH,
    MODEL_COL_WIDTH, TOKENS_COL_WIDTH, TOOLS_COL_WIDTH,
};

#[must_use]
pub(super) fn status_emoji_for_run(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",           // ●
        "completed" => "\u{2713}",         // ✓
        "failed" | "killed" => "\u{2717}", // ✗
        _ => "\u{25cb}",                   // ○
    }
}

#[must_use]
pub(super) fn status_emoji_for_phase(status: &str) -> &'static str {
    match status {
        "active" => "\u{25cf}",  // ●
        "done" => "\u{2713}",    // ✓
        "pending" => "\u{25cb}", // ○
        _ => "\u{25cb}",         // ○
    }
}

#[must_use]
pub(super) fn status_emoji_for_agent(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",          // ●
        "done" => "\u{2713}",             // ✓
        "pending" => "\u{25cb}",          // ○
        "dead" | "skipped" => "\u{2717}", // ✗
        _ => "\u{25cb}",                  // ○
    }
}

pub(super) fn phase_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "active" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "failed" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

pub(super) fn agent_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "running" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "dead" | "skipped" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

/// 壁钟驱动的运行中动画帧指示器。每 100ms 推进一帧。
pub(super) fn running_indicator() -> String {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    const FRAMES: &[char] = &[
        '✳', '✴', '✵', '✶', '✷', '✸', '✹', '✺', '✻', '✼', '❃', '❊', '✼', '✻', '✺', '✸',
    ];
    let start = START.get_or_init(peri_time::monotonic_now);
    let tick = (peri_time::elapsed_since(*start).as_millis() / 100) as usize;
    let frame = FRAMES[tick % FRAMES.len()];
    format!("{frame}")
}

/// Abbreviate a count into a human-readable short form.
#[must_use]
pub(super) fn abbreviate_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
}

/// 右栏 Agents 表的列宽计划——按可用宽度伸缩，保证任何终端宽度下表格不越界。
///
/// 优先级：名称（最多 18 列）> Tokens / Tools > Model。空间不足时先收缩名称，
/// 再隐藏 Model，最后依次舍弃 Tools、Tokens；名称本身始终保留至少 1 列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentTableColumns {
    pub(super) name: usize,
    pub(super) model: Option<usize>,
    pub(super) tokens: bool,
    pub(super) tools: bool,
}

impl AgentTableColumns {
    pub(super) fn for_width(width: usize) -> Self {
        const LEADING: usize = AGENT_ROW_LEADING_COLS;
        const NAME_MIN: usize = AGENT_NAME_MIN_WIDTH;
        const NAME_MAX: usize = AGENT_NAME_COL_WIDTH;
        const TOKENS_PART: usize = 1 + TOKENS_COL_WIDTH;
        const TOOLS_PART: usize = 2 + TOOLS_COL_WIDTH;
        const MODEL_PART_MIN: usize = 1 + MODEL_COL_MIN_WIDTH;

        let metrics = TOKENS_PART + TOOLS_PART;
        if width >= LEADING + NAME_MAX + metrics + MODEL_PART_MIN {
            let model = MODEL_COL_WIDTH.min(width - LEADING - NAME_MAX - metrics - 1);
            return Self {
                name: NAME_MAX,
                model: Some(model),
                tokens: true,
                tools: true,
            };
        }
        if width >= LEADING + NAME_MIN + metrics {
            return Self {
                name: NAME_MAX.min(width - LEADING - metrics),
                model: None,
                tokens: true,
                tools: true,
            };
        }
        if width >= LEADING + NAME_MIN + TOKENS_PART {
            return Self {
                name: NAME_MAX.min(width - LEADING - TOKENS_PART),
                model: None,
                tokens: true,
                tools: false,
            };
        }
        Self {
            name: width.saturating_sub(LEADING).max(1),
            model: None,
            tokens: false,
            tools: false,
        }
    }

    /// 该计划下单行占用的终端列数（含列间分隔）。
    #[cfg(test)]
    pub(super) fn row_width(&self) -> usize {
        AGENT_ROW_LEADING_COLS
            + self.name
            + self.model.map_or(0, |width| 1 + width)
            + if self.tokens { 1 + TOKENS_COL_WIDTH } else { 0 }
            + if self.tools { 2 + TOOLS_COL_WIDTH } else { 0 }
    }
}

/// Agents 表头：列位置与数据行（`arrow + emoji + name` + 可选指标列）逐列对齐。
pub(super) fn agent_table_header(model_header: &str, columns: &AgentTableColumns) -> String {
    let label = truncate_to_width("Agents", columns.name);
    let mut out = String::from(' ');
    out.push_str(&label);
    let used = 1 + UnicodeWidthStr::width(label.as_str());
    let name_end = AGENT_ROW_LEADING_COLS + columns.name;
    if used < name_end {
        out.push_str(&" ".repeat(name_end - used));
    }
    if let Some(width) = columns.model {
        out.push(' ');
        out.push_str(&model_cell(Some(model_header), width));
    }
    if columns.tokens {
        out.push(' ');
        out.push_str(&format!("{:<width$}", "Tokens", width = TOKENS_COL_WIDTH));
    }
    if columns.tools {
        out.push_str("  ");
        out.push_str(&format!("{:<width$}", "Tools", width = TOOLS_COL_WIDTH));
    }
    out
}

/// Model 列单元格：缺失显示 '-'；超过列宽按显示宽度截断（Unicode 安全），
/// 未超过则补齐到列宽。
pub(super) fn model_cell(model: Option<&str>, width: usize) -> String {
    let s = model.unwrap_or("-");
    let w = UnicodeWidthStr::width(s);
    if w <= width {
        format!("{s}{}", " ".repeat(width - w))
    } else {
        truncate_to_width(s, width)
    }
}

/// 按终端显示宽度截断（Unicode 字符边界安全）：显示宽度超过 `width` 时
/// 保留 `width - 1` 列并追加 '…'。
pub(super) fn truncate_to_width(s: &str, width: usize) -> String {
    if UnicodeWidthStr::width(s) <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > keep {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}
