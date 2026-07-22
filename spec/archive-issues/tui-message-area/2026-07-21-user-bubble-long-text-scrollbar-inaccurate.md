# 长 UserBubble 纯文本消息导致滚动条 thumb 不准

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-21
**类型**：Bug

## 问题描述

当用户发送很长的纯文本消息后，消息区滚动条 thumb 长度不准确——页面内容已经滚到底部，但右侧滚动条 thumb 仍未抵达底部，表现为 `total_visual_rows`（内容总行数）被高估，滚动条认为还有更多内容未显示。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | 在输入区输入并发送一条很长的纯文本消息（超过一屏，含自动折行） |
| 触发时机 | 消息刚发送后立即出现 |
| 消息特征 | 纯文本长消息，无 Markdown 格式（无代码块、无列表、无引用），含大量换行导致自动折行 |
| 实际表现 | 消息区可以正常滚到底部看到完整内容，但右侧滚动条 thumb 明显偏短，未抵达底部 |
| 期望表现 | 滚动条 thumb 长度应正确反映内容总量与视口比例，滚到底时 thumb 应抵达滚动条轨道底部 |
| 影响范围 | 仅 UserBubble（用户消息）触发，AI 回复和其他消息类型不受影响 |
| 复现频率 | 必现 |

## 涉及文件

- `peri-tui/src/kit/message_area/render.rs` —— `vm_to_lines` + UserBubble 渲染（`❯ ` 前缀 + `  ` 续行缩进）
- `peri-tui/src/kit/message_area/selection.rs` —— `build_wrap_map`（基于 `Paragraph::line_count(width)` 逐行计算折行）
- `peri-tui/src/kit/message_area/mod.rs` —— `total_visual_rows` 计算 + 滚动条 `ScrollbarFields` 渲染

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | deepseek-v4-pro | 创建（auto-issue-fixer skill） |

## 修复记录

### 根因

`vm_to_lines_cached` 中 UserBubble 分支先调用 `parse_markdown_cached(data.text, width, ...)` 按 `vis_width` 折行文本，再给每一行加上 `❯ ` 前缀（首行）或 `  ` 续行缩进（2 字符）。折行后文本宽 ≤ `vis_width`，加上 2 字符前缀后总宽 ≤ `vis_width + 2`。

`build_wrap_map`（`selection.rs`）用 `Paragraph::line_count(vis_width)` 对含前缀的行重新折行。对于 `vis_width` 的 Paragraph，一条 `vis_width + 2` 宽的 Line 会被折为 2 个视觉行。

虽然实际 Paragraph 渲染也按 `vis_width` 折行，但 `parse_markdown_cached` 的折行算法与 `ratatui::Paragraph::line_count` 在边界情况（trailing spaces / word break / 等）存在细微差异，导致估算的 `total_visual_rows` 与实际渲染行数不一致。对于很长的纯文本消息，这种差异被放大为显著的 scrollbar thumb 偏移。

### 修复

```diff
-    let segments = crate::kit::markdown::parse_markdown_cached(
-        &data.text,
-        width,
+    // 预留 2 列给 ❯ 前缀 / 续行缩进，确保折行后的文本 + 前缀总宽 ≤ vis_width，
+    // 避免 build_wrap_map 的 Paragraph::line_count(vis_width) 把含前缀行多计一行。
+    let user_text_width = width.saturating_sub(2).max(1);
+    let segments = crate::kit::markdown::parse_markdown_cached(
+        &data.text,
+        user_text_width,
```

**原理**：让 markdown 解析器按 `vis_width - 2` 折行纯文本，加上 2 字符前缀后，单行总宽始终 ≤ `vis_width`。这样 `build_wrap_map` 的 `Paragraph::line_count(vis_width)` 对每条 Line 恒定输出 1 个视觉行，与实际渲染一致。

### 涉及文件

- `peri-tui/src/kit/message_area/render.rs:190-196` —— `parse_markdown_cached` width 参数改为 `width.saturating_sub(2).max(1)`

### 验证

- `cargo build -p peri-tui` 通过
- `cargo test -p peri-tui --lib` 586 全部通过
- `cargo test -p peri-tui --lib -- message_area` 62 全部通过

### 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | deepseek-v4-pro | 创建（auto-issue-fixer skill） |
| 2026-07-21 | Open | Fixed | deepseek-v4-pro | 修复：UserBubble markdown 解析宽度预留前缀 2 列 |
