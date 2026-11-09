# P1：history v2 rewind 后实时流拒收与历史投影身份缺失

**状态**：已修复并验证（PR #270，head `ab91c70f4`）。
**优先级**：P1。
**类型**：回归（实时输出丢失、同文消息误删）。
**创建日期**：2026-11-03。
**来源**：PR #270 review 复核意见（最新提交 c1d9029）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 c1d902993bfcc1dd4724d601d801664b986abc24）。

## 确认证据

### P1-1 rewind 后实时输出被拒收

- `peri-tui/src/kit/acp_events/system.rs:268-274`：`request_close_current()` → `reset_state()`（`history_v2/mod.rs:174-177` 将 `HISTORY_V2_STATE` 置为 `default()`）→ `reset_consumer_state()` → `request_open_latest()`。
- open 结算：`consumer.rs:822-870` 在空状态下走 `initial::admit_new_session_open`（`initial.rs:239-251`）；`admit_open_response` 的 `paging` 来自 `previous.window`，为空时 `unwrap_or_default()`（`initial.rs:286-290`），`live.stream_id = None`；`live_boundary = previous.live_boundary` 同样为 `None`。
- 接纳：`live.rs:76-87` 在 `stream_id` 缺失时返回 `HistoryV2LiveAdmission::NotReady`；`live.rs:185-193` 对 `NotReady` 记 warn 并 `LiveGate::Skip`，`acp_notifier.rs:268-271` 整条更新不渲染。
- 全仓仅 `initial.rs:154` 写入 `paging.live`（仅 `session/load` initial 建立边界）；open/page/seek 均不写。`HistoryWindowPage` 无 `liveBoundary` 字段。

### P1-2 历史投影缺稳定 message 身份

- 用户气泡：`peri-tui/src/kit/acp_events/history_v2.rs:282-286` 用 `TuiUserBubble::new`，`message_id = None`（`bubble.rs:73-96`）。
- assistant：`history_v2.rs:287-289,298` 用 `committed_assistant_unit`，`message_id = None`（`turn.rs:469`）。
- live 侧身份：`turn.rs:389-394` 用 `UserInputDelivered.input_id`；assistant 用 `messageId`（`acp_notifier/session_update.rs:126-129,151-154`）。
- `entry_id` 格式为 `{session_id}:{message_uuid}`（`dispatch/session_replay.rs:61-63`、`history_v2_adapter/view.rs:349,362`），投影未读取；即使直接比较也与 live 裸 uuid 不相等。
- 后果：`live_unit_matches`（`history_v2.rs:161-198`）在双方 `message_id = None` 时退回 `(role, text)` 指纹。历史已有“继续”，用户再次发送“继续”，翻页/重开窗口时新消息可能被删除。

### P2-1 rewind 回填内容被当作 live 尾部

- `reset_consumer_state()` → `reset_view_projection()` 将 `HISTORY_V2_PREFIX_LEN` 置 0（`consumer.rs:260-270`）。
- `handle_rewind_completed` 从 `messages_json` 重建整段 `committed`（`system.rs:275-305`），但前缀长度保持 0。
- open 后 `replace_prefix` 取 `previous = min(0, committed.len()) = 0`（`history_v2.rs:89-100`），整段回填成为 suffix；`merge_live_tail` 仅在新页最后 12 个单元（`TAIL_MATCH_WINDOW`）内查找，回填首条通常匹配不上，整批保留，形成“新页 + 整批回退历史”。

### P2-2 工具卡片历史/live 身份不一致

- 历史侧 `invocation_key = {session_id}:{toolCallId}`（`history_v2_adapter/projection.rs:44`），TUI 直接作为 `tool_id`（`history_v2.rs:313-315`、`tool.rs:189`）。
- live 侧 `tool_id = toolCallId`（`acp_notifier/session_update.rs:170-174,205-209`）。
- `live_unit_matches` 对工具卡片在双方非空时要求全等（`history_v2.rs:173-179`），因此永远不匹配，重开窗口后保留重复卡片。

## 根因链

1. rewind 复位把连接级投递事实（streamId、已接纳 seq）与窗口状态一起清空；open 协议响应不携带投递边界，无法重建。
2. 历史投影只构造展示内容，不携带 `entry_id` 中的消息身份；去重只能退回文本指纹，同文消息无法区分。
3. rewind 回填的展示内容没有归属标记，被合并逻辑当作“未持久化 live 尾部”。
4. 历史与 live 的工具身份一个带会话前缀、一个不带，稳定身份比较失效。

## 影响面

- 用户影响：rewind 后新输出不再显示；同文重复消息在翻页/重开后可能消失；长会话回退后出现整批重复历史；工具卡片重开后重复。
- 受影响文件：`peri-tui/src/kit/acp_events/system.rs`、`history_v2.rs`、`turn.rs`、`peri-tui/src/kit/history_v2/mod.rs`、`initial.rs`、`consumer.rs`、`acp_notifier.rs`。
- 可规避：重新 `session/load`（切换会话）可恢复 live；其余问题无用户侧规避。

## 修复方案

- rewind 只作废窗口与投影，保留投递事实（streamId、已接纳 seq）；open 接纳时从保留的 `live_boundary` 重建 `paging.live`。
- rewind 期间（open 完成前）的 transcript 更新进入既有 pending 暂存；open 接纳成功后按序重放（复用 `replay_pending_initial` 的接纳与派发逻辑）。
- rewind 回填内容标记为“窗口前缀”：回填后设置 `HISTORY_V2_PREFIX_LEN = committed.len()`，新页整体替换回填内容，open 前新增的 live 单元仍作为尾部保留。
- 历史投影从 `entry_id` 解析消息 uuid，写入用户/assistant 气泡的 `message_id`；工具卡片在投影时把 `{session_id}:{toolCallId}` 归一为裸 `toolCallId`。
- 明确不做：不改 `entry_id` / `invocationKey` 协议形状；不放宽 `TAIL_MATCH_WINDOW`；不改 live 连续序号规则与 initial 校验。

## 回归测试

- `peri-tui`：load → rewind → open 接纳 → 新 live 事件被接纳；rewind 回填超过 12 个单元时新页整体替换；真实 `HistoryEntry` 投影带 `message_id`，同文不同 id 不被删除；历史工具卡与 live 卡按裸 toolCallId 去重。
- `peri-acp`：adapter 输出的 `invocationKey` 形状断言。

## 验收标准

1. rewind 完成后下一条实时 transcript 更新正常渲染，不进入 detached。
2. 历史中已有同文消息时，新发送的同文消息在翻页/重开后保留。
3. 长会话 rewind 后新窗口不叠加回退历史。
4. 重新打开窗口后同一工具调用只显示一张卡片。
5. 目标测试与 `check-changed` 通过。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_v2` → 85 passed, 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- rewind` → 41 passed, 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_v2_adapter` → 59 passed, 0 failed。
- `./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui -p peri-acp` → 通过。
- `bash scripts/check-changed.sh` → 通过。
- `git diff --check` → 无输出（exit 0）。

新增回归用例：`rewind_keeps_live_boundary_and_open_restores_admission`、
`rewind_backfill_is_replaced_by_the_opened_window`、
`historical_user_bubble_carries_message_id_and_keeps_same_text_live_tail`、
`historical_tool_card_uses_bare_tool_call_id_and_dedupes_live_card`。

head `ab91c70f4` 复核：`rewind` 目标用例 44 passed；上述四个用例定向运行全部通过；
`tool_call_and_result_share_invocation_key_across_entries` 通过。PR CI（macOS / Ubuntu /
Windows Build、E2E replay）在该 head 全部 pass。
