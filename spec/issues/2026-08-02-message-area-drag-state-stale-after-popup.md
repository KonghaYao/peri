# 消息区被弹窗遮挡时提前返回，拖拽/滚动条状态残留

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`message_area/scroll.rs` 的鼠标处理在 `mouse_router::is_occluded()`（弹窗/面板遮挡）时提前 `return Ignored`，跳过下方所有状态复位逻辑。若遮挡发生在拖拽中途（滚动条拖拽或文本选区拖拽），`scrollbar_drag.active`、`selection_down_pos`、`text_sel.dragging` 等状态不会被清除，弹窗关闭后残留状态影响下一次交互。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `scroll.rs` 约 249-253 行：`is_occluded()` 时直接 `return EventResult::Ignored`。
- 复位逻辑位于其后：`MouseEventKind::Up(MouseButton::Left)` 分支（约 363-465 行）负责清 `scrollbar_drag.active`、`selection_down_pos`、`text_sel.dragging`。
- 拖拽中弹出弹窗 → 鼠标松开事件被遮挡分支吃掉 → 状态残留：
  - 滚动条 `drag_active` 仍为 true，后续普通点击被误当拖拽处理（且几何计算依赖旧 area）；
  - 文本 `selection_down_pos`/`dragging` 残留，下一次点击可能触发误选中或复制。

## 复现条件

- **复现频率**：拖拽期间弹窗打开时（如拖选中文字时按快捷键打开弹窗）
- **触发步骤**：
  1. 在消息区按住左键拖拽（滚动条 thumb 或文本选区）
  2. 拖拽过程中打开任意弹窗（遮挡消息区）
  3. 松开鼠标，关闭弹窗
  4. 观察滚动条或选区进入异常状态（点击行为错乱、选中残留）
- **环境**：带遮挡路由（弹窗）的 TUI 会话

## 期望改进方向

- 遮挡分支仅跳过事件消费，不跳过状态清理；或在进入遮挡时显式复位拖拽相关状态。

## 涉及文件

- `peri-tui/src/kit/message_area/scroll.rs` —— `handle_event` 遮挡提前返回（约 249-253 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: 遮挡分支 return Ignored 前显式复位 scrollbar_drag.active、selection_down_pos，拖拽中 text_sel 用 clear() 全清 |

## 修复记录

**改动摘要**（`peri-tui/src/kit/message_area/scroll.rs` `handle_event`）：

`is_occluded()` 遮挡分支在 `return EventResult::Ignored` 前显式清理拖拽残留状态：
- `scrollbar_drag.write_no_update().active = false` —— render 不依赖 active，用 `write_no_update` 避免 wake 噪音（与现有拖拽记录逻辑一致）；
- `*selection_down_pos.write_no_update() = None`；
- `text_sel`：先 `text_sel.read().dragging` copy 出判断（规避 parking_lot 同 thread read+write panic），dragging 时调用 `text_sel.write().clear()`（start/end/dragging/selected_text 全清，与正常 Up 分支复制后的清理一致），用 `write()` 触发 wake 清除渲染高亮。

**text_sel 复位的风险说明**：已读 `TextSelection` 结构（`peri-tui/src/kit/text_selection.rs`），`clear()` 是安全的全清方法，与 Up 分支 `sel.clear()`（scroll.rs:465-469）完全一致；遮挡期间消息区不可见，弹窗关闭后无高亮/拖拽残留。唯一行为变化：遮挡前若已有完成选中（selected_text），clear 会一并清掉——但该选中在正常拖拽流程中复制后也会被 clear，且遮挡场景下不可见，风险可接受。故选择做完整复位而非跳过。

**验证**：`cargo check -p peri-tui --all-targets` 通过（无警告）；`cargo test -p peri-tui --lib -- message_area` 70 passed。

未改遮挡时放行事件给前景 handler 的语义（仍 `return EventResult::Ignored`）。
