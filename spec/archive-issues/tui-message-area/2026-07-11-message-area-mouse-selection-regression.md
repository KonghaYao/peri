> 归档于 2026-07-17，原路径 spec/issues/2026-07-11-message-area-mouse-selection-regression.md
# 消息区鼠标拖拽选中复制功能因重构回归 + 鼠标拖拽 CPU 暴涨

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-11

## 问题描述

消息区的鼠标拖拽选中文本并自动复制到剪贴板的功能在 ratatui-kit 标记迁移重构（`3bfb9fff`）后完全失效。鼠标在消息区拖拽无选区高亮、无文本提取、无剪贴板复制——任何操作均无响应。

此外，`3bfb9fff` 后的鼠标事件处理器只过滤了 `MouseEventKind::Moved`，未过滤 `MouseEventKind::Drag(Left)`。用户在消息区拖拽时，终端按 cell 级高频发送 Drag 事件，每个事件都触发 `EventResult::Consumed` → 组件重渲染 → `all_lines` 数千行 clone → CPU 飙升。

## 症状详情

| 操作 | 期望行为 | 实际行为（修复前） |
|------|----------|----------|
| 鼠标按下左键开始拖拽 | 记录起始坐标，准备选区 | 无反应 |
| 鼠标拖拽过程中 | 显示选区高亮（主题色背景） | 无反应，且 CPU 暴涨 |
| 松开鼠标 | 提取选中文本并复制到系统剪贴板 | 无反应 |
| 复制成功后 | 状态栏显示 "已复制 N 字符" | 无提示 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在消息区用鼠标按住左键拖拽
  2. 观察无选中高亮、无复制行为
- **环境**：所有平台

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 消息区鼠标事件处理，`Down(Left)` 被显式忽略（884 行 `return EventResult::Ignored`），缺少 Drag/Up 的选区处理逻辑
- `peri-tui/src/kit/text_selection.rs`（265 行） —— 完整的 TextSelection 数据结构 + 字符级文本提取 + 选区高亮，但被 `#[allow(dead_code)]` 禁用，无任何调用方。依赖已删除的 `RENDER_CACHE.entries(Vec<Line>)` + `wrap_map`
- `peri-tui/src/kit/atoms.rs` —— `COPY_CHAR_COUNT` / `COPY_MESSAGE_UNTIL` atom 仍存在，status_bar 仍在读取，但无写入方（死代码）
- `peri-tui/src/kit/status_bar.rs` —— "已复制 N 字符" 提示逻辑完整保留，仅缺少触发入口

## 回归根因

commit `3bfb9fff` ("refactor(tui): delete render_bridge + bubbles/view_render pipeline") 删除了 ~3350 行渲染管线代码，其中包括 `render_bridge.rs`（含 `RENDER_CACHE.entries: Vec<Line>` + `wrap_map`）。text_selection 的文本提取和选区渲染原本依赖这些数据结构做"终端坐标 → 视觉坐标 → 文本内容"映射。

迁移计划中明确标注为已知风险，标记为"后续独立补回"，但至今未补回：

> **R2. text_selection 功能丢失（接受，后续补回）**
> 保留 `text_selection.rs` 代码但功能失效（鼠标 Drag 复制不可用）。
> —— `docs/superpowers/specs/2026-07-09-ratatui-kit-markdown-migration-design.md:223-225`

**CPU 暴涨追加根因**：`message_area.rs` 鼠标事件处理器仅过滤 `MouseEventKind::Moved`，`MouseEventKind::Drag(Left)` 穿透 `_ => {}` 后 `return EventResult::Consumed`，触发组件重渲染 → 每帧 clone 数千行 `Line<'static>`（含 syntect 高亮 Span）。

## 参考

