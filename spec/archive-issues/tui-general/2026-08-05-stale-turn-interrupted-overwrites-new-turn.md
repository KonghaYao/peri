> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-stale-turn-interrupted-overwrites-new-turn.md

# 新 turn 的 loading/输入状态会被旧 turn 的 TurnInterrupted 事件污染（事件无 turn 代际防护）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-05

## 问题描述

用户取消旧 turn（Ctrl+C）后立即提交新 prompt，旧 turn 的 `TurnInterrupted` 事件（notifier 异步投递）与新 turn 的本地提交事件汇入同一个 bridge channel，**到达顺序不确定**。若新提交事件先入队，随后到达的 stale `TurnInterrupted` 会把**新 turn** 的 `is_loading` 清零；新 turn 零产出时还会误删新用户气泡、误恢复旧输入文本、清空 `INPUT_BUFFER`。bridge 只按 `session_id` 过滤事件，**无 turn 代际（generation）防护**。

> **对抗验证修正（2026-08-05）**：竞态主角实为 **`LocalUserBubble`**（`input_area.rs:907-940`：`is_loading=true` 时新提交只发本地气泡 + 推 `INPUT_BUFFER`，**不发** `SubmitRequest`/`PromptSubmitted`），而非 `PromptSubmitted`；`is_loading` 存在部分自愈（新 turn 首个流式 chunk 经 `streaming.rs:15,26,61,77` 重设 `phase=PromptRunning`）。最严重形态（删新气泡）需旧 turn **零产出**。严重级别由 P1 校准为 **P2**（见状态变更记录）。

来源：cancel 链路三方并行审查（Agent 2 确认，P1）。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| 新 turn 的 loading 被错误清零 | `TurnInterrupted` 处理（`acp_notifier.rs:232-247`）无条件将 `is_loading=false`，与新 turn 的 `is_loading=true` 竞争，顺序不定 |
| 零产出时新用户气泡被误删、输入被误回填 | `turn.rs:58-82` 的 `last_submitted_text` 在提交新 prompt 时已被覆盖，stale 事件回滚会删除错误的气泡并恢复已过期的文本 |
| `INPUT_BUFFER` 被清空 | `turn.rs:84-97` 零产出分支 `clear()` 清空排队输入，但该输入可能属于新 turn |

## 复现条件

- **复现频率**：偶发（依赖事件到达顺序；用户快速"取消→重发"时概率显著）
- **触发步骤**：
  1. turn A 运行中，用户按 Ctrl+C 取消
  2. 在 turn A 的 `TurnInterrupted` 事件到达 TUI 之前，用户立即提交新 prompt（turn B）
  3. 新提交事件先入队 → turn B 正常开始（loading=true）
  4. stale `TurnInterrupted` 随后到达 → turn B 的 `is_loading` 被清零、输入区状态被回滚
- **环境**：TUI 主进程（cancel 走 `CANCEL_TX → cancel_consumer → session/cancel`，事件经 `LOCAL_EVENT_TX → notifier → bridge_tx` 异步回流）

## 根因分析

