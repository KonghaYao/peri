> 归档于 2026-07-18，原路径 spec/issues/2026-07-13-clear-scrollbar-persists-at-welcome.md
# /clear 后回到 Welcome 页面，滚动条仍然可见

**状态**：Fixed
**优先级**：低
**创建日期**：2026-07-13

## 问题描述

长会话下消息区有滚动条（内容超出视口），执行 `/clear` 后视图回到 Welcome 欢迎页面，但**视口右侧的滚动条仍然绘制在屏幕上**——此时 Welcome 页面没有任何可滚动内容，滚动条不应出现。

## 症状详情

| 场景 | 现象 |
|------|------|
| 长对话产生滚动条 → 执行 `/clear` | Welcome 页面显示正常，但滚动条 thumb/箭头仍绘制在消息区右侧 |
| Welcome 页面静止 | 滚动条保持僵尸状态，不响应鼠标/键盘滚动（因为没有可滚动内容） |
| /clear 后立即发送新消息 | 滚动条很快被新内容的 `run_auto_follow` → `scroll_to_bottom()` 重置为正确状态 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI，进行多轮对话使消息区内容超出视口高度，出现滚动条
  2. 执行 `/clear`
  3. 观察到 Welcome 页面出现，但右侧滚动条仍然可见
- **环境**：所有平台

## 根因分析

### 渲染路径

`MessageArea` 组件的渲染逻辑（`peri-tui/src/kit/message_area/mod.rs`）：

1. **第 76 行**：`empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty()`
2. **第 272-293 行**：当 `empty = true`，直接 `return Welcome(...)`，渲染 Welcome 页面，**跳过后续所有视口裁剪/滚动逻辑**
3. **第 341-348 行**（被跳过的代码）：更新 `scrollbar_fields`（`content_length`、`position`、`viewport_length`）
4. **第 143-145 行**：`ScrollbarHook` 在组件生命周期中注册，其 `post_component_draw` **每帧都会执行**，基于 `scrollbar_fields` 的值决定是否渲染滚动条

### 问题链路

```
/clear → push_view_models_for_reset()（VIEW_MODELS = 空）
       → MessageArea 重渲染：empty = true
       → 进入 Welcome 分支（Line 272），提前 return
       → scrollbar_fields 未被更新，仍保留旧会话的值（content_length=2000, viewport_length=40）
       → ScrollbarHook.post_component_draw 执行：
           content_length(2000) > viewport_length(40) ✓ → 渲染僵尸滚动条
```

### 关键代码

`props.rs:51-56`（`ScrollbarHook.post_component_draw`）：
```rust
fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
    let f = *self.fields.read();
    // 仅当内容超出视口时才渲染滚动条
    if f.content_length <= f.viewport_length {
        return;
    }
    // ... 绘制滚动条
}
```

`mod.rs:142-145`（Hook 注册——在 empty 分支之前）：
```rust
let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
hooks.use_hook(move || ScrollbarHook {
    fields: scrollbar_fields,
});
```

## 修复方案

在 Welcome 分支（`mod.rs:272`，`if empty { ... }` 内部），**提前 return 之前**将 `scrollbar_fields` 重置为默认值（`ScrollbarFields::default()`，即全零），使后续 `post_component_draw` 判定 `content_length(0) <= viewport_length(0)` 并跳过渲染。

### 最小改动

```rust
// mod.rs:272 行之前插入
if empty {
    // 重置滚动条字段，避免 Welcome 页面残留旧会话的滚动条
    *scrollbar_fields.write_no_update() = ScrollbarFields::default();
    // ... 原有的 Welcome 渲染逻辑
}
```

- 位置：`if empty {` 块首行，所有 return 路径之前
- 使用 `write_no_update()` 避免触发额外 re-render（不会产生自激回路——`empty = true` 时渲染路径不变，Welcome 组件本身不依赖 `scrollbar_fields`）
- 仅触发一次（state 写入仅在 `empty` 从 false 变 true 时执行）

### 风险

- **零风险**：`ScrollbarFields` 重置为全零仅在 Welcome 页面期间生效。用户发送新消息后，正常渲染路径会重新计算正确的 `content_length` / `viewport_length` / `position`
- 不影响已有的 `run_auto_follow` → `scroll_to_bottom()` 行为（新消息到来时仍会正确复位）
- 不影响 `scrollbar_drag` 状态（独立的 `use_state`）

## 相关文件

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/message_area/mod.rs` | `MessageArea` 组件本体，需在 Welcome 分支前重置 `scrollbar_fields` |
| `peri-tui/src/kit/message_area/props.rs` | `ScrollbarFields` / `ScrollbarHook` 定义 |
