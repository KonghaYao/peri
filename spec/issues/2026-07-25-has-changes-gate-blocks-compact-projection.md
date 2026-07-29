# has_changes() 决策门控阻断 Compact 投影——三条根因汇合

> **状态**：Open | **优先级**：P0 | **类型**：Bug | **日期**：2026-07-25 | **来源**：/systematic-debugging

## 问题总结

Micro Compact 的 truncated 标记对 LLM 实际可见内容**完全无效**。三条独立逻辑缺陷汇合在同一断点：

```
             estimate_tokens().max(50)  膨胀投影字符数
                          ↓
            estimated_tokens_saved = 0  对短消息永远成立
                          ↓
        ┌─────────────────┼─────────────────┐
        ↓                                     ↓
  has_changes() = false                 reclaim_target = 0
  (saved > 0 ? → 否)                    (estimated - target ≤ 0)
        ↓                                     ↓
  Reason 跳过 render_llm_view            Full 升级路径永远不触发
        ↓                                     ↓
  LLM 看到完整原文                       Micro 永远"满足"条件
        ↓
  truncated 标记形同虚设
```

## 根因链 ①：`has_changes()` = `estimated_tokens_saved > 0` 不可靠

**文件**：`peri-agent/src/agent/compact_v2/projection.rs:128-129`

```rust
pub fn has_changes(&self) -> bool {
    self.estimated_tokens_saved > 0
}
```

**调用点**：`peri-agent/src/agent/stages/reason.rs:79-80`

```rust
let plan = plan_micro(&guard, config, false);
if plan.has_changes() {
    let view = render_llm_view(&guard, &plan, &caps)?;
    // ...
} else {
    visible  // ← fallback：完整原文，truncated 标记被无视
}
```

**验证实验**（`planner_test.rs`）：

```
test test_has_changes_returns_false_for_short_messages_even_with_actions ... ok
```

构造了 `MicroCompactPlan{ actions: 10个, estimated_tokens_saved: 0 }`：
- `has_changes()` → **false** ❌（应有 10 个投影 action 待应用）
- 10 个 action 因 token 估算为 0 被丢弃

## 根因链 ②：`reclaim_target = 0` 在 75%-93.5% budget 区间恒为 0

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:29-41`

```rust
pub fn target_tokens(&self) -> u64 {
    let reserve = self.output_reserve + self.predicted_tool_growth + self.safety_buffer;
    self.context_window.saturating_sub(reserve as u32) as u64  // ≈ 93.5% 窗口
}
pub fn target_reclaim_tokens(&self) -> u64 {
    self.estimated_tokens.saturating_sub(self.target_tokens())
}
```

**验证实验**（200K 窗口，8K output_reserve，5K safety_buffer）：

| budget | estimated_tokens | reclaim_target | 现象 |
|--------|-----------------|----------------|------|
| 50% | 100K | 0 | ✅ 正常 |
| **75%** | 150K | **0** | ❌ Micro 触发但无回收目标 |
| **85%** | 170K | **0** | ❌ 逼近饱和仍无回收目标 |
| 93.5% | 187K | 0 | = target_tokens() |
| **95%** | 190K | 3000 | 才首次 >0 |

**调用点 `run_compact` Micro 分支**（`mod.rs:231`）：

```rust
} else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
    // ↑ reclaim_target > 0 为 false → Full 升级路径永远不触发
    // → 落到 else 分支（"部分收益也好"，但实际收益=0）
}
```

## 根因链 ③：`estimate_tokens()` 的 `.max(50)` 膨胀

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:336`

```rust
let projected_chars = (chars / 3).max(50);
before += chars;
after += projected_chars;
```

**验证实验**（8 轮短消息对话，stale_steps=1）：

```
actions=14, before=22, after=175, saved=0
```

- 14 个 action 选中了实际消息
- 每条消息的 `projected_chars` = `max(chars/3, 50)` 被 50 的 floor 膨胀
- `after (175) > before (22)` → `saturating_sub` → `saved = 0`
- 原意图是至少保留 50 字符避免投影丢失关键信息，但副作用是所有短消息的 `saved` 归零

## 三条根因的汇合

```
has_changes() 用 saved>0 判有效  ← 依赖不可靠的 token 估算
       +
reclaim_target=0 在大部分区间      ← 永远满足"回收目标"，从不升级
       +
.max(50) 让短消息 saved=0         ← 有 action 但 estimate 为 0
       =
LLM 从未看到压缩后的内容
```

## 修复方向

### 修改 1：`has_changes()` 改为判 `!actions.is_empty()`

```rust
// projection.rs:128
pub fn has_changes(&self) -> bool {
    !self.actions.is_empty()
}
```

**理由**：投影是否有效应该看有无 action，不应依赖不可靠的 token 估算。`plan_micro` 返回空 actions 时才应跳过投影。

### 修改 2：`reclaim_target` 加最小值

```rust
// planner.rs:39-41
pub fn target_reclaim_tokens(&self) -> u64 {
    let raw = self.estimated_tokens.saturating_sub(self.target_tokens());
    let min_floor = (self.context_window as u64 * 5) / 100; // 5% 窗口
    raw.max(min_floor)
}
```

**理由**：防止 reclaim_target=0 时"永远满足"阻断 Full 升级。但 5% 是按比例的还是固定值需讨论。

### 修改 3：`estimate_tokens` 修正确保 saved 不过低

```rust
// planner.rs:336
let projected_chars = (chars / 3).max(50).min(chars);  // 投影不应比原文大
```

或去掉 `.max(50)`：

```rust
let projected_chars = chars / 3;
```

**理由**：`max(50)` 让短消息 projected > original，导致 saved 归零。用 `min(chars)` 确保投影 ≤ 原文，或干脆去掉 floor。

## 影响范围

- **Micro Compact**、**Smart Compact**：两者都走 `plan_micro` + `has_changes` + `render_llm_view` 路径
- **Full Compact**：不受影响（Full 是摘要式压缩，不依赖 plan_micro）
- **Reason 阶段**：修改 1 是核心，直接改变 LLM 看到的输入内容
- **Compact 阶段**：修改 2 是核心，改变 Micro → Full 的升级逻辑

## 验证实验记录

三个验证实验已写入 `peri-agent/src/agent/compact_v2/planner_test.rs`（最后 3 个测试函数），可直接运行：

```bash
cargo test -p peri-agent --lib -- planner_test --nocapture
```

## 修复记录

（修复阶段追加）

---

*创建于 /systematic-debugging Phase 3 验证后，三条假设全部确认*
