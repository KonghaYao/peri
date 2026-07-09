# History 面板切换 session 后 loading 永久卡死，界面完全无响应

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-09
**类型**：Bug

## 问题描述

用户通过 History 面板（/history）选择历史 session 并按 Enter 切换后，消息区虽然显示了历史消息，但底部的 LoadingFooter（spinner 动画）持续转动不消失，整个界面完全失去响应——键盘输入无效、无法 Esc 退出、无法 Ctrl+C 打断。用户只能强制退出 TUI 进程恢复。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板（/history）中选择历史 session → 按 Enter 切换 |
| 实际表现 | 消息区显示了历史消息，但 LoadingFooter 一直转，输入框锁定，键盘无响应 |
| 期望表现 | 切换后正常显示历史对话，loading 消失，输入框恢复可输入状态 |
| 复现频率 | 每次切换必现 |
| 影响范围 | 所有历史 session（无论是否包含工具调用） |
| 恢复方式 | 只能强制退出 TUI 进程（Ctrl+C 直接关闭应用） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 TUI 中进行任意对话（产生历史 session）
  2. 输入 `/history` 打开 History 面板
  3. 用 ↑/↓ 选择一个历史 session
  4. 按 Enter 切换
  5. 观察：消息区出现历史消息，loading 持续转动，键盘无响应
- **环境**：macOS，任意模型

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs` —— `is_session_replay` 提取逻辑：ACP SDK 序列化 `meta` 为 `_meta`，但 notifier 读的是 `"meta"`（无下划线），导致 replay 事件 `periReplay` 从未被检测到
- `peri-tui/src/kit/acp_events.rs` —— `dispatch_and_notify` 中 `ToolStarted`/`TextChunk` 等流式事件会设 `phase = PromptRunning` → `is_loading = true`
- `peri-acp/src/dispatch/session_replay.rs` —— replay 生成 ACP 通知时正确设置了 `periReplay: true` meta，但 key 不匹配导致下游未识别
- `peri-tui/src/kit/thread_load_consumer.rs` —— History 面板 Enter 切换入口

## 根因分析

ACP SDK v1.4.0 的 `meta` 字段全部标注 `#[serde(rename = "_meta")]`（含下划线），符合 ACP 协议规范：

| 类型 | 序列化 key | 场景 |
|------|-----------|------|
| `ContentChunk` | `content._meta` | `agent_message_chunk` / `user_message_chunk` |
| `ToolCall` | `_meta` | `tool_call` |
| `ToolCallUpdate` | `_meta` | `tool_call_update` |

但 `acp_notifier.rs` 中 `is_session_replay` 提取链只用 `"meta"` 和 `"content.meta"`（均无下划线），永远找不到 `periReplay=true` 标记。后果：

1. replay 的 `tool_call` → 生成 `ToolStarted`（流式事件）→ `phase = PromptRunning`
2. replay 的 `agent_message_chunk` → 生成 `TextChunk`（流式事件）→ `phase = PromptRunning`
3. replay 的 `tool_call_update` → 生成 `ToolEnded`（流式事件）→ `phase = PromptRunning`
4. replay 全程无 `TurnDone` → `is_loading` 永久卡 true → loading 不消

诊断日志（`[LOADING_DIAG]`）精确印证了此链：`ToolStarted`/`TextChunk` 事件在 replay 期间出现且设 `is_loading=true`，`TurnDone` 从未到达。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |
| 2026-07-09 | Open | Fixed | agent | 根因定位 + 修复（见修复记录 #1） |

## 修复记录

### 修复 #1（2026-07-09）

- **操作人**：agent（systematic-debugging skill）
- **用户原意**：History 切换后 loading 不卡死，正常显示历史消息
- **根因**：ACP SDK `meta` 序列化为 `_meta`（含下划线），`acp_notifier.rs` 的 `is_session_replay` 提取链只用 `"meta"`（无下划线），导致 replay 事件 `periReplay=true` 标记从未被检测到。replay 事件被当作流式事件处理，`ToolStarted`/`TextChunk` 设 `phase = PromptRunning` → `is_loading = true`，无 `TurnDone` 兜底 → 永久 loading。
- **修复内容**：
  - `peri-tui/src/kit/acp_notifier.rs`：`is_session_replay` 提取链改为四级 fallback：`_meta` → `meta` → `content._meta` → `content.meta`
  - `peri-tui/src/kit/acp_notifier.rs`：`tool_call` handler 添加 `is_session_replay` 分支 → `ReplayToolStarted`
  - `peri-tui/src/kit/acp_notifier.rs`：`tool_call_update` handler 添加 `is_session_replay` 分支 → `ReplayToolEnded`
- **涉及 commit**：working tree（待提交）
- **验证状态**：待验证
