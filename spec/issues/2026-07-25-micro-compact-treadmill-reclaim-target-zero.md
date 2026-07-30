# Micro Compact 反复触发但压不住预算（跑步机效应 + reclaim_target=0）

> **状态**：Open | **优先级**：高 | **类型**：Bug | **日期**：2026-07-25

## 问题描述

日志中 Micro Compact 连续触发 3 次且间隔很短，每次都能"成功"执行（`affected_count > 0`），但预算始终降不下来。表现为无限循环：预算 ≥ 75% → Micro 触发 → 预算微降 → 下轮新工具结果涌入 → 预算又 ≥ 75% → Micro 再次触发……

## 症状详情

### 直接现象

- Micro Compact 在同一 session 中 3 次连续触发，间隔 ≤ 2 个 turn
- 每次返回 `estimated_tokens_saved > 0`，系统认为 Micro 生效
- 但 `estimated_context_tokens()` 长期停留在 75%-95% 区间
- Full Compact 从未被触发——Micro 永远不升级

### 根因 1：`reclaim_target` 在 75%-93.5% 区间恒为 0

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:39-41`

```rust
pub fn target_reclaim_tokens(&self) -> u64 {
    self.estimated_tokens.saturating_sub(self.target_tokens())
}
pub fn target_tokens(&self) -> u64 {
    let reserve = self.output_reserve + self.predicted_tool_growth + self.safety_buffer;
    self.context_window.saturating_sub(reserve as u32) as u64
}
```

`target_tokens()` = `context_window - (output_reserve + safety_buffer)` ≈ **93.5% 窗口**。

验证结果（200K 窗口，8000 output_reserve，5000 safety_buffer）：

```
60% (120K): reclaim_target = 0
70% (140K): reclaim_target = 0
75% (150K): reclaim_target = 0    ← Micro 触发阈值
80% (160K): reclaim_target = 0
85% (170K): reclaim_target = 0
90% (180K): reclaim_target = 0
93.5% (187K): reclaim_target = 0  ← = target_tokens()
95% (190K): reclaim_target = 3000 ← 终于 > 0
```

**后果**：

1. `reclaim_target == 0` → `estimated_tokens_saved >= 0` 永远成立 → Micro 永远判定"有效"，从不升级 Full
2. `reclaim_target > 0` 是 `run_compact` 中升级 Full 的前置条件（`mod.rs:231`），被阻断
3. budget 不到 95% 时根本不可能触发 Full

### 根因 2：`stale_steps=5` 造成跑步机效应

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:233`

```rust
let stale_limit = total_groups.saturating_sub(config.micro_compact_stale_steps); // 默认 5
for (gi, group) in groups.iter().enumerate() {
    if gi >= stale_limit { continue; }  // 最近 5 轮完全不碰
}
```

验证结果（10 轮对话，每轮含工具调用）：

```
plan_micro #1 (skip=true): actions=10, tokens_saved=546  ← 标了 0-4 轮
plan_micro #2 (skip=true): actions=0,  tokens_saved=0    ← 0-4 已标 + 5-9 被保护，颗粒无收
plan_micro #3 (skip=true): actions=0,  tokens_saved=0    ← 完全无新消息可标
```

**后果**：

1. 第一次 compact 截断旧消息后，保护窗内的 5 轮新消息完全不被触碰
2. 第二次、第三次 compact 返回 `actions=0`，`estimated_tokens_saved=0`
3. 但 budget 仍超阈值（因为保护窗内的消息没被截断），系统进入"触发→空转→触发→空转"死循环

### 两个根因的叠加效应

```
┌──────────────────────────────────────────────────────┐
│  reclaim_target=0 → Micro 永远"有效"                  │
│       +                                              │
│  stale_steps=5 → 第二次起 actions=0, tokens_saved=0  │
│       =                                              │
│  Micro 反复触发但既不生效也不升级 Full                 │
└──────────────────────────────────────────────────────┘
```

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-agent/src/agent/compact_v2/planner.rs:39-41` | `target_reclaim_tokens()` 计算 |
| `peri-agent/src/agent/compact_v2/planner.rs:233` | `stale_limit` 计算 |
| `peri-agent/src/agent/compact_v2/mod.rs:231` | Full 升级条件 `reclaim_target > 0` |
| `peri-agent/src/agent/stages/compact.rs:65-73` | `ContextPressure` 构造 |
| `peri-agent/src/agent/compact_v2/config.rs` | `micro_compact_stale_steps` 默认值 |

## 修复方向

1. **reclaim_target=0 时也应升级**：当 `budget_pct >= 0.85` 且 Micro 只找到少量可截断消息时，应升级 Full，而不是停在 Micro
2. **降低 stale_steps**：从 5 降到 2-3，减少保护窗覆盖面
3. **或改为动态 stale_steps**：根据窗口大小自适应 `stale_steps`

## 验证标准

- budget 在 75%-93.5% 区间且已连续 Micro N 次时，应升级为 Full
- Micro 不应在 `actions=0` 的情况下反复触发
- 两次连续的 compact 之间应有实质性进展（要么预算降了，要么升级策略）

---

## 状态变更记录

| 日期 | 原状态 | 新状态 | 操作人 | 操作说明 |
|------|--------|--------|--------|----------|
| 2026-07-25 | — | Open | agent | 创建 issue，记录双根因 |

## 修复记录

（由修复阶段追加，创建时留空）
