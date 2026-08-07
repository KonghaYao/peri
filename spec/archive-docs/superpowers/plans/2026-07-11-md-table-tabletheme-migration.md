# Markdown 表格接入 ratatui-kit TableTheme 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 md 表格的硬编码 `Color::Gray` 边框替换为 ratatui-kit `TableTheme`（从 `Palette` 派生），并用 ratatui 原生 `Table` widget 替代手工 Paragraph→Buffer 单元格渲染。

**Architecture:** `table_data_to_lines` 签名从 `border_style: Style` 改为接受 `TableTheme`。内部用 `ratatui::widgets::Table` 渲染到临时 `Buffer`，提取 `Vec<Line>` 保持当前 inline 消息流兼容性。列宽计算增强——增加每列最小 CJK 宽度保底（当前统一 `max(3)` 后按比例压缩，长 CJK 内容会被压到不可读）。

**Tech Stack:** Rust 2021, ratatui 0.29, ratatui-kit 0.10.2 (`ComponentTheme`/`Palette`/`TableTheme`), unicode-width

**关联 issue:** `spec/issues/2026-07-11-markdown-table-ratatui-kit-theme.md`

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `peri-tui/src/kit/markdown/table.rs` | **Modify** | 核心改动：签名变更、内部用 ratatui Table widget、列宽增强 |
| `peri-tui/src/kit/message_area.rs` | **Modify** | 两处调用点（L251-257, L310-324）：传入 `TableTheme` 替代 `Style::default().fg(Gray)` |
| `peri-tui/src/kit/markdown/types.rs` | **No change** | `TableData` 结构不变——`headers/rows` 仍在 `Vec<Vec<Span>>`，alignments 用 `pulldown_cmark::Alignment` |
| `peri-tui/src/kit/markdown/mod.rs` | **No change** | 导出不变 |

---

### Task 1: `compute_table_col_widths` 增加 CJK 最小宽度保底

**Files:**
- Modify: `peri-tui/src/kit/markdown/table.rs:16-58`

**背景**：当前每列 `.max(3)` 保底对所有列统一，但 CJK 列（如中文表头 "用户角色"）在等比例压缩后会变成 2 列宽，只能容下半个中文字符。应基于该列最长 CJK 字符的实际显示宽度设保底线。

- [ ] **Step 1: 重写 `compute_table_col_widths`，在压缩前加入每列 CJK 最小宽度计算**

```rust
/// 计算表格各列宽度（含 CJK 最小宽度保底 + 等比例缩放适配 max_width）。
pub(crate) fn compute_table_col_widths(
    headers: &[Vec<Span<'static>>],
    rows: &[Vec<Vec<Span<'static>>>],
    col_count: usize,
    max_width: usize,
) -> Vec<usize> {
    let mut col_widths = vec![0usize; col_count];
    for (i, cell) in headers.iter().enumerate() {
        col_widths[i] = col_widths[i].max(span_width(cell));
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                col_widths[i] = col_widths[i].max(span_width(cell));
            }
        }
    }

    // CJK 最小宽度保底：每列至少能容纳该列内容中最宽的单个 CJK 字符
    // （或最少 3 列——ASCII 最小可读）。
    let min_cjk: Vec<usize> = (0..col_count)
        .map(|i| {
            let content_max = headers
                .get(i)
                .into_iter()
                .flatten()
                .chain(rows.iter().flatten().filter_map(|row| row.get(i)).flatten())
                .flat_map(|s| s.content.chars().map(|c| c.width().unwrap_or(1)))
                .max()
                .unwrap_or(1);
            // 保底：至少 3 列（ASCII 最小可读），最多 10（防止某列有超宽 emoji 撑爆）
            content_max.max(1).min(10).max(3)
        })
        .collect();

    // 确保每列至少满足 min_cjk
    for i in 0..col_count {
        col_widths[i] = col_widths[i].max(min_cjk[i]);
    }

    let total_border = 1 + 3 * col_count;
    let needed = total_border + col_widths.iter().sum::<usize>();
    if needed > max_width {
        let available = max_width.saturating_sub(total_border);
        if available > 0 {
            let total: usize = col_widths.iter().sum();
            if total > 0 {
                let mut allocated = 0;
                for (i, w) in col_widths.iter_mut().enumerate() {
                    if i == col_count - 1 {
                        *w = available.saturating_sub(allocated).max(min_cjk[i].min(2));
                    } else {
                        *w = (*w * available / total).max(min_cjk[i].min(2));
                        allocated += *w;
                    }
                }
            }
        }
    }

    col_widths
}
```

