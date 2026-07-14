# 用户发送 prompt 后消息区不自动跳转到最底部

**状态**：Open
**优先级**：中
**创建日期**：2026-07-13
**类型**：Bug

## 问题描述

用户发送 prompt（按 Enter 提交输入）后，消息区不会立即跳转到最底部。用户刚发送的 UserBubble 可能不在视口内，需要手动滚动才能看到自己的消息。一旦 agent 开始流式输出，自动吸底恢复正常。

**期望行为**：用户提交 prompt 是一个有意的操作，消息区应立即 `scroll_to_bottom()`，让用户看到自己的消息出现在底部。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | 在消息区输入框中输入文本，按 Enter 提交 |
| 实际表现 | 消息区滚动位置不变，如果用户之前往上滚过历史，UserBubble 可能在视口上方不可见 |
| 期望表现 | 提交后立即跳到底部，UserBubble 和后续的 loading spinner 在视口最下方可见 |
| 流式阶段表现 | **正常**——agent 开始流式输出后，`is_loading` 分支的 proximity 检测能正常吸底跟随 |
| 复现频率 | 100% 必现（只要提交瞬间页面不在底部） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI，进行一次对话让消息区有足够内容
  2. 往上滚动回看历史消息（离开底部）
  3. 输入新 prompt，按 Enter 提交
  4. 观察：消息区停留在回看的位置，不会跳到 UserBubble / loading spinner 所在底部
  5. 等 agent 开始流式输出，才逐渐自动滚到底部
- **环境**：macOS，ratatui-kit 架构

## 涉及文件

- `peri-tui/src/kit/message_area/scroll.rs:525-543` —— `run_auto_follow` 的 `is_loading` 分支通过 proximity 检测（`distance <= vis_height/4`）决定是否 `scroll_to_bottom()`，无"用户主动提交"的强制滚底信号
- `peri-tui/src/kit/submit_consumer.rs:163-168` —— `handle_agent_text_submit` 设置 `is_loading = true` + 递增 `LOADING_EPOCH`，但此时 `VIEW_MODELS` 尚未包含 UserBubble（prompt RPC 还在飞行中）
- `peri-tui/src/kit/message_area/mod.rs` —— `use_effect` deps = `(items_len, vm_generation, is_loading)`，第一次 effect 触发时 `is_loading=true` 但 `items_len` 未变（UserBubble 尚未到达），第二次触发时 proximity guard 生效

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | deepseek-v4-pro | 创建（issue-create skill） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加）
