# /clear 清空消息区后约 1 秒，旧对话消息全部恢复


> 归档于 2026-07-20，原路径 spec/issues/2026-07-07-slash-clear-messages-reappear-after-1s.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-07

## 问题描述

执行 `/clear` 命令后，消息区域被清空（变为空白/Welcome 页面），但大约 1 秒之后，清空前的全部旧对话消息（用户消息、AI 回复、工具调用等）重新出现在消息区中。

## 症状详情

| 项目 | 内容 |
|------|------|
| 触发操作 | 在 InputArea 输入 `/clear` 并按 Enter |
| 即时表现 | 消息区清空，显示空白 |
| 延迟表现 | 约 1 秒后，旧对话消息全部恢复 |
| 恢复的内容 | 清空前存在的所有用户/AI/工具消息 |
| 期望行为 | 消息区清空后保持空白，进入新会话状态 |

### 现象 2（2026-07-08 Reopen）

ACP 协议化（废弃多个自定义事件、TUI 渲染迁移到标准 `session/update`）之后，用户反馈同一现象再次复现，表现与原现象完全一致。

| 项目 | 内容 |
|------|------|
| 触发操作 | InputArea 输入 `/clear` 并 Enter |
| 即时表现 | 消息区瞬间清空（与原现象一致） |
| 延迟表现 | 约 1 秒后，清空前全部旧对话消息重新出现（与原现象一致） |
| 频率 | 每次必现 |
| 代码背景 | TUI 已完全切换为基于 ACP 标准事件的状态维护（用户原话："现在改为了 acp 的状态维护"） |
| 相关近期 commit | `f0d41fa6`（refactor(acp): 废弃 11 个冗余自定义事件，复用标准 session/update）、`68e15875`（fix(tui): ACP 协议化后 TUI 渲染全面修复） |
| 期望行为 | 清空后保持空白，进入新会话状态 |

## 复现条件

- **复现频率**：每次都出现
- **触发步骤**：
  1. 在 Peri TUI 中进行若干轮对话（产生用户消息和 AI 回复）
  2. 在 InputArea 输入 `/clear` 并按 Enter
  3. 消息区短暂清空
  4. 约 1 秒后，旧对话消息全部恢复
- **环境**：macOS，peri-tui kit 单路径

## 涉及文件

- `peri-tui/src/kit/submit_consumer.rs` —— `/clear` 命令拦截和状态重置逻辑所在
- `peri-tui/src/kit/acp_bridge.rs` —— ACP 事件桥接，维护 `BridgeState`（committed/current_turn），消费 `BRIDGE_RESET_COUNTER`
- `peri-tui/src/kit/acp_events.rs` —— `push_view_models` / `dispatch_and_notify`，ViewCommit/TurnDone 事件处理

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |
| 2026-07-07 | Open | Fixed | agent | session/new 推空 ViewCommit，TUI 删除自定义 hack |
| 2026-07-08 | Fixed | Reopen | agent | ACP 协议化后同一现象再次复现（用户反馈"现在改为了 acp 的状态维护"） |

## 修复记录

### 修复 #1（2026-07-07）

- **操作人**：agent
- **用户原意**：/clear 后消息区清空保持空白，不再恢复旧消息；/history 线程加载正常
- **根因**：session/new 不推送 ViewCommit。TUI 用 BRIDGE_RESET_COUNTER 侧信道清空 bridge 状态，但旧 session 滞留事件在 pipe 中晚于 reset 到达，committed 被旧 ViewCommit 覆写。由于无新 ViewCommit 来覆盖，旧数据永久残留。
- **修复内容**：
  1. `peri-tui/src/acp_server/requests.rs`：session/new handler 推空 `ViewCommit { view_models: [] }`，利用 FIFO 管道排序在旧事件之后到达，自然覆盖清除。
  2. TUI 侧移除全部自定义 hack（SESSION_ACTION drain gate、generation guard、双次 BRIDGE_RESET_COUNTER 递增），桥接层回归纯 ACP 事件驱动。
- **涉及 commit**：`d53d63c3`
- **涉及文件**：`acp_server/requests.rs`、`kit/acp_bridge.rs`、`kit/acp_events.rs`、`kit/atoms.rs`、`kit/submit_consumer.rs`、`kit/thread_load_consumer.rs`
- **验证状态**：待验证
