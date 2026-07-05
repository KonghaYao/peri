# 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-05

## 问题描述

用户在 TUI 中输入文本并 Enter 提交后，消息区不会立即显示用户输入的气泡，必须等到工具调用或 agent 输出内容后才出现，中间存在明显空白窗口期。同时 loading 状态可能永久卡死（如 prompt RPC 失败后无人清 loading），导致用户感觉输入框被锁定、无法继续使用。此外 history 浏览时无法从历史项回到空输入状态，给用户"卡死"的体感。

## 症状详情

### 症状 1：Enter 后用户输入不立即显示

- **现象**：用户按 Enter 提交文本后，消息区短暂空白（只有 `◜ 思考中…`），直到第一次 agent 输出或工具调用时才出现用户气泡
- **根因分析**：`InputArea.submit_text()` 直接写 `VIEW_MODELS.committed`（追加 UserBubble），但 `MessageArea` 渲染时只看 `RENDER_CACHE`。`render_bridge` 是异步 task，只在收到 ACP 事件时刷新缓存，本地提交没有触发渲染刷新
- **涉及文件**：`input_area.rs`、`message_area.rs`、`render_bridge.rs`

### 症状 2：loading 状态可能永久卡死

- **现象**：prompt RPC 失败或 notifier 丢掉生命周期事件时，`ACP_STATE.is_loading = true` 无法回落到 `false`，loading 样式永久保持，输入框表现像被锁住
- **根因分析**：
  - `prompt()` 失败时 `submit_consumer` 只打日志不清 loading
  - `is_loading` 被 `InputArea` 和 `acp_bridge` 多方写入，缺少唯一事实源
- **涉及文件**：`submit_consumer.rs`、`input_area.rs`、`acp_events.rs`

### 症状 3：history 浏览后无法回到编辑态

- **现象**：用户 Up 键浏览历史后按 Down 回到底部，历史项仍留在输入框里，无法清空为空白输入
- **根因分析**：`history_up()` 只在当前草稿非空白时才保存 `DRAFT`。用户本来输入框是空的，`DRAFT = None`，`history_down()` 回到底部时返回 `None`，编辑器未能清空文本
- **涉及文件**：`input_history.rs`、`input_area.rs`

### 症状 4：loading 时提交的输入不显示，等 TurnDone 才出现

- **现象**：agent 运行中用户 Enter 提交新输入，文字被放入 `INPUT_BUFFER` 队列，但本地没有立即回显。必须等上一轮 `TurnDone` 才会在消息区出现、才会真正发送
- **根因分析**：`submit_text()` 在 loading 时只 push 到 `INPUT_BUFFER`，不写 ViewModels 也不刷新 RENDER_CACHE。`TurnDone` 才一次性为缓冲输入创建 UserBubble
- **涉及文件**：`input_area.rs`、`acp_events.rs`

## 复现条件

- **复现频率**：必现（症状 1/3/4 每次触发均可复现）；偶发（症状 2 在 prompt 失败或网络波动时出现）
- **触发步骤**：
  1. 症状 1/2：TUI 中输入任意文本，Enter 提交 → 观察消息区空白
  2. 症状 3：输入两行文本并提交两次，Up 键浏览历史，持续 Down 到底部 → 输入框保留最后一条历史
  3. 症状 4：agent 正在输出时继续 Enter 输入 → 观察消息区没有回显
- **环境**：任意模型、macOS/Linux

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/input_area.rs` | 提交逻辑、本地 echo、RENDER_CACHE 同步 |
| `peri-tui/src/kit/input_history.rs` | history 草稿保存/恢复 |
| `peri-tui/src/kit/submit_consumer.rs` | prompt 失败时的 loading 清理 |
| `peri-tui/src/kit/render_bridge.rs` | 渲染缓存桥——仅响应 ACP 事件，不含本地提交 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-05 | Open | Fixed | agent | 已完成修复，等待用户验证 |

## 修复记录

### 修复 #1（2026-07-05）

- **操作人**：agent
- **用户原意**：提交后用户输入应立即可见，loading 不应卡死，history 回到底部应能恢复空输入，loading 中提交的消息应即时回显
- **修复内容**：
  - `input_area.rs`：本地提交后同步刷新 `RENDER_CACHE`，loading 时提交也即时本地回显 UserBubble
  - `input_history.rs`：`history_up()` 始终保存草稿（包括空串），回到底部时正确恢复空输入
  - `submit_consumer.rs`：`prompt()` 失败后调用 `clear_loading_state()` 防止 loading 永久卡死
- **涉及 commit**：未提交（工作区改动中）
- **验证状态**：待验证
