> 归档于 2026-08-11，原路径 spec/issues/2026-07-25-stale-v2-events-bypass-session-filter.md

# 旧会话的 v2 直连事件可能写入当前 TUI

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-25
**来源**：2026-07-24 架构审查 A3；过程文档已删除，本归档 issue 保留结论

## 问题描述

ACP 通知路径会携带 session ID，`acp_bridge` 可据此丢弃旧 session 的晚到事件；v2 直连路径却将 `active_session_id` 设置为空字符串，而过滤逻辑只检查非空 session。用户切换 thread、执行 `/clear`、恢复会话或旧异步任务晚到时，旧 turn 的 v2 事件可能绕过 stale-session 过滤并写入当前 `BridgeState`。期望所有事件路径都具有不可为空的 session identity 和可区分 reset 前后的 epoch。

> 当前证据来自静态架构审查；项目主要采用单活跃 session，尚未证明生产环境必然出现污染，但过滤缺口在代码路径上已存在。

## 症状详情

- ACP 路径把通知 session 包装进 `AcpEventWithEpoch.active_session_id`。
- `acp_bridge` 仅在事件 session 非空且与 `ACTIVE_SESSION_ID` 不同时丢弃事件。
- `process_v2_event` 为直连事件写入 `String::new()`，因此该路径跳过上述过滤。
- `V2_EVENT_TX` 是进程级 `OnceLock` sender，每个 prompt 都 clone 同一发送端。
- 会话切换和清理依赖 `ACTIVE_SESSION_ID`、`BRIDGE_RESET_COUNTER` 与空值约定共同维持隔离。

可能表现为：

- 切换 thread 后旧消息重新出现在新会话；
- `/clear` 后旧流式内容、tool card 或 SystemNote 回流；
- 已结束 turn 的晚到事件让 loading 状态复活；
- 未来并发 session 共享一个全局 sender，无法表达目标 session。

## 复现条件

- **复现频率**：需要旧 sender 在 session switch/reset 后继续发送，属于确定性竞态窗口；实际频率未测量。
- **触发步骤**：
  1. 启动 session A 并保留其 v2 event sender。
  2. 切换到 session B，或在 session A 执行 `/clear` 触发 bridge reset。
  3. 使用旧 sender 发送 Text、Tool、Turn 或 StateSnapshot 事件。
  4. 检查新 `BridgeState`、`VIEW_MODELS`、loading 与 tool cards 是否被旧事件修改。
- **环境**：TUI v2 直连路径；审查基线 `ce682d53`。

## 目标状态

- session identity 是事件 transport 的必填字段，不再使用空字符串表示“无 session”或“绕过过滤”。
- 每次 session reset 生成可单调比较或唯一标识的 epoch；同一 session ID 的 reset 前后事件可区分。
- event sink/sender 与 session 绑定，或由路由器在发送时强制提供 `(session_id, epoch)`。
- 所有 TUI 入口在修改 reducer state 或 atom 前执行同一套 stale-event 判定。

## 验收标准

- [ ] v2 直连事件携带非空、typed `session_id` 与 epoch；构造缺失身份的事件会失败。
- [ ] ACP 与 in-process 路径复用同一 stale-session 判定，不存在空字符串 bypass。
- [ ] `/clear` 后由旧 sender 发送的事件被拒绝，当前 session 的新事件仍正常显示。
- [ ] thread switch 后 session A 的晚到 Text、Tool、Turn 和 StateSnapshot 均不会修改 session B。
- [ ] resume/load 场景能接受当前 epoch 的事件，并拒绝恢复前遗留 sender 的事件。
- [ ] stale drop 与缺失 identity 有独立 tracing/metric，且不记录敏感内容。
- [ ] 新增确定性竞态回归测试，不依赖 sleep 或真实网络。

## 非目标

- 不在本 issue 中实现多 session 并发 UI。
- 不要求立即移除 ACP 或 v2 任一路 transport。
- 不负责定义所有事件的 delivery class。

## 关联 Issue

- `spec/issues/2026-07-25-event-identity-diverges-across-dual-delivery-paths.md` —— canonical event identity；本 issue 可作为其 session/epoch 垂直切片先行修复。

## 涉及文件

- `peri-tui/src/kit/acp_types.rs` —— `AcpEventWithEpoch` 及 TUI 事件身份。
- `peri-tui/src/kit/acp_notifier.rs` —— ACP 通知中的 session 提取。
- `peri-tui/src/kit/v2_bridge.rs` —— 当前直连事件写入空 session 的位置。
- `peri-tui/src/kit/acp_bridge.rs` —— active session、reset 与 stale-event 过滤。
- `peri-tui/src/kit/atoms.rs` —— 全局 `V2_EVENT_TX`、session/reset atoms。
- `peri-tui/src/acp_server/prompt.rs` —— prompt 构建时克隆直连 sender。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A3 创建 |
| 2026-08-11 | Open | Fixed | agent | 归档：v2_bridge 双轨退役（3.0-M），stale session 过滤缺口随直连路径删除消除，修复记录见正文 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