- [ ] **Step 2: 运行现有测试确认不破坏原有行为**

```bash
cargo test -p peri-tui --lib -- markdown
```

预期：全部通过。

- [ ] **Step 3: 提交**

```bash
git add peri-tui/src/kit/markdown/table.rs
git commit -m "feat(markdown): add per-column CJK min-width guard in table col width calc"
```

---

### Task 2: `table_data_to_lines` 改用 ratatui Table widget + TableTheme

**Files:**
- Modify: `peri-tui/src/kit/markdown/table.rs:66-146`（公开渲染入口 + build_grid_* 辅助函数）

**背景**：当前 `align_cell` 闭包对每个单元格创建 Paragraph→Buffer→逐字提取，O(N·cols·rows·widest) 且只能处理单行单元格。改用 ratatui `Table` widget 后，由 ratatui 内部处理对齐、换行。

- [ ] **Step 1: 重写 `table_data_to_lines` 签名和实现**

```rust
use ratatui::widgets::{Block, Borders, Cell, Row, Table as RTable};
use ratatui::layout::Constraint;
use ratatui_kit::{ComponentTheme, Palette};

use super::types::TableData;

/// 使用 ratatui `Table` widget + ratatui-kit `TableTheme` 渲染表格到 `Vec<Line>`。
pub fn table_data_to_lines(data: &TableData, theme: &ratatui_kit::components::TableTheme) -> Vec<Line<'static>> {
    let col_count = data.col_widths.len();
    if col_count == 0 {
        return vec![Line::default()];
    }

    // 构建 Block 边框
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.border_style);

    // 把 TableData 的 header 行转换为 ratatui Row
    let header_align = |i: usize| match data.alignments.get(i) {
        Some(Alignment::Center) => RAlignment::Center,
        Some(Alignment::Right) => RAlignment::Right,
        _ => RAlignment::Left,
    };

    let header_row: Option<Row> = if data.headers.is_empty() {
        None
    } else {
        Some(Row::new(
            (0..col_count)
                .map(|i| {
                    let spans = data.headers.get(i).cloned().unwrap_or_default();
                    Cell::from(Line::from(spans)).style(theme.header_style)
                })
                .collect::<Vec<_>>(),
        ))
    };

    // 构建数据行
    let data_rows: Vec<Row> = data
        .rows
        .iter()
        .map(|row| {
            Row::new(
                (0..col_count)
                    .map(|i| {
                        let spans = row.get(i).cloned().unwrap_or_default();
                        let align = header_align(i);
                        // ratatui Table 的 Cell 不直接支持单单元格对齐，
                        // 我们用 Line 级别的 alignment
                        let line = Line::from(spans).alignment(align);
                        Cell::from(line).style(theme.row_style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    // 构建列宽约束
    let constraints: Vec<Constraint> = data
        .col_widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();

    // 构建并渲染 Table
    let mut table = RTable::new(data_rows, &constraints)
        .block(block)
        .style(theme.row_style)
        .column_spacing(1);

    if let Some(hr) = header_row {
        table = table.header(hr.style(theme.header_style));
    }

    // 计算渲染区域
    let total_height = table_rows_count(&data, &header_row) as u16;
    let total_width = data.col_widths.iter().sum::<usize>() as u16
        + (col_count as u16) * 3; // borders + spacing

    let area = Rect::new(0, 0, total_width, total_height);
    let mut buf = Buffer::empty(area);
    table.render(area, &mut buf);

    // 从 Buffer 提取 Vec<Line>
    buffer_to_lines(&buf, area)
}

/// 计算需要渲染的总行数（表头 + 数据行 + 边框）
fn table_rows_count(data: &TableData, has_header: &bool) -> usize {
    let border_rows = 2; // 上下边框各 1
    let header_rows = if *has_header { 1 } else { 0 };
    let separator = if *has_header && !data.rows.is_empty() { 1 } else { 0 };
    border_rows + header_rows + separator + data.rows.len()
}

/// 将 ratatui Buffer 行转换为 Vec<Line<'static>>
fn buffer_to_lines(buf: &Buffer, area: Rect) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in 0..area.height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_span = String::new();
        let mut current_style: Option<Style> = None;

        for x in 0..area.width {
            let cell = buf.cell((x, y));
            let ch = cell.map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '));
            let cell_style = cell.map(|c| c.style()).unwrap_or_default();

            match current_style {
                Some(s) if s == cell_style => {
                    current_span.push(ch);
                }
                _ => {
                    if !current_span.is_empty() {
                        spans.push(Span::styled(
                            std::mem::take(&mut current_span).into(),
                            current_style.unwrap_or_default(),
                        ));
                    }
                    current_span.push(ch);
                    current_style = Some(cell_style);
                }
            }
        }
        if !current_span.is_empty() || current_style.is_some() {
            spans.push(Span::styled(
                current_span.into(),
                current_style.unwrap_or_default(),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}
```

