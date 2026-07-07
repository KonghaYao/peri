# 主输入框无法输入——MessageArea ScrollView 事件处理器消费所有键盘事件

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-04

## 问题描述

最近几个 git commit 之后，打开 Peri TUI 后主输入框（InputArea）在任何输入状态下都无法输入任何字符。键盘事件全部被 MessageArea 的 ScrollView 重构引入的事件处理器消费，InputArea 收不到任何按键。

## 症状详情

| 项目 | 内容 |
|------|------|
| 触发条件 | 必现，打开 TUI 后在任何输入状态下都无法输入 |
| 期望行为 | 在 InputArea 中输入字符、命令、Ctrl 组合键等正常响应 |
| 实际行为 | 没有任何输入响应，所有按键均无效 |
| 影响范围 | 所有键盘输入——普通字符、快捷键、方向键等均被拦截 |

**问题机制**（现象层面）：

1. MessageArea 的事件处理器注册了 `EventScope::Global, EventPriority::High`
2. 该处理器对所有 `Event::Key(_)` 和 `Event::Mouse(_)` 事件返回 `EventResult::Consumed`
3. 由于优先级为 High 且全局作用域，所有键盘事件在到达 InputArea 之前被 MessageArea 消费
4. 旧代码只消费 Ctrl+↑/↓/Home/End 和鼠标滚轮事件，其余事件返回 Ignored（允许事件继续传递到 InputArea）
5. 修复已在进行中（消息区事件处理器过滤逻辑已修正），但需要记录此 bug

### 现象 2（2026-07-07，ScrollView 残留在特定导航键上的键盘拦截）

修复 #1 解决了 Global/High 层的全局拦截问题后，普通字符和大部分快捷键已恢复使用，但以下键仍然被消息区捕获、无法到达 InputArea：

| 被拦截的键 | 效果 | 来源 |
|-----------|------|------|
| `↑` / `k` | scroll_up | `ScrollViewState::handle_event` Current/Normal handler |
| `↓` / `j` | scroll_down | 同上 |
| `←` / `h` | scroll_left | 同上 |
| `→` / `l` | scroll_right | 同上 |
| `PageUp` | scroll_page_up | 同上 |
| `PageDown` | scroll_page_down | 同上 |
| `Home`（无 Ctrl） | scroll_to_top | 同上 |
| `End`（无 Ctrl） | scroll_to_end | 同上 |

另外 Ctrl+Home/Ctrl+End 虽然触发滚动，但也可能因 `handle_event` 内部逻辑导致滚动幅度不可控。

**原因**：`ratatui-kit` `ScrollView` 组件在 `Current/Normal` 优先级注册了内置 `handle_event` handler（`scroll_view/mod.rs:224-235`），修复 #1 只覆盖了 Global/High 层，未覆盖此层。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 焦点在主输入框（默认状态）
  3. 尝试输入任意字符 → 无响应
- **环境**：macOS，ratatui-kit 0.7.2

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— MessageArea 的 ScrollView 重构在 commit `64033e3a` 中引入了事件处理器 bug
- `ratatui-kit` `crates/ratatui-kit/src/components/scroll_view/state.rs:167-174` —— `ScrollViewState::handle_event` 消费 j/k/h/l/↑/↓/PageUp/PageDown/Home/End
- `ratatui-kit` `crates/ratatui-kit/src/components/scroll_view/mod.rs:224-235` —— ScrollView 组件注册 Current/Normal 内置 handler

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 |
| 2026-07-07 | Open | Fixed | agent | 修复 #2：ScrollView active=false + 显式按键匹配 |

## 修复记录

### 修复 #1（2026-07-04）

- **操作人**：agent
- **用户原意**：消息区不应拦截键盘输入
- **修复内容**：MessageArea Global/High 事件处理器改为仅在 `message_accepts_key` 命中时返回 Consumed，其余返回 Ignored
- **涉及 commit**：64033e3a 附近
- **验证状态**：已验证（全局拦截已修复）

### 修复 #2（2026-07-07）

- **操作人**：agent
- **用户原意**：消息区滚动不应拦截 j/k/h/l/↑/↓/PageUp/PageDown/Home/End 等键——这些键应从输入框输入
- **根因**：`ratatui-kit` `ScrollView` 组件的 `ScrollViewState::handle_event`（`scroll_view/state.rs:167-174`）注册在 `Current/Normal` 优先级，消费了 `↑`/`k`(scrollUp)、`↓`/`j`(scrollDown)、`←`/`h`(scrollLeft)、`→`/`l`(scrollRight)、`PageUp`、`PageDown`、`Home`、`End`。修复 #1 只解决了 Global/High 层的问题，Current/Normal 层的 ScrollView 内置 handler 仍会消费这些键
- **修复内容**：
  1. ScrollView 传 `active: false` 关闭其内置键盘/鼠标事件 handler（鼠标滚动由 Global/High handler 单独接管，不受影响）
  2. 显式键盘 handler 用 `scroll_up()`/`scroll_down()`/`scroll_to_top()`/`scroll_to_bottom()` 替代 `handle_event(&event)`，仅匹配 `Ctrl+↑↓HomeEnd`
  3. 清理死代码：`let _ = focus_router::message_accepts_key(key)` 空调用 + `use crate::kit::focus_router` import
- **涉及文件**：
  - `peri-tui/src/kit/message_area.rs` —— 两处修改（ScrollView active=false + 显式 key match）
- **验证状态**：待验证