1. **事件无代际标识**：`TurnInterrupted` 等 turn 结束事件不携带 turn 序号/generation，bridge（`acp_bridge.rs`）与 `turn.rs` 的 handler 只能按 `session_id` 过滤（`2026-07-25-stale-v2-events-bypass-session-filter` 已解决跨 session 的同类问题，但**同 session 内跨 turn** 无防护）。
2. **双通道汇合**：`PromptSubmitted` 走 submit_consumer 同步发 `LOCAL_EVENT_TX`；`TurnInterrupted` 走 notifier 异步收 ACP 事件再转发。两个通道在 bridge 汇合，无统一排序。
3. **回滚副作用无归属检查**：`turn.rs:58-98` 的 UI 回滚（删气泡/恢复文本/清 buffer）不检查"该事件是否仍属于当前 turn"，stale 事件直接作用于新状态。

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs:232-247` —— `TurnInterrupted` 事件处理（无条件清 `is_loading`）
- `peri-tui/src/kit/turn.rs:58-98` —— UI 回滚逻辑（`last_submitted_text` 覆盖、`INPUT_BUFFER` 清理、零产出分支）
- `peri-tui/src/kit/acp_bridge.rs` / `v2_bridge.rs` —— 事件汇合与 phase/variant/is_loading 更新（session 级过滤，无 turn 代际）
- `peri-tui/src/kit/submit_consumer.rs` —— `PromptSubmitted` 同步投递路径

## 修复方向

1. **turn 代际（generation）机制**：TUI 维护单调递增的 turn 序号，`PromptSubmitted` 时递增并携带；bridge/handler 对 turn 结束类事件（`TurnInterrupted`/`TurnDone`/`TurnSuspended`）校验代际，stale（< 当前代际）直接丢弃。
2. **回滚前置校验**：`turn.rs` 的 UI 回滚在执行前检查事件所属 turn 仍为当前 turn。
3. 补测试：新提交事件先入队、stale `TurnInterrupted` 后到达的场景（现有测试无此竞态覆盖）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（来源：cancel 链路三方并行审查，P1；待对抗验证） |
| 2026-08-05 | Open | Open | agent | 对抗验证：**部分证实，P1→P2**。竞态主角修正为 LocalUserBubble（is_loading gate 阻断 SubmitRequest）；is_loading 有流式自愈；消息丢失形态需旧 turn 零产出。确认：事件汇合（entry.rs:300-326）、无代际防护（acp_bridge.rs:161-172 仅 session 过滤）、无条件清 loading（turn.rs:78,95）、无竞态测试。补充：INPUT_BUFFER 归档分支不清（排队输入在下一 TurnDone 被 drain_input_buffer 意外提交）、零产出分支只删最后气泡却清空全部 buffer、last_submitted_text 跨 turn 残留、设计文档（tui-acp-data-flow.md:812）承诺的 BRIDGE_RESET_COUNTER 防护实际未实现 |
| 2026-08-05 | Open | Open | agent | 修复 v1（turn 代际机制：BridgeState.turn_generation/last_prompt_generation；stale TurnInterrupted 跳回滚；归档分支清 buffer；TurnDone 清 last_submitted_text；+4 测试；peri-tui 839 passed） |
| 2026-08-05 | Open | Open | agent | 修复验证：**不通过**。stale 判定（turn_generation > last_prompt_generation）只覆盖"气泡到、RPC 未发"的排队分支；当新提交 PromptSubmitted 先到（本地 channel 微秒级）而旧 TurnInterrupted 晚到（服务器往返数百 ms，主导排序）时判定 is_stale=false → 仍走零产出回滚删新气泡——核心验收未达成，且 turn.rs:63-65 注释声称的能力代码不具备。stale 分支无条件清 INPUT_BUFFER 与验收冲突（取消后提交的新请求被静默丢弃，测试固化了该行为）。cancel 兜底复位（issue 2 修复）使完整路径变主导排序，两修复需合并评审。待返工 |
| 2026-08-05 | Open | Open | agent | 修复 v2（协议层 request_id 配对 + v1 代际兜底 OR 组合，27 文件）：TUI 提交时生成 uuid v7 作 request_id 随 PromptSubmitted 与 session/prompt RPC 同发；服务器透传 SessionContext→SpawnPumpRequest→pump，AgentDone(cancelled) 回带；TurnInterrupted 判定 `stale = (request_id.is_some() && request_id != current_request_id) \|\| (代际)`。stale 分支**保留** INPUT_BUFFER 与 last_submitted_text（与 v1 相反，修复连续取消回滚）。+9 测试（主导排序核心回归 :602、排队分支、正常取消、None 降级、连续取消、pump/notifier/executor 链路）。peri-tui 850 / peri-acp 408 passed |
| 2026-08-05 | Open | Open | agent | 修复验证（workflow review）：**有条件通过**。主导排序核心验收测试真实且防假阳性（:650 显式断言 last_prompt_generation==2 锁定 v1 失效场景；id 判定双向锁定）。中：stale 分支保留的排队输入无提交触发器（取消后不提交新请求则悬挂，提交 C 后 B 在 C 完成后才 drain 提交，顺序反转）——设计取舍，建议后续 stale 分支复位后非空 buffer 主动 drain。低：current_request_id=None 时 id_mismatch 误判 stale（方向安全）；run_prompt 解析无直接单测；stale 归档不校验 current_turn 归属（无害）。待处理：排队输入提交触发器 |
| 2026-08-11 | Open | Fixed | agent | 归档：协议层 request_id 配对 + 代际兜底 OR 组合（27 文件，9 测试），修复记录见正文 |
