# 消息区滚动条缺少拖拽、箭头点击和 Human 消息刻度标记


> 归档于 2026-07-20，原路径 spec/issues/2026-07-07-message-area-scrollbar-interaction.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

当前消息区的滚动条只支持滚轮滚动和键盘快捷键（Ctrl+U/D/Home/End），但不支持以下交互：

1. **滚动条无法拖动**：thumb 只作为视觉指示器，不能用鼠标拖拽来快速定位
2. **▲/▼ 箭头无点击响应**：虽然 `clean_scrollbars()` 已配置 `begin_symbol("▲")` 和 `end_symbol("▼")`（`panel_registry.rs:489-504`），但 ratatui-kit `ScrollView` 组件不处理这些箭头区域的鼠标点击事件——用户点击 ▲/▼ 时没有任何反应
3. **无消息位置标记**：对于长对话，用户无法快速知道自己的消息（Human message）大致在什么位置，只能盲目滚动查找

这些交互是 TUI 滚动条的基础功能，缺失导致长对话场景下定位效率低下。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 鼠标拖拽滚动条 thumb | thumb 跟随鼠标移动，内容同步滚动到对应位置 | 无反应（拖拽被当作文本选区起点） |
| 点击滚动条顶部的 ▲ 箭头 | 内容自动跳到顶部 | 无反应 |
| 点击滚动条底部的 ▼ 箭头 | 内容自动跳到底部 | 无反应 |
| 在长对话中寻找自己的消息 | 滚动条上有刻度标记，看一眼就能知道 Human 消息在哪些位置 | 只能一行行滚轮盲找 |

## 复现条件

- **复现频率**：必现（当前实现不支持这些交互）
- **触发步骤**：
  1. 启动 TUI，产生足够多的对话消息使滚动条出现（消息超过一屏）
  2. 尝试用鼠标拖拽滚动条的 thumb → 无反应
  3. 点击滚动条顶部 ▲ 或底部 ▼ → 无反应
  4. 观察滚动条 track → 没有任何消息位置标记
- **环境**：macOS，ratatui-kit 架构

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 消息区渲染与鼠标事件处理，需提供 Human 消息位置数据，配合 ratatui-kit 变更处理拖拽/点击事件
- `peri-tui/src/kit/panel_registry.rs:486-508` —— `clean_scrollbars()` 已配置 ▲/▼ 符号，但无事件处理支持
- `peri-tui/src/kit/render_bridge.rs` —— 可能需在 `RenderCache` 中新增 Human 消息的视觉行位置映射，供刻度标记渲染使用
- ratatui-kit（git fork: `KonghaYao/ratatui-kit@45b9b3a`）：
  - `components/scroll_view/state.rs` —— `ScrollViewState` 需新增 `set_scroll_y(offset)` 方法
  - `components/scroll_view/scrollbars.rs` —— `ScrollBars::render_ref` 需暴露滚动条几何信息（track 区域、thumb 位置、按钮区域）、处理鼠标拖拽/点击、支持刻度标记渲染
  - `components/scroll_view/mod.rs` —— `ScrollView` 组件需集成新的交互事件处理
  - `widgets/scrollbar.rs`（ratatui 内置）—— 可能需要扩展以支持刻度标记的自定义渲染

## 期望改进方向

1. **在 ratatui-kit 层面扩展滚动条交互功能**（不在 message_area 自建滚动条渲染），具体：
   - `ScrollViewState` 增加 `set_scroll_y(offset: u16)` 用于程序化定位
   - `ScrollBars` / `ScrollView` 组件暴露滚动条几何信息，在 `MouseEventKind::Down(MouseButton::Left)` 时判断是否命中 track 区域，若命中则进入拖拽模式
   - 拖拽过程中（`Drag(MouseButton::Left)`），根据鼠标在 track 上的位置按比例计算滚动偏移
   - 点击 ▲/▼ 箭头区域时，调用 `scroll_to_top()` / `scroll_to_bottom()`
   - 支持通过 `ScrollBars` 配置传入刻度标记位置列表（如 `Vec<u16>` 表示相对偏移），在 scrollbar track 上用小色块渲染

2. **在消息区提供 Human 消息位置数据**：
   - 在 `RenderCache` 或 `message_area` 中识别 Human 消息对应的视觉行号
   - 将这些位置传给滚动条的刻度标记渲染

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
