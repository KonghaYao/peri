# TUI Loading 状态机制 split-brain——三个写入源造成状态分裂


> 归档于 2026-07-20，原路径 spec/issues/2026-07-17-loading-state-split-brain.md
> ⚠️ **未证实**：本文档基于代码静态分析（阅读 `acp_events.rs`、`submit_consumer.rs`、`atoms.rs`、`acp_bridge.rs` 等文件），关键结论待实际运行验证。详见 `docs/design/tui-loading-state-analysis.md`。

**状态**：Fixed
**优先级**：高
**类型**：技术债
**创建日期**：2026-07-17

## 问题描述

TUI 的 loading 状态（spinner 显示/隐藏）由**三个不同路径写入**同一个 `ACP_STATE.is_loading` 字段，形成 split-brain——提交时 submit_consumer 乐观写入、流式时 acp_bridge 通过 push_acp_state 派生写入、终止时 TurnDone 等 7 个事件手动兜底写入。三者之间通过一条"防御性状态提升"逻辑互相修补，workaround 之上叠加 workaround，任何时序变化都可能击穿这套防御。

目前已有 **5 个**与 loading 相关的高优 issue（见「关联」节），每个都是这套机制的某个具体时序故障。修复单个症状而不理顺整体架构会在别处引入新问题。

## 现状

### loading 状态的三个写入源

```
submit_consumer         ──直接写 atom──▶
                                          ▶  ACP_STATE.is_loading
acp_bridge.push_acp_state  ──phase派生──▶     (同一个 bool)
                                          ▶
TurnDone 等 7 个事件       ──手动兜底──▶
```

| 写入源 | 触发的状态 | 时机 | 问题 |
|--------|-----------|------|------|
| `submit_consumer` | `is_loading = true` | 用户按 Enter，prompt RPC 前 | 绕过 bridge phase，导致 split-brain |
| `push_acp_state` | `is_loading = phase == PromptRunning` | 每个事件处理后 | 正常路径，但有"防御提升" hack |
| `TurnDone` 等 7 事件 | `ACP_STATE.is_loading = false`（手动） | turn/suspend/compact 完成 | 为绕过防御逻辑而存在 |

### 三层 workaround 的层层叠加

**第 1 层**：`push_acp_state` 的"防御性状态提升"（`acp_events.rs:1123`）：
```rust
if acp.is_loading && state.phase == SessionPhase::Idle {
    state.phase = SessionPhase::PromptRunning;  // 自动提升
}
```
——因为 submit_consumer 绕开 bridge 写了 atom，首个到达事件可能不是流式的，防御逻辑自动把 bridge phase 和 atom 对齐。

**第 2 层**：TurnDone 等 7 个事件中在每个事件处理内手动 `ACP_STATE.state().write().is_loading = false`（`acp_events.rs:264` 等）：
```rust
state.phase = SessionPhase::Idle;
ACP_STATE.state().write().is_loading = false;  // 手动清 atom
push_acp_state(state);  // 此时防御检测：acp.is_loading == false → 不触发
```
——因为 push_acp_state 的防御逻辑是单向提升（只能 Idle→PromptRunning，不能降级），如果 atom 的 is_loading 先于 phase 被清，防御逻辑会将它重新改回 true。所以抢在 push_acp_state 之前手动清 atom。

**第 3 层**：`submit_consumer` 的 `clear_loading_state()` 兜底（`submit_consumer.rs:328`）——prompt RPC 失败时的最后防线。

### 具体混乱点

1. **`CompactStarted` 不设 `variant=1`**（`acp_events.rs:578`）：只设 phase=PromptRunning（→ is_loading=true），但 variant 保持旧值（可能为 0）。ACP_STATE 出现 `variant=0` + `is_loading=true` 的矛盾组合。scroll auto-follow 用 variant 判断是否跟随，compact 期间会误判。

2. **`SubagentStopped` 设 `variant=1` 但不设 phase**（`acp_events.rs:556`）：如果 bg agent 完成后 TurnSuspended 已清 loading（phase=Idle），SubagentStopped 的 `variant=1` 写入后触发 push_acp_state，此时若 atom 的 is_loading 恰被新提交改回 true，防御逻辑将 phase 重新提升为 PromptRunning——loading 重新卡死。

3. **defensive promotion 是双向 race**：TurnDone 已正常结束 loading 后，若下一个事件到达前 atom 被 submit_consumer 新提交改为 true，则下一个非流事件（ToolCount/Progress）的 push_acp_state 会将 phase 从 Idle 重新提升为 PromptRunning。

4. **transport 断连无 watchdog**：流式过程中 ACP transport 断开 → notifier 退出 → bridge 退出 → loading 停留在 true，无清理路径。

5. **`variant` 和 `phase` 功能重叠**：两者都试图表达 "agent 是否在运行中"，但语义略有不同（variant = UI 模式，phase = Agent 执行状态），且在不同路径分别设置，缺乏协变保证。

## 期望改进方向

**P0 最小修补**（先止血）：
- `CompactStarted` 增设 `state.variant = 1`
- `SubagentStopped` 中仅当 phase==PromptRunning 时设 variant=1
- bridge 退出前兜底清 `ACP_STATE.is_loading`
- 提取 `clear_loading_state()` 辅助函数消除 7 处重复代码

**P1 根治方案**（统一状态源）：
- 让 `submit_consumer` 不直接写 atom，改为发 `AcpEventData::PromptSubmitted` 事件
- bridge 统一处理所有 loading 状态变更
- 删除 push_acp_state 的防御性状态提升逻辑
- 删除 TurnDone 等事件中的手动 atom 写入

**P2 架构整理**：
- 合并 `variant` 和 `phase` 为单一枚举（如 `Idle | Streaming | Modal`），消除冗余和分裂可能

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/acp_events.rs` | **核心**：dispatch_and_notify（事件→状态转换）、push_acp_state（防御逻辑）、7 处手动 atom 写入 |
| `peri-tui/src/kit/submit_consumer.rs` | 提交时乐观写入 is_loading=true（split-brain 起点） |
| `peri-tui/src/kit/atoms.rs` | ACP_STATE atom 定义（is_loading 字段） |
| `peri-tui/src/kit/acp_bridge.rs` | bridge 主循环，退出时无 loading 清理 |
| `peri-tui/src/kit/message_area/footer.rs` | is_loading 消费端（spinner 渲染） |
| `peri-tui/src/kit/message_area/scroll.rs` | variant 消费端（auto-follow 判断） |

## 关联

已有 loading 相关 issue（症状级），本 issue 为这些症状的**结构性根因**：

| Issue | 状态 | 与本 issue 的关联 |
|-------|------|-----------------|
| `2026-07-13-main-agent-done-loading-persists-bg-still-running.md` | Open | bg agent 完成后 SubagentStopped 覆盖 TurnDone 的 loading 清除 → 是本 issue §现状-2 的具体表现 |
| `2026-07-11-bg-multi-agent-loading-callback-ok-but-loading-stuck.md` | Open | 多 bg agent 全部回调正常但 loading 不退 → 防御逻辑被触发后的症状 |
| `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md` | Open | 同上场景不同症状 |
| `2026-07-08-loading-indicator-never-displays.md` | Open | spinner 从不显示 → split-brain 导致 is_loading 被覆盖 |
| `2026-07-17-spinner-tick-decouple-from-acp-bridge.md` | Open | spinner tick 解耦 → loading 状态机制整理时的依赖项 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建（基于 docs/design/tui-loading-state-analysis.md 静态分析） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
