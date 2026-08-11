> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-rewind-popup-selection-out-of-window.md

# rewind 候选超过 8 条时键盘选择可移出渲染窗口，Enter 回退不可见目标

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

rewind 候选弹窗只渲染前 8 条（`take(8)`、`scroll_start: 0`），但键盘导航用 `next_selection(*s, msg_count)` 允许选中到 `msg_count - 1`。候选超过 8 条时，选中标记移出可视区，Enter 仍会向一个用户看不到的目标发送 Preview/执行。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- rewind_popup.rs 约 83-93 行：`msg_rendered = msg_count.min(8)`，`visible_items = msg_rendered`，`scroll_start: 0`。
- 约 153 行：`next_selection(*s, msg_count)` 上限为全部候选数；约 148 行 `previous_selection` 无钳制。
- 约 156-170 行：Confirm 用 `p.messages.get(*msg_sel.read())` 取目标——索引 ≥8 时指向不可见候选。
- 鼠标路径（`hit_item` + `msg_layout`）被限制在 8 条内，仅键盘路径越界。

## 复现条件

- **复现频率**：候选 >8 条时必现
- **触发步骤**：
  1. 在长会话中双击 Esc 打开 rewind 弹窗（候选 ≥9 条）
  2. 连续按 ↓ 超过 8 次
  3. 选中标记消失，按 Enter
- **环境**：候选超过 8 条的会话

## 期望改进方向

- 将键盘导航上限钳制到渲染窗口（`msg_count.min(8)`），或让滚动窗口跟随选中项移动。

## 涉及文件

- `peri-tui/src/kit/popups/rewind_popup.rs` —— 行布局（约 82-93 行）、键盘导航（约 145-155 行）、渲染（约 345 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: MoveDown 钳制到渲染窗口上限 msg_rendered（msg_count.min(8)），选中不再移出可视区 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：键盘选择钳制到渲染窗口，修复记录见正文 |

## 修复记录

**改动摘要**（`peri-tui/src/kit/popups/rewind_popup.rs`）：

键盘导航 MoveDown 分支（约 153 行）上限从 `msg_count` 改为 `msg_rendered`（即 `msg_count.min(8)`，与渲染 `take(8)` 窗口一致）：

```rust
*s = next_selection(*s, msg_rendered);
```

- `msg_rendered` 在闭包外（约 83 行）已定义，move 闭包按值捕获，类型 usize；
- 钳制后选中索引恒在 `[0, min(8, msg_count))` 内，渲染循环（`take(8)` + `is_selected = i == msg_sel`）必能显示选中标记；Confirm 分支 `p.messages.get(*msg_sel.read())` 不再可能取到屏外索引（候选刷新变少时 get 返回 None 安全降级，不发送 Preview）；
- `previous_selection` 上界为 0 本身安全，未改；鼠标路径本就受 `visible_items: msg_rendered` 限制，未改。

**验证**：`cargo check -p peri-tui --all-targets` 通过（无警告）；`cargo test -p peri-tui --lib -- rewind` 36 passed。