- [ ] **Step 2: 删除不再使用的私有函数**

删除：
- `build_grid_border`（L150-166）：被 ratatui Table widget 的 `Block::bordered()` 替代
- `build_grid_row`（L168-183）：被 `Row::new() + Cell::from()` 替代
- `align_cell` 闭包（L80-92）：被 `Line::alignment()` + ratatui Table 内部处理替代

保留：
- `compute_table_col_widths`（已在前 task 改进）
- `span_width`（仍被 `compute_table_col_widths` 使用）

- [ ] **Step 3: 更新 imports**

```rust
use pulldown_cmark::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment as RAlignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table as RTable},
};
use unicode_width::UnicodeWidthStr;

use super::types::TableData;
use ratatui_kit::components::TableTheme;
```

- [ ] **Step 4: 编译验证**

```bash
cargo check -p peri-tui 2>&1 | head -30
```

修复编译错误直到 clean。

- [ ] **Step 5: 提交**

```bash
git add peri-tui/src/kit/markdown/table.rs
git commit -m "refactor(markdown): use ratatui Table widget + TableTheme for md table rendering"
```

---

### Task 3: `message_area.rs` 调用点适配

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:251-257`（AssistantBubble inline）
- Modify: `peri-tui/src/kit/message_area.rs:310-324`（UserBubble inline）

- [ ] **Step 1: 在两处调用点从 Palette 构建 TableTheme**

AssistantBubble 处（L241-260 上下文已有 `palette_guard`）：

```rust
// 当前代码 (L251-257):
crate::kit::markdown::MarkdownSegment::Table(data) => {
    let table_border_style =
        Style::default().fg(ratatui::style::Color::Gray);
    lines.extend(crate::kit::markdown::table_data_to_lines(
        &data,
        table_border_style,
    ));
}

// 改为:
crate::kit::markdown::MarkdownSegment::Table(data) => {
    let table_theme = ratatui_kit::components::TableTheme::from_palette(&palette_guard);
    lines.extend(crate::kit::markdown::table_data_to_lines(
        &data,
        &table_theme,
    ));
}
```

UserBubble 处（L310-324，需要先从 `THEME_ATOM` 获取 Palette）：

```rust
// 当前代码:
crate::kit::markdown::MarkdownSegment::Table(data) => {
    lines.push(Line::from(vec![Span::styled(
        "  ",
        Style::default().bg(user_bg),
    )]));
    let table_border_style = Style::default().fg(ratatui::style::Color::Gray);
    let table_lines =
        crate::kit::markdown::table_data_to_lines(&data, table_border_style);
    for tl in table_lines {
        let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
        for span in tl.spans {
            spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
        }
        lines.push(Line::from(spans));
    }
}