- 旧 issue `spec/issues/2026-07-05-message-area-no-copy-capability.md`（Partial）—— 初次实现记录，包含旧 v1 架构参考代码路径、初版修复记录、三项残留问题（崩溃、事件处理器累积、macOS arboard）
- `spec/archive-issues/2026-07-06-message-area-copy-complex-content-crash.md` —— 复制后滚动卡死的根因修复（render body 写 atom 自激回路 + arboard 线程安全）
- `spec/archive-issues/2026-07-05-message-area-crashes-and-rendering.md` —— u16 overflow / ScrollView 双重滚动 / arboard 剪贴板线程三项修复

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建 |
| 2026-07-11 | Open | Fixed | agent | 实现完成，见修复记录 #2（代码实际从未提交——修复 #1 为描述性草案） |

## 修复记录

### 修复 #2（2026-07-11，实际实施）

- **操作人**：agent
- **用户原意**：恢复消息区鼠标拖拽选中 + 自动复制到剪贴板功能，并解决拖拽时 CPU 暴涨

**设计概要**：

在 ratatui-kit 新架构下重建文本选中功能。利用已有的 `text_selection.rs` helpers（`TextSelection` / `line_to_plain_text` 等），在 message_area 中集成完整的 Down/Drag/Up 三态选区流程 + 高亮 + 剪贴板复制 + 状态栏通知。

**数据流**：

```
MouseEvent(Down/Drag/Up)
  → (mouse_row - area.y + scroll_y) → visual_ row/col
  → TextSelection.start_drag / update_drag / end_drag
  → extract_logical_range(vis_start, vis_end, &all_lines, &wrap_map) → 纯文本
  → copy_to_clipboard(text)   // std::thread::spawn + arboard
  → mark_copy_message(char_count)  // COPY_CHAR_COUNT / COPY_MESSAGE_UNTIL
```

**新增组件**：

| 组件 | 说明 |
|------|------|
| `DragThrottle` | Drag 事件 16ms 节流，`write_no_update` 避免自激回路 |
| `WrappedLineInfo` + `build_wrap_map()` | 用 `Paragraph::line_count(width)` 逐行计算折行，构建视觉行→逻辑行映射 |
| `visual_to_logical()` | wrap_map 二分查找 |
| `highlight_logical_range()` | 选区覆盖的逻辑行整行高亮（主题 `surface.selection` 色） |
| `copy_to_clipboard()` | `std::thread::spawn` 独立线程写 arboard 剪贴板 |
| `extract_logical_range()` | 视觉行范围 → 逻辑行范围 → 纯文本拼接 |
| `highlight_cache` use_state | key=(generation, first, last) 缓存高亮结果，`write_no_update` |

**CPU 修复要点**：

- `MouseEventKind::Drag(Left)` 在消息区外直接 `Ignored` 返回
- 消息区内 Drag 通过 `DragThrottle` 节流（16ms 窗口），`write_no_update` 不触发 notifier.wake
- `wrap_map_cache` / `highlight_cache` / `total_rows_cache` 全部 `write_no_update`
- `build_wrap_map` 对每行调用 `Paragraph::line_count()`（O(N·W)），但仅 generation/width 变化时重建

**与修复 #1 草案的差异**：

- 坐标转换内联在事件处理器中（非单独 `mouse_visual_position` 函数）
- 文本提取用逻辑行整行提取（非 `extract_selected_text` 的字符级列截取）
- 高亮为整行逻辑行高亮（非字符级 split）
- 实现未使用 `text_selection::highlight_selected_lines` / `extract_selected_text`，改用自建的 `highlight_logical_range` / `extract_logical_range`

**涉及文件**：
- `peri-tui/src/kit/message_area.rs`：+250/-70 行（唯一修改文件）
- `peri-tui/src/kit/text_selection.rs`：0 行变更（仅使用了 `TextSelection` 结构体和 `line_to_plain_text` 函数）

**验证状态**：`cargo build` ✅，`cargo test -p peri-tui --lib` 394 passed ✅，`cargo test --workspace --lib` 963 passed（2 个已存在的 peri-middlewares 失败无关联）✅，需真实终端人工验证

