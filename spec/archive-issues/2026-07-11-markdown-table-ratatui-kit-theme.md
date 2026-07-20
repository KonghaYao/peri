# Markdown 表格使用 ratatui-kit TableTheme 替代硬编码样式


> 归档于 2026-07-20，原路径 spec/issues/2026-07-11-markdown-table-ratatui-kit-theme.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-11
**类型**：功能增强

## 问题描述

当前 md 表格渲染（`peri-tui/src/kit/markdown/table.rs`）使用手绘 unicode 边框 + 硬编码 `Color::Gray` 样式，不跟随主题变化。虽然 ratatui-kit 0.10.2 已有完整的 `Table` 组件（`TableTheme`、`TableProps`、列宽自适应、单元格换行），但当前表格以静态 `Vec<Line>` 输出混入 Markdown 文本流，未接入 `TableTheme`。

## 症状详情

### 现象 1：主题切换后表格样式不变

用户切换主题（`/theme` 或 ThemePanel），代码块、标题、链接等所有 Markdown 元素颜色随主题变化，但表格边框仍是 `Color::Gray`，与当前主题的 `border` 色不匹配。

### 现象 2：cell 对齐通过 Paragraph→Buffer 提取字符

`table_data_to_lines:80-92` 的 `align_cell` 闭包对每个单元格创建 `Paragraph`、渲染到 1 行 `Buffer`，再逐字符提取 `symbol()`。这是 O(N·cols·rows·widest) 的开销，且只支持单行单元格——无法处理含换行符的长文本。

### 现象 3：列宽分配只做等比缩放

`compute_table_col_widths` 的分配策略是将超出宽度后等比例压缩各列，最后一列兜底。缺少最小宽度保底（当前对所有列统一 `max(3)`，不管内容实际需要多少 CJK 字符宽度），也没有换行策略——内容过长的单元格被截断而非换行。

## 期望行为

1. 表格渲染接入 ratatui-kit `TableTheme`（从 `Palette` 派生），边框/表头/数据行颜色随主题变化
2. Cell 对齐和内容渲染使用 ratatui 原生的 `Table` widget 能力（列宽自适应、单元格内换行 `TableWrapMode`）
3. 保持 inline 静态渲染模式——不需要键盘导航、不需要 `TableState`/row selection
4. 表格仍以 `Vec<Line>` 形式嵌入消息流（与当前调用方 `message_area.rs` 兼容）

## 涉及文件

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `peri-tui/src/kit/markdown/table.rs` | 修改 | 核心改动——`table_data_to_lines` 签名从 `border_style: Style` 改为接受 `TableTheme` |
| `peri-tui/src/kit/markdown/types.rs` | 可能修改 | 表格可能需要存储换行后的多行行信息 |
| `peri-tui/src/kit/markdown/mod.rs` | 可能修改 | 导出变更 |
| `peri-tui/src/kit/message_area.rs` | 小改 | 两处 `table_data_to_lines` 调用点：`line 251-257`（AssistantBubble 内联）和 `line 309-316`（UserBubble 内联），需要传入 `TableTheme` 而非 `Style::default().fg(Gray)` |

## ratatui-kit Table 组件参考

ratatui-kit 0.10.2 的 `Table` 组件位于 `src/components/table/`：

- **`TableTheme`** (`component.rs:26-58`)：`header_style`(取 `palette.accent`) / `footer_style` / `row_style`(取 `palette.fg`) / `highlight_style` / `border_style`(取 `palette.border`) / `horizontal_line_style`
- **`TableProps<T>`** (`component.rs:68-105`)：`columns: Vec<TableColumn>` / `rows: Vec<T>` / `render_row`（闭包把 T 转成 `Vec<TableCell>`）/ `footer` / `active` / `wrap_mode`
- **`TableColumn`** (`types.rs:53-77`)：`header: Line` / `alignment` / `width: Constraint` / `highlight_style`
- **`TableCell`** (`types.rs:82-93`)：`content: Line` / `alignment_override`
- **列宽逻辑** (`layout.rs`)：`resolve_column_widths` 支持 `Constraint::Length` / `Percentage` / `Max` 等

**用于 inline 渲染的可行方案**：参考 ratatui 原生的 `ratatui::widgets::Table`，将 `TableData` 转换为 ratatui `Table` widget，渲染到临时 Buffer 提取 `Vec<Line>`，但样式字段从 `ratatui-kit` 的 `TableTheme` 读取。这避免了重写列宽/对齐/换行逻辑，也兼容当前的 inline `Vec<Line>` 输出方式。

## 实现策略建议

1. **`table_data_to_lines` 签名改为 `(data: &TableData, theme: &TableTheme) -> Vec<Line>`**
2. **内部用 ratatui `Table` widget 渲染**（build 时用 `theme.border_style` 设 `Block::bordered()`、`theme.header_style` 设 `header_style`、`theme.row_style` 设 `style`）
3. **列宽使用 `Constraint::Length(w)`** 基于现有 `compute_table_col_widths` 结果
4. **`message_area.rs` 两处调用点改为从 `THEME_ATOM` 读取 `TableTheme`**
5. **可选的列宽计算增强**：复用 ratatui-kit `layout::resolve_column_widths` 或增加最小 CJK 列宽保底线

## 备注

- 跟之前的 ratatui-kit-markdown 迁移属于同一方向（全面接入 ratatui-kit 主题体系），但不是 `Markdown` 组件迁移——md 表格解析仍走 `ratatui_kit_markdown::parse_markdown` → `TableData`，只改渲染层
- 不需要引入新的 crate 依赖（`ratatui-kit` 0.10.2 已是 direct dependency）
