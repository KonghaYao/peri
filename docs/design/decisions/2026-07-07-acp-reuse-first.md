# Decision: ACP 协议复用优先 + 自定义事件全量审计

> 日期：2026-07-07 | 状态：已决议

## 决策

**标准有的走标准，真没有的才自定义。** `peri/unstable_event` 自定义事件仅用于 ACP v1 `session/update` 无法覆盖的场景。

## 原则

新增 Agent → TUI 事件时的判断流程：

1. 查 ACP v1 `SessionUpdate` 枚举是否有对应 tag
2. 有 → 走标准 `session/update` 通知，**不在 `peri/unstable_event` 重复定义**
3. 没有 → `peri/unstable_event` 自定义事件

## 全量审计（24 个自定义事件 → 保留 13 个）

| # | 事件名 | 标准等价物 | 裁决 | 理由 |
|---|--------|-----------|------|------|
| 1 | `text-chunk` | `agent_message_chunk` | 🗑 废弃 | 完全重复 |
| 2 | `reasoning-chunk` | `agent_thought_chunk` | 🗑 废弃 | 完全重复 |
| 3 | `tool-started` | `tool_call` | 🗑 废弃 | 完全重复 |
| 4 | `tool-ended` | `tool_call_update` | 🗑 废弃 | 完全重复 |
| 5 | `view-commit` | `session/update` 增量 + `turn-done` | 🗑 废弃 | 全量传输改增量边界 |
| 6 | `token-usage` | `usage_update`（meta 已含 input/output/model/cacheTokens） | 🗑 废弃 | 标准 meta 字段完全覆盖 |
| 7 | `hitl-pending` | `session/request_permission`（broker JSON-RPC） | 🗑 废弃 | 实际走 broker，从未作为事件产出 |
| 8 | `ask-user` | `elicitation/create`（broker JSON-RPC） | 🗑 废弃 | 同上 |
| 9 | `turn-done` | 无 | ✅ 保留 | 轮次结束信号 |
| 10 | `turn-interrupted` | 无 | ✅ 保留 | 中断信号 |
| 11 | `budget-warning` | `usage_update` 可承载但语义不同（警告 vs 信息） | ✅ 保留 | 独立警告事件 |
| 12 | `system-notification` | 无（`session_info_update` 语义不同） | ✅ 保留 | 文本通知 |
| 13 | `rewind-preview` | 无 | ✅ 保留 | 回退预览 |
| 14 | `subagent-started` | 无 | ✅ 保留 | SubAgent 生命周期 |
| 15 | `subagent-stopped` | 无 | ✅ 保留 | SubAgent 生命周期 |
| 16 | `bg-task-started` | 无 | ✅ 保留 | 后台任务生命周期 |
| 17 | `bg-task-completed` | 无 | ✅ 保留 | 后台任务生命周期 |
| 18 | `bg-task-cancelled` | 无 | ✅ 保留 | 后台任务生命周期 |
| 19 | `bg-task-snapshot` | 无 | ✅ 保留 | 后台任务快照 |
| 20 | `tool-count` | 可放入 `usage_update` meta | ⚠️ 待合并 | 不单独建事件 |
| 21 | `progress` | 可放入 `usage_update` meta | 🔲 预留 | 目前无数据源 |
| 22 | `prediction` | 无 | 🔲 预留 | 输入预测 |
| 23 | `file-suggestions` | 无 | 🔲 预留 | 文件补全 |
| 24 | `oauth-needed` | 无 | 🔲 预留 | MCP OAuth 授权 |

## 决策 B：`view-commit` 废弃，走增量模式

`view-commit` 每次传输全量 `ViewModel[]`，随着对话增长 payload 线性增大。改为复用标准 `session/update` 增量机制：

- **流式阶段**：`session/update` 逐条推 → TUI 增量构建 `current_turn`
- **边界信号**：`turn-done` / `turn-interrupted` → TUI 归档/丢弃 `current_turn`
- **首屏加载**：`session/replay` 逐条重放历史 `session/update`
- **compact/rewind 后**：重放变更后的消息，增量

详见 `spec/issues/2026-07-07-acp-protocol-refactor.md`。

## 待清理

### 废弃事件（8 个）

| 事件 | 涉及代码 |
|------|---------|
| `text-chunk` | `router.rs` 映射 + `event_data.rs::TextChunk` + `acp_types.rs` 解码 |
| `reasoning-chunk` | `router.rs` 映射 + `event_data.rs::ReasoningChunk` + `acp_types.rs` 解码 |
| `tool-started` | `router.rs` 映射 + `event_data.rs::ToolStarted` + `acp_types.rs` 解码 |
| `tool-ended` | `router.rs` 映射 + `event_data.rs::ToolEnded` + `acp_types.rs` 解码 |
| `view-commit` | `router.rs` 映射 + `event_data.rs::ViewCommit` + `view_mapper.rs`（整个文件）+ `acp_types.rs` 解码 + `session_load.rs` payload 构建 |
| `token-usage` | `router.rs` 映射 + `event_data.rs::TokenUsage` + `acp_types.rs` 解码（TUI 改为从 `usage_update` meta 读取） |
| `hitl-pending` | `event_data.rs::HitlPending` + `acp_types.rs` 解码（router 从未产出） |
| `ask-user` | `event_data.rs::AskUser` + `acp_types.rs` 解码（router 从未产出） |

### 文档更新

- `peri-acp-protocol.md`：§4.1 标注废弃，§4.2 移除 `view-commit`，移除 `hitl-pending`/`ask-user`
- `ACP_COMPATIBLE.csv`：移除废弃事件行
- `acp-feature-list.md`：功能状态清单已并入 `peri-acp-protocol.md`（2026-08-10 删除该文档）
