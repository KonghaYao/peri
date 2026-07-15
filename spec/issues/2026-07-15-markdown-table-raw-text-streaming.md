# Markdown 表格流式输出时显示为原始 pipe 格式

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-15
**类型**：Bug

## 问题描述

Agent 在流式输出含 markdown 表格的回复时，部分表格不渲染为表格样式，而是直接显示原始的 pipe 格式（如 `| 原文 | 改为 |`、`|---|---|`），用户看到的是 markdown 源码而非终端表格。

此前表格能正常渲染（至少有 unicode 边框），是最近 1-2 天内出现的回归。

## 症状详情

### 现象 1：流式输出期间表格始终保持原始格式

从 agent 开始输出表格内容的第一个字符起，到整个回复完成，表格一直显示为原始的 pipe 分隔符格式，从未被渲染为终端表格。不是先正常后异常，而是一开始就不正常。

### 现象 2：仅部分表格受影响

并非所有表格都有此问题，部分表格能正常渲染。用户怀疑与表头格式有关——可能特定格式的表头导致解析器回退为普通段落。

### 现象 3：最近 1-2 天的回归

这个问题是在最近 1-2 天内出现的，之前表格能正常渲染。期间与 markdown 表格/增量缓存相关的提交包括：

- `12a38806` perf(tui): 消息区流式渲染性能优化（引入 markdown 增量缓存机制）
- `23a177d7` fix(markdown): Table 增量缓存导致数据行丢失（修复 table header 缓存导致 rows 消失的 bug）
- `95227fa8` fix: ratatui-kit-markdown 改用 crates.io 版本 0.3.0（从本地 path 依赖切换到发布版）

## 复现条件

- **复现频率**：偶发（部分表格出现，并非所有表格）
- **触发步骤**：
  1. 向 agent 发出一个会让它输出含 markdown 表格的请求
  2. 观察流式输出过程中表格的渲染状态
  3. 对比：某些表格正常渲染，某些显示原始 pipe 格式
- **环境**：macOS，TUI 模式

## 根因分析

流式输出期间，表格的 header 行（如 `| 原文 | 改为 |\n`）先到达时，pulldown-cmark 因缺少分隔符（`|---|`）将其解析为 `Paragraph` 而非 `Table`。增量缓存 `parse_markdown_cached` 以 `\n` 换行结尾的条件将此时的状态持久化（`processed_block_count=1`）。

随后分隔符+数据行到达，同一文本前缀下的 block 结构从 `[Paragraph]` 翻转为 `[Table]`，但 `can_reuse` 检查仅在 block 数量上满足（1 ≤ 1），未检测到 block **类型**已变更。导致 `Table` block 被跳过，旧 `Paragraph` 中的原始 pipe 文本永久残留在输出中，表格无法渲染。

## 修复内容

在 `ConvertState` 中新增 `has_potential_table_header` 标记——当 `Paragraph` 首行以 `|` 开头时标记为「可能是表头行的段落」。`parse_markdown_cached` 的 `can_reuse` 条件中增加 `&& !has_potential_table_header`，确保此场景下缓存失效、全量重跑。

## 涉及文件

（见上文）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-15 | — | Open | agent | 创建 |
| 2026-07-15 | Open | Fixed | agent | 修复：新增 `has_potential_table_header` 缓存失效标记 |

## 修复记录

### 修复 #1（2026-07-15）

- **操作人**：agent
- **用户原意**：表格在流式输出中正常渲染，而非显示原始 pipe 格式
- **修复内容**：
  - `peri-tui/src/kit/markdown/convert.rs`：`ConvertState` 新增 `has_potential_table_header` 字段，Paragraph 处理中检测首行以 `|` 开头的行
  - `peri-tui/src/kit/markdown/mod.rs`：`can_reuse` 条件新增 `&& !cache.stable_state.has_potential_table_header`；新增回归测试 `test_cached_table_header_streamed_before_separator`
- **验证状态**：待验证（用户确认后更新为 Verified）
