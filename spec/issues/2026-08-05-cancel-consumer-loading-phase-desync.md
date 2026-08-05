# cancel_consumer 直接写 ACP_STATE 与 bridge phase 派生不同步（loading 闪回 + 提交判定竞态）

**状态**：Open
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S4.2
**关联 issue**：`2026-08-05-loading-stuck-after-transport-close.md`（is_loading 无看门狗，修复需协调兜底语义，不重复）

## 问题描述

`cancel_consumer` 取消时只写 `ACP_STATE.is_loading=false`（`submit_consumer.rs:432-440` `clear_loading_state`），bridge 内部 `state.phase` 仍为 `PromptRunning`。后续任意事件（ToolCount/Progress/迟到 TextChunk）触发 `push_acp_state`（`render.rs:82-96`）会用 phase 重算 `is_loading=true`——取消后 loading 重新亮起（服务端尚未停止时的正常流事件）或闪烁；若服务端取消成功但 TurnInterrupted 迟到，期间用户提交的输入被判定为"loading 中"而进入 INPUT_BUFFER。

## 症状详情

- 直接写 atom 是"transport 死亡/prompt task panic 时 TurnInterrupted 永不到达"的兜底（`submit_consumer.rs:432-436` 注释）——不能简单删除
- **同类未覆盖**：`handle_clear_submit` 两处直接写（`submit_consumer.rs:130,155`）、prompt 失败 `clear_loading_state`（`:67`）同款问题
- 与 `2026-08-05-loading-stuck-after-transport-close.md` 的边界：该 issue 是 transport 死后无看门狗（pump 退出后无事件可达）；本 issue 是取消瞬间的 phase 派生覆盖（pump 存活）

## 复现条件

- **复现频率**：偶发（取消后仍有流事件到达时）
- **触发步骤**：Ctrl+C 取消 → 服务端停止前的迟到事件 → loading 闪回；或取消后立即提交 → 误入 INPUT_BUFFER

## 涉及文件

- `peri-tui/src/kit/submit_consumer.rs:432-440` —— `clear_loading_state` 直接写 atom
- `peri-tui/src/kit/acp_events/render.rs:82-96` —— `push_acp_state` 由 phase 派生 is_loading
- `peri-tui/src/kit/submit_consumer.rs:130,155,67` —— 同类直接写

## 修复方向（对抗 review 补充）

1. **保留直接写兜底 + 注入事件双保险**（幂等）：直接写是 transport 死亡时的兜底，改走 `LOCAL_EVENT_TX` 依赖 bridge 存活——bridge 是独立 task 不受 transport 死亡影响，但 shutdown 路径（bridge 已退出）注入无人消费
2. **顺序定义**：先 cancel RPC（带超时）再复位——否则 cancel 期间服务端流尾巴又把 phase 拉回 PromptRunning，闪烁依旧
3. 需要新增内部事件变体（不能复用 `TurnInterrupted`——有回滚/清 buffer 副作用），成本含 dispatch + 测试矩阵
4. 顺带覆盖 `handle_clear_submit` 与 prompt 失败路径

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-tui 审查发现；对抗 review 补充兜底语义与顺序） |
| 2026-08-05 | Open | Fixed | agent | 修复：新增内部事件 LocalLoadingReset——clear_loading_state 双保险（直接写兜底 + 注入事件）；cancel 顺序改为先 cancel RPC（带超时）再复位；handle_clear_submit 两处直接写并入；补 3 个测试 |

## 修复记录

### 修复 #1（2026-08-05）

- **复位机制设计**：保留直接写 `ACP_STATE.is_loading=false` 兜底（bridge 已退出时唯一生效路径）+ 新增 TUI 内部事件 `LocalLoadingReset` 经 `LOCAL_EVENT_TX` 注入（bridge 存活时把 phase 从 PromptRunning 幂等复位为 Idle 并重推 ACP_STATE，防止后续事件 `push_acp_state` 用 phase 重算 is_loading=true）
- **顺序**：cancel_consumer 先 `tokio::time::timeout(2s, cancel RPC)` 再复位——cancel 完成前服务端流尾巴会把 phase 拉回 PromptRunning，复位在流停止后执行才稳定；transport 死亡时 cancel 挂起由超时兜底，复位必定执行
- **改动文件**：
  - `peri-tui/src/kit/acp_types.rs` —— `AcpEventData::LocalLoadingReset` 变体（TUI 内部事件，不走 ACP 协议）
  - `peri-tui/src/kit/acp_events/mod.rs` —— dispatch 分支
  - `peri-tui/src/kit/acp_events/turn.rs` —— `handle_loading_reset`（幂等：phase 非 PromptRunning 时 no-op）
  - `peri-tui/src/kit/acp_bridge.rs` —— `event_kind_short` 分支（穷尽 match）
  - `peri-tui/src/kit/submit_consumer.rs` —— `clear_loading_state` 双保险；cancel_consumer 先 cancel（带超时）再复位；`handle_clear_submit` 两处直接写并入 `clear_loading_state`（同构一并改）；prompt 失败路径（:67）经 `clear_loading_state` 自动受益
  - `peri-tui/src/kit/acp_events_test.rs` —— `test_loading_reset_event_resets_phase`（复位 + 幂等）、`test_loading_reset_then_turn_interrupted_keeps_idle`（取消后收尾事件不拉回 loading）
  - `peri-tui/src/kit/submit_consumer_test.rs` —— `test_clear_loading_state_emits_loading_reset_event`（事件注入 + 直接写兜底）；cancel 测试注释更新为"先 cancel 后复位"
- **验证状态**：待验证（本机 `cargo test -p peri-tui --lib` 894 passed、`cargo clippy -p peri-tui --lib -- -D warnings` 通过）
- **残余风险**：cancel RPC 完成瞬间服务端已在途的流事件（通知通道异步）到达 bridge 时仍会把 phase 拉回 PromptRunning 一次——修复通过"先 cancel 后复位"将窗口压缩到 cancel 完成前，无法完全消除在途事件；彻底消除需 bridge 增加"取消屏蔽期"状态（超出本 issue 范围）

（由 auto-issue-fixer 修复阶段追加，创建时留空）