// 改为:
crate::kit::markdown::MarkdownSegment::Table(data) => {
    let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
    let palette_guard = palette_state.read();
    let table_theme = ratatui_kit::components::TableTheme::from_palette(&palette_guard);
    lines.push(Line::from(vec![Span::styled(
        "  ",
        Style::default().bg(user_bg),
    )]));
    let table_lines =
        crate::kit::markdown::table_data_to_lines(&data, &table_theme);
    for tl in table_lines {
        let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
        for span in tl.spans {
            spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
        }
        lines.push(Line::from(spans));
    }
}
```

注意：UserBubble 这边需要**新增** `PALETTE_ATOM` 的 state 读取。需要确认当前作用域中已有 `use peri_theme::atoms` import。

- [ ] **Step 2: 编译验证**

```bash
cargo check -p peri-tui 2>&1 | head -30
```

- [ ] **Step 3: 提交**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "refactor(markdown): pass TableTheme from palette to table_data_to_lines"
```

---

### Task 4: 手动验收测试

**Files:** 无新建文件

- [ ] **Step 1: 启动 TUI 并确认表格渲染正常**

```bash
cargo run -p peri-tui
```

在对话中让 agent 输出一个含表格的 Markdown 内容，例如：

```
请用表格列出 Rust 的三种字符串类型及其特点：

| 类型 | 特点 | 使用场景 |
|------|------|----------|
| &str | 借用、不可变 | 函数参数 |
| String | 拥有所有权、可变、堆分配 | 构建新字符串 |
| OsString | 平台原生编码 | 文件路径处理 |
```

验收点：
- [ ] 表格边框颜色跟随当前主题（不是硬编码灰色）
- [ ] 表头有 accent 色样式
- [ ] 数据行用 fg 色
- [ ] CJK 列宽正常（中文字符不被截断）

- [ ] **Step 2: 切换主题验证**

```bash
/theme dark  # 或其他已安装主题
```

验收点：
- [ ] 表格边框颜色随主题变化
- [ ] 表头 accent 色随主题变化

- [ ] **Step 3: 验证历史对话中的表格**

进入 History 面板，加载一个包含表格输出的历史 session。

验收点：
- [ ] 历史表格渲染正常（颜色跟随当前主题）
- [ ] 无 panic/crash

- [ ] **Step 4: 运行所有 TUI 测试**

```bash
cargo test -p peri-tui --lib
```

预期：全部通过。

- [ ] **Step 5: 提交**（如有问题修复）

```bash
git add -A
git commit -m "test(markdown): manual verification of md table theme-aware rendering"
```

---

### Task 5: 清理——`table.rs` 中不再需要的 import

**Files:**
- Modify: `peri-tui/src/kit/markdown/table.rs:1-9`

- [ ] **Step 1: 移除旧 import**

当前 import：
```rust
use pulldown_cmark::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment as RAlignment, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Paragraph, Widget},
};
```

需要新增 `Block`, `Borders`, `Cell`, `Row`, `Table as RTable`；移除 `Text`, `Paragraph`, `Widget`（如不再使用）。

最终 import：
```rust
use pulldown_cmark::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment as RAlignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table as RTable},
};
use unicode_width::UnicodeWidthStr;

use super::types::TableData;
use ratatui_kit::components::TableTheme;
```

- [ ] **Step 2: 编译验证**

```bash
cargo check -p peri-tui
```

- [ ] **Step 3: 提交**

```bash
git add peri-tui/src/kit/markdown/table.rs
git commit -m "chore(markdown): clean up unused imports in table.rs"
```

---

## 验收标准总结

- [ ] 表格边框/表头/数据行颜色从 `TableTheme::from_palette()` 派生——切主题后表格颜色跟随变化
- [ ] 表格列宽在 CJK 内容下有合理的保底线（不再出现中文列被压缩到 2 列宽）
- [ ] 宽表格（超出终端宽度）正常按比例缩放（行为不变——后续独立 issue 可支持 ScrollView 内横向滚动）
- [ ] `cargo test -p peri-tui --lib` 全部通过
- [ ] 历史对话中的表格加载正常
- [ ] 无 panic、无 crash

## 不在此计划内

- ❌ 表格键盘交互（row selection、列导航）——保持 inline 静态渲染
- ❌ 宽表格横向滚动（ScrollView wrap）——当前超出终端宽度仍按比例缩放各列
- ❌ 将 `TableData` 改为 ratatui-kit `Table` 组件在 component tree 中渲染 —— 保持 `Vec<Line>` inline 输出
