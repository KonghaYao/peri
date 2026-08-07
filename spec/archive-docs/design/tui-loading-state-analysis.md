# TUI Loading 状态机制分析与改进建议

> 状态：分析文档 | 日期：2026-07-17

## 目录

1. [状态定义全景](#1-状态定义全景)
2. [loading 状态的三个来源](#2-loading-状态的三个来源)
3. [状态转换链路](#3-状态转换链路)
4. [混乱点分析](#4-混乱点分析)
5. [风险场景编目](#5-风险场景编目)
6. [改进建议](#6-改进建议)

---

## 1. 状态定义全景

整个 loading 链路涉及 4 层抽象，6 个状态相关字段：

```
Agent::ExecutorEvent  →  ACP::MappedEvent(SessionUpdate)  →  AcpEventData  →  BridgeState  →  ACP_STATE atom  →  UI 渲染
```

### 1.1 `SessionPhase`（BridgeState 内部）

```rust
// peri-tui/src/kit/acp_events.rs:22
pub enum SessionPhase {
    Idle,             // 空闲
    PromptRunning,    // prompt 执行中
    ReplayingHistory, // 历史回放中（实际是 dead path）
}
```

**用途**：通过 `state.phase == SessionPhase::PromptRunning` 派生 `is_loading`。

### 1.2 `variant`（BridgeState 内部，u8）

| 值 | 含义 | 触发事件 |
|---|------|---------|
| 0 | Idle | TurnDone, TurnInterrupted, TurnSuspended, RewindCompleted, CompactCompleted, CompactError, AgentExecutionFailed |
| 1 | Streaming | TextChunk, ReasoningChunk, ToolStarted, ToolEnded, SubagentStarted, SubagentStopped |
| 2 | Modal | HitlPending, AskUser, RewindPreview, OauthNeeded |

**用途**：写入 `AcpStateSnapshot.variant`，UI 组件据此决策（如 `scroll.rs` 的 auto-follow）。

### 1.3 `is_loading`（AcpStateSnapshot 中，ACP_STATE atom）

```rust
// peri-tui/src/kit/atoms.rs:45
pub struct AcpStateSnapshot {
    pub variant: u8,           // 从 BridgeState.variant 同步
    pub is_loading: bool,      // 派生自 SessionPhase（但有例外，见下文）
    pub view_count: usize,
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}
```

**来源**：`push_acp_state()` 中 `is_loading = state.phase == SessionPhase::PromptRunning`。**但** `submit_consumer` 和多个事件处理路径会**直接写** `ACP_STATE.is_loading`，绕过 `phase`。

### 1.4 `LOADING_EPOCH`（独立 atom）

```rust
pub static LOADING_EPOCH: AtomStatic<u64>;
```

每次新提交递增，用于 footer 检测新一轮 loading 会话，重置 spinner 动画。

### 1.5 `CurrentTurn.active`（BridgeState 内部）

```rust
pub struct CurrentTurn {
    pub active: bool,  // 当前是否在流式输出
    // ...
}
```

用于消息区渲染——`active=true` 时增量追加文本，`false` 时完整展示。

### 1.6 `TurnStatus`（Agent 层）

```rust
pub enum TurnStatus { Done, Interrupted, Error }
```

Agent 执行结束后的状态，映射为 ACP 层的 `stop_reason`，最终驱动 TUI 的 `TurnDone`/`TurnInterrupted`。

---

## 2. loading 状态的三个来源

loading 状态**不是单一数据源**，而是由三个不同位置写入，形成三角关系：

```
                    ┌──────────────────┐
                    │  submit_consumer  │  ← 来源 1：用户提交时直接写 atom
                    │  is_loading=true  │
                    └────────┬─────────┘
                             │
                    ┌────────▼─────────┐
                    │  ACP_STATE atom  │  ← 最终消费端
                    │  .is_loading     │
                    └──▲──────────▲───┘
                       │          │
         ┌─────────────┘          └──────────────┐
         │                                       │
┌────────┴──────────┐              ┌─────────────┴──────┐
│   acp_bridge      │              │  TurnDone 等事件   │
│ push_acp_state()  │              │ 直接写 atom         │
│ phase→is_loading  │  ← 来源 2    │ is_loading=false   │  ← 来源 3
└───────────────────┘              └────────────────────┘
```

### 来源 1：submit_consumer 乐观写入

```rust
// peri-tui/src/kit/submit_consumer.rs:172
let mut guard = acp.write();
guard.variant = 1;
guard.is_loading = true;      // ← 直接写 atom，不经过 bridge
```

**意图**：在 prompt RPC 发出之前就让 UI 响应，避免用户感知延迟。

**问题**：绕过了 bridge 的 `phase` 状态机。bridge 侧的 `phase` 此时仍是 `Idle`（首个流式事件尚未到达），造成 bridge ↔ atom 的状态分裂。

### 来源 2：acp_bridge 通过 push_acp_state 写入

```rust
// peri-tui/src/kit/acp_events.rs:1114
fn push_acp_state(state: &mut BridgeState) {
    // 防御性状态提升（见 §4.1）
    if acp.is_loading && state.phase == SessionPhase::Idle {
        state.phase = SessionPhase::PromptRunning;
    }
    // 从 phase 派生 is_loading
    let snapshot = AcpStateSnapshot {
        is_loading: state.phase == SessionPhase::PromptRunning,
        // ...
    };
    // 仅变化时写入 atom
    if *acp != snapshot { *acp = snapshot; }
}
```

**意图**：bridge 是正常路径——流式事件到来后设 phase=PromptRunning，push_acp_state 写入 is_loading=true。

### 来源 3：TurnDone 等事件手动兜底写入

```rust
// peri-tui/src/kit/acp_events.rs:264
// 必须在 push_acp_state 之前手动清 atom 的 is_loading，
// 否则 push_acp_state 的防御逻辑会将 phase 从 Idle 提升回 PromptRunning
state.phase = SessionPhase::Idle;
ACP_STATE.state().write().is_loading = false;  // ← 直接写
push_acp_state(state);
```

**意图**：绕开 push_acp_state 的防御逻辑（见 §4.1）。

**问题**：这是 workaround 的 workaround。TurnDone/TurnInterrupted/TurnSuspended/RewindCompleted/CompactCompleted/CompactError/AgentExecutionFailed 共 **7 个事件**都复制了同样的 "manual clear + push_acp_state" 模式。

---

## 3. 状态转换链路

### 3.1 正常流程：提交 → 流式 → 结束

```
用户按 Enter
  │
  ├─ submit_consumer
  │   ACP_STATE.is_loading = true         ← 来源 1
  │   LOADING_EPOCH += 1
  │   acp_client.prompt()                 ← 异步 RPC
  │
  ▼
Agent 开始执行，产生 TextChunk
  │
  ├─ acp_bridge dispatch_and_notify(TextChunk)
  │   state.variant = 1
  │   state.phase = PromptRunning         ← bridge 侧设 phase
  │   push_acp_state()                    ← 来源 2：is_loading=true（已为 true）
  │   push_view_models()
  │
  ▼
  ... 更多 TextChunk/ToolStarted/ToolEnded ...
  │
  ▼
Agent TurnEnded → stop_reason="end_turn"
  │
  ├─ acp_bridge dispatch_and_notify(TurnDone)
  │   state.flush_current_turn()
  │   state.variant = 0
  │   state.phase = Idle
  │   ACP_STATE.is_loading = false        ← 来源 3：手动清（绕开防御）
  │   push_acp_state()                    ← 来源 2：is_loading=false
  │   drain_input_buffer()
  │
  ▼
UI 检测 is_loading=false → spinner 消失 → 显示 "Brewed for Xs"
```

### 3.2 挂起流程：TurnSuspended

```
Agent TurnSuspended（wait-until 等）
  │
  ├─ dispatch_and_notify(TurnSuspended)
  │   flush_current_turn()
  │   variant=0, phase=Idle
  │   ACP_STATE.is_loading = false
  │   push_acp_state()
  │   // ⚠️ 不调用 drain_input_buffer（Agent 保持存活）
  │
  ▼
UI loading 停止，Agent 在后台等待唤醒
```

### 3.3 Compact 流程

```
Agent CompactStarted
  │
  ├─ dispatch_and_notify(CompactStarted)
  │   state.phase = PromptRunning         ← is_loading=true
  │   // ⚠️ BUG：不设 state.variant = 1
  │   push_acp_state()
  │
  ▼
Agent CompactCompleted
  │
  ├─ dispatch_and_notify(CompactCompleted)
  │   state.variant = 0
  │   state.phase = Idle
  │   ACP_STATE.is_loading = false
  │   push_acp_state()
  │   注入 TuiSystemNote（compact 摘要）
```

### 3.4 交互弹窗流程（HitlPending/AskUser 等）

```
Agent 产生 HitlPending
  │
  ├─ dispatch_and_notify(HitlPending)
  │   state.variant = 2                  ← Modal
  │   state.popup_kind = Some(...)
  │   // phase 不设（维持 PromptRunning）
  │   // loading 继续转
  │   push_acp_state()
  │
  ▼
用户审批/输入 → /approve 命令
  │
  ├─ Agent 继续执行
  │   或 TurnDone → loading 停止
```

---

## 4. 混乱点分析

### 4.1 核心混乱：防御性状态提升（split-brain workaround）

```rust
// acp_events.rs:1123
if acp.is_loading && state.phase == SessionPhase::Idle {
    state.phase = SessionPhase::PromptRunning;  // 防御提升
}
```

**设计目的**：submit_consumer 乐观设 `is_loading=true` 后，首个到达的事件如果是非流事件（ToolCount/Progress），bridge 的 phase 仍为 Idle，`push_acp_state` 会推导 `is_loading=false`，覆盖掉刚设的 true。

**为什么这是混乱的**：

1. **它承认了 split-brain**：逻辑存在的前提是 bridge 和 atom 的状态不同步，而它不去修复根本原因（submit_consumer 不应绕开 bridge 写 atom），而是用一个 patch 来弥合裂缝。

2. **它是单向的**：只能提升（Idle→PromptRunning），不能降级。一旦 atom 的 is_loading 被**其他路径**写回 true（如 submit_consumer 新提交），而 bridge 的 phase 已正确设为 Idle，防御逻辑会错误地将 phase 提升回 PromptRunning，让 loading 重新卡死。

3. **它迫使 7 个事件手动清 atom**：TurnDone 等事件必须在调 `push_acp_state` 前先手动写 `ACP_STATE.is_loading = false`，否则防御逻辑会将 phase 重新提升。这是 workaround 之上的 workaround。

### 4.2 `CompactStarted` 不设 variant=1 → phase/variant 分裂

```rust
// acp_events.rs:578
CompactStarted => {
    state.phase = SessionPhase::PromptRunning;  // is_loading=true
    push_acp_state(state);
    // ← variant 未设 1（保持旧值 0 或之前的值）
}
```

**结果**：ACP_STATE 中 `variant=0` (Idle) 但 `is_loading=true`。UI 同时读到 "Idle 模式" 和 "loading 中" 的矛盾组合。scroll auto-follow 用 variant 判断是否跟随，此时会错误地不跟随。

### 4.3 `SubagentStopped` 设 variant=1 但不设 phase → 反向分裂

```rust
// acp_events.rs:556
SubagentStopped { agent_id } => {
    state.variant = 1;              // Streaming
    // phase 不设 ← 注释说"避免 bg agent TurnDone 后被重新激活"
    push_view_models(state);
    push_acp_state(state);
}
```

**场景**：bg agent 完成后，`TurnSuspended` 已设 `phase=Idle, variant=0`。若 `SubagentStopped` 先于 `TurnSuspended` 到达，则 `variant=1, phase=PromptRunning`（正常）。若 `SubagentStopped` 后于 `TurnSuspended` 到达，则 `variant=1, phase=Idle`（分裂）。

**危险**：如果此时 submit_consumer 的新提交将 atom 的 is_loading 改回 true，`push_acp_state` 的防御逻辑会触发 → phase 被提升为 PromptRunning → loading 卡死。这是 issue `2026-07-13-main-agent-done-loading-persists-bg-still-running.md` 的根本原因。

### 4.4 variant 和 phase 功能重叠

两者都试图表达 "agent 是否在运行中"：

| 状态 | variant | phase | is_loading |
|------|---------|-------|------------|
| Idle | 0 | Idle | false |
| Normal streaming | 1 | PromptRunning | true |
| Compact | 0 (bug) | PromptRunning | true |
| Modal (HITL) | 2 | PromptRunning(*) | true |
| Subagent stopped after suspend | 1 | Idle | false(**) |

(*) Modal 期间 phase 维持 PromptRunning（因为 Agent 仍在等待用户输入）
(**) 但如果防御逻辑触发，可能变成 true

**variant 的语义更偏向 "UI 模式"（Streaming vs Modal vs Idle）**，而 **phase 的语义更偏向 "Agent 执行状态"**。两者在不同路径中分别被设置，缺乏统一的协变保证。

### 4.5 SessionReplayStarted/Done 是 dead path

```rust
// acp_events.rs:231, 242
// dead path: SessionReplayStarted is not emitted by notifier
// dead path: SessionReplayDone is not emitted by notifier
```

这两个变体虽然存在但没有实际触发路径。replay 事件以 TextChunk/ToolStarted 流式变体进入 bridge。历史上 replay 曾导致 loading 永久卡住（因为流式事件设 phase=PromptRunning）。当前修复方案是在 `thread_load_consumer` 中 `load_session()` 后兜底清 loading。

### 4.6 transport 断连无 watchdog

如果 ACP transport 在流式过程中断开：
- notifier 的 notification_rx 关闭，退出循环
- bridge 的 rx recv() 收到 None，退出循环
- **loading 停留在 true**，没有清理路径

没有超时/watchdog 机制检测 bridge 存活状态并清理 loading。

---

## 5. 风险场景编目

| # | 场景 | 触发条件 | 严重度 | 相关 Issue |
|---|------|---------|--------|-----------|
| A | loading 永久卡 true | transport 断连 | 🔴 高 | 无 |
| B | loading 重新激活（已关闭后被提升） | TurnDone → submit_consumer 新提交 → 防御逻辑触发 | 🔴 高 | [agent-done-loading-persists](spec/archive-issues/2026-07-13-main-agent-done-loading-persists-bg-still-running.md) |
| C | variant/phase 不一致（Compact） | CompactStarted 不设 variant=1 | 🟡 中 | 无 |
| D | variant/phase 不一致（SubagentStopped） | bg agent 完成后 SubagentStopped 覆盖 variant | 🟡 中 | 同上 |
| E | scroll auto-follow 误判 | 读 variant 时 variant/phase 不一致 | 🟡 中 | 无 |
| F | thread 切换 loading race | load_session replay 事件与兜底清理竞态 | 🟢 低 | 无 |

---

## 6. 改进建议

### 6.1 根治方案：统一状态源，消除 split-brain

**目标**：loading 状态有且仅有一个写入路径。

**方案**：让 `submit_consumer` **不直接写** `ACP_STATE.is_loading`，改为通过一个专用事件通知 bridge：

```
submit_consumer 发出 "PromptSubmitted" 事件 → bridge dispatch → 
  bridge 统一设置 phase=PromptRunning, variant=1 →
  push_acp_state 统一写 is_loading=true
```

**改动量**：中
- 删除 `submit_consumer` 中直接写 `ACP_STATE` 的代码（3 处）
- 新增 `AcpEventData::PromptSubmitted` 变体
- bridge dispatch 中处理该变体，统一设 phase/variant

**收益**：
- 消除 split-brain 根因
- `push_acp_state` 的防御逻辑可以删除
- TurnDone 等 7 个事件中的 `ACP_STATE.state().write().is_loading = false` 可以移除

**风险**：如果 `PromptSubmitted` 事件到达 bridge 前有延迟，UI 可能在用户按 Enter 后有短暂空白（无 spinner）。可以用两种方式缓解：
- 保持 `submit_consumer` 设 `is_loading=true` 但作为**短期预加载**（桥接方案）
- 或者接受这点延迟（当前 commit + RPC 本身的延迟可能已超过 bridge 处理延迟）

### 6.2 折中方案：删除防御逻辑，让 submit_consumer 成为唯一 atom 写入者

**方案**：反向简化——让 bridge 的 `push_acp_state` **只写 variant/view_count，不写 is_loading**。`is_loading` 完全由 submit_consumer 和 TurnDone 等终止事件手动管理。

**改动量**：小
- `push_acp_state` 中删除 `is_loading` 的写入
- 删除防御性状态提升逻辑
- bridge 中所有 `state.phase = xxx` 改为仅用于内部逻辑（不派生 is_loading）

**收益**：防御逻辑消失，race 场景消除。

**问题**：这是让 submit_consumer 和 acp_bridge **共管** atom（但各自管不同字段），仍然是两个写入源，只是避免了冲突。

### 6.3 最小修补方案

如果暂时不重构架构，至少修复以下 3 个明确的 bug：

| 修复项 | 改动 |
|--------|------|
| `CompactStarted` 设 `variant=1` | +1 行 |
| `SubagentStopped` 只在 phase==PromptRunning 时设 variant=1 | +2 行（条件判断） |
| 添加 loading watchdog（bridge 退出时清 loading） | +5 行（bridge loop 退出前清 ACP_STATE） |
| 统一 TurnDone 等事件的手动 atom 清空为辅助函数 | 消除 7 处重复代码 |

### 6.4 长期建议：合并 variant 和 phase

`variant` 和 `phase` 有大量语义重叠：

```
Idle          ↔ variant=0, phase=Idle
PromptRunning ↔ variant=1, phase=PromptRunning
Modal         ↔ variant=2, phase=PromptRunning（agent 仍在等）
ReplayingHistory ↔ variant=0, phase=ReplayingHistory（dead path）
```

可以合并为一个枚举：

```rust
pub enum SessionPhase {
    Idle,
    Streaming,                         // 原 variant=1, phase=PromptRunning
    Modal { popup_kind: PopupKind },   // 原 variant=2
    Replaying,                         // 原 ReplayingHistory
}
```

然后 `AcpStateSnapshot.is_loading = !matches!(phase, Idle | Replaying)`。

这将消除 variant/phase 不一致的所有路径。

---

## 附录：修改影响面评估

### 若采用 §6.1（统一状态源）

| 文件 | 改动类型 | 行数 |
|------|---------|------|
| `peri-tui/src/kit/acp_types.rs` | 新增 `AcpEventData::PromptSubmitted` | +5 |
| `peri-tui/src/kit/acp_events.rs` | 新增 dispatch 分支 + 删除防御逻辑 + 删除 7 处手动 atom 写 | +10 / -30 |
| `peri-tui/src/kit/submit_consumer.rs` | 删除 3 处直接写 ACP_STATE | -15 |
| `peri-tui/src/kit/acp_notifier.rs` | 生成 PromptSubmitted 事件 | +5 |

总改动约 65 行，净减少约 20 行。

### 若采用 §6.3（最小修补）

| 文件 | 改动类型 | 行数 |
|------|---------|------|
| `acp_events.rs` | CompactStarted 加 variant=1 | +1 |
| `acp_events.rs` | SubagentStopped 条件判断 | +3 |
| `acp_events.rs` | 提取 `clear_loading` 辅助函数 | +5/-35 |
| `acp_bridge.rs` | bridge 退出前清 loading | +3 |

总改动约 12 行，净减少约 25 行（7 处重复代码合并）。
