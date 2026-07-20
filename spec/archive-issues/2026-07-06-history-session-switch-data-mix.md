# History 面板切换 session 后消息区出现新旧数据混合


> 归档于 2026-07-20，原路径 spec/issues/2026-07-06-history-session-switch-data-mix.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-06

## 问题描述

在 History 面板（ThreadBrowser）中选择一个历史 session 并按 Enter 切换后，消息区显示的内容并非纯粹是该 session 的消息，而是混合了**上一个 session 的部分消息**和**当前 session 的消息**，两者出现在同一个滚动区域内。

表现形式：滚动到顶部可以看到旧 session 末尾的几条消息（UserBubble / AssistantBubble），滚动到下方则出现新 session 的消息。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板选择 session → 按 Enter 切换 |
| 实际表现 | 消息区内同时存在上一个 session 的消息和当前 session 的消息 |
| 期望表现 | 消息区仅显示当前 session 的消息 |
| 复现频率 | 每次切换必现 |
| 影响范围 | 所有 session 切换操作（包括 CLI -c/-r 启动） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 session A 中进行若干轮对话（产生消息历史）
  2. Ctrl+B 打开 History 面板
  3. 选择一个有历史消息的 session B，按 Enter
  4. 观察消息区→滚动到顶部可看到 session A 的旧消息，下方是 session B 的消息
- **环境**：macOS 26.5.1（推测影响所有平台）

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/thread_load_consumer.rs` | session 切换触发点——递增 BRIDGE_RESET_COUNTER 后调用 load_session RPC |
| `peri-tui/src/kit/acp_bridge.rs` | BRIDGE_RESET_COUNTER 检测→清空内部 BridgeState，但不立即推 VIEW_MODELS atom |
| `peri-tui/src/kit/acp_events.rs` | push_view_models 的 fallback 逻辑——当 committed 为空且 has_view_commit 为 false 时，从 atom 读取旧值 |
| `peri-tui/src/kit/render_bridge.rs` | 渲染缓存构建——只通过 VIEW_MODELS atom 指针变化检测更新，不感知 BRIDGE_RESET_COUNTER |
| `peri-acp/src/dispatch/session_load.rs` | 空 session（history 为空）时 build_session_view_commit_payload 返回 None，不发送 ViewCommit |
| `peri-tui/src/acp_server/requests.rs` | session/load 服务器端处理——发送 ViewCommit 通知的入口 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建 |

## 修复记录

（待修复）
