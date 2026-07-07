# InputArea 鼠标点击光标快速定位——功能缺失

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

v2 ratatui-kit 架构迁移后（S13 删除 `src/event/mouse.rs`），InputArea 失去了鼠标点击→光标定位的能力。用户点击 composer 区域（输入框本体）中的文本，光标不跟随跳动。

### 旧代码（已删除）

原 `src/event/mouse.rs` 中有 `textarea_mouse_to_cursor` 和 `display_col_to_char_idx` 两个函数，配合旧 state machine 的 mouse dispatch 实现此功能。S13 清理时一并删除（commit `59d0574a`），ratatui-kit 架构下未重新实现。

## 尝试记录

### 尝试 1：Current scope + same event handler

在 `input_area.rs` 的 `use_event_handler(EventScope::Current, ...)` 闭包中新增 `Event::Mouse(Down(Left))` arm，使用 `AreaTracker` custom hook 追踪 composer 区域，坐标换算后移动光标。

**结果**：无效。`EventScope::Current` 不向 InputArea 分发鼠标事件（键盘事件正常，鼠标事件只有 `EventScope::Global` 才能收到）。

### 尝试 2：Global scope 独立 handler + Normal 优先级

新增独立 `use_event_handler(Global, Normal)`，仅处理鼠标点击。

**结果**：无效。`message_area.rs` 的 Global High 优先级 handler 对所有鼠标事件返回 `Consumed`（包括消息区外的点击），Normal 优先级 handler 永远排不到。

### 尝试 3：Global scope + High 优先级 + message_area 放行

- 将 InputArea 的 mouse handler 提升到 `Global + High`（与 message_area 同级）
- 修改 `message_area.rs`：鼠标在消息区外的 `Down(Left)` 返回 `Ignored` 而非 `Consumed`

**结果**：编译通过，运行无效。原因待排查——可能是以下之一：
- ratatui-kit 的 handler 排序逻辑中，同 priority 同 event 的情况下先注册的 handler Consumed 后后续 handler 不执行（注册序决定优先级）
- `AreaTracker` hook 的 `pre_component_draw` 捕获的区域坐标不准确
- `overlay_height` Arc 共享状态在 render body 和事件 handler 间的时序不一致

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/input_area.rs` | 当前有 mouse handler（Global+High），但不生效 |
| `peri-tui/src/kit/message_area.rs` | 已修改：消息区外 Down(Left) 改为 Ignored |
| `peri-widgets/src/textarea/state.rs` | `line_col_to_cursor` 可用于坐标换算 |
| 旧 `peri-tui/src/event/mouse.rs` | 参考实现（已删除，commit `59d0574a`） |

## 当前代码位置

`input_area.rs:350-405` — Global scope High priority mouse handler
`message_area.rs:493-495` — 消息区外 Down(Left) 返回 Ignored

## 可能方向

1. **不用 Global scope**：在 entry.rs 层面从 crossterm 直接读鼠标事件，跳过 ratatui-kit 的事件分发——但这破坏了分层
2. **使用 ratatui-kit `use_input_layer`**：创建 InputArea 专属的输入层，鼠标事件通过 Layer 路由——需要研究 ratatui-kit 的 Layer 机制是否支持鼠标
3. **在 message_area 的 handler 内调用 InputArea 的回调**：通过 atom 或 channel 通知 InputArea 有鼠标点击——耦合度高
4. **调试当前方案**：添加 tracing log 确认 handler 是否被调用、`AreaTracker` 坐标是否正确

## 根因与修复

### 根因

尝试 3 中 `area_rect` 每帧通过 `Arc::new(Mutex::new(None))` 重新创建，而 `AreaTracker` hook 的 `pre_component_draw` 写入的是第一帧的原始 Arc。从第 2 帧开始 mouse handler 读到的新 Arc 永远为 `None`，`if let Some(outer) = ...` 条件永不成立。

### 修复

将 `AreaTracker.rect` 从 `Arc<Mutex<Option<Rect>>>` 改为直接 `Option<Rect>`（值类型，`Copy`），仿照 `MsgAreaTracker` 模式：每帧从 hook 取出副本后释放 `&mut hooks` 借用，闭包按值捕获副本。

### 涉及改动

| 文件 | 变更 |
|------|------|
| `peri-tui/src/kit/input_area.rs` | `AreaTracker` 结构体 + hook 注册 + mouse handler 改为值拷贝模式（~10 行） |
| `peri-tui/src/kit/message_area.rs` | 消息区外 `Down(Left)` 返回 `Ignored` 而非 `Consumed`（+4 行） |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建，记录三次尝试 |
| 2026-07-07 | Open | Fixed | agent | 根因定位（Arc 重建）并修复 |
