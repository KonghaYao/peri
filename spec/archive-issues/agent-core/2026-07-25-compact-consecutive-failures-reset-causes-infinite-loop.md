> 归档于 2026-07-30，原路径 spec/issues/2026-07-25-compact-consecutive-failures-reset-causes-infinite-loop.md

# Compact 死机开关失效——修复引入 `consecutive_failures` 提前清零导致无限 Full 重试

> **状态**：Fixed | **优先级**：P0 | **类型**：Bug | **日期**：2026-07-25 | **来源**：/systematic-debugging（5 个子代理独立验证）

## 问题描述

上一次 compact 修复（`issue/2026-07-25-compact-decay-full-fails-micro-skipped-no-fallback`）在 mod.rs 新增了两处 `*consecutive_failures = 0`，位于 `run_full_or_degrade` 调用之前。这导致 Full 失败计数永不累积，死机开关（`consecutive_failures >= max_consecutive_failures = 3`）完全失效。

**用户观察**：micro compact 运行 1 次后，每轮都触发 Full compact，但 Full compact 没有任何实际效果（消息未被 exclude，budget 不下降）。

## 症状详情

### 直接现象

- Micro compact 第一轮正常执行（截断标记写入）
- 之后每轮 budget ≥ 0.95 → 进入 Micro+Full 分支
- Micro 正常 apply（标记 truncated），Full 尝试失败
- `consecutive_failures` 在 Full 之前被归零 → 振荡在 0↔1，永不触达 3
- Full 失败后策略仍为 `CompactStrategy::Full` → `token_tracker` 被重置
- 下一轮 Reason 阶段 LLM 收全量消息（Full 没排除任何内容）→ budget 立即恢复高位
- 循环：高 budget → Micro+Full(失败) → reset → Skip → LLM 全量 → 高 budget → ...

### 完整环路

```
Round N:   budget≥0.95 → Micro+Full
             *consecutive_failures = 0  ← BUG
             → Full 失败 → consecutive_failures = 1
             → strategy=Full, affected_count>0 → token_tracker.reset()
Round N+1: budget=0 → Skip
             Reason: LLM 全量消息 → accumulate(180K input_tokens)
Round N+2: budget≥0.95 → Micro+Full
             *consecutive_failures = 0  ← 又归零
             → Full 失败 → consecutive_failures = 1
             → ...
```

### `consecutive_failures` 轨迹

```
Round 1: 0 → (line 251 清零) → Full 失败 → +1 → 1
Round 2: 1 → (line 251 清零) → Full 失败 → +1 → 1
Round 3: 1 → (line 251 清零) → Full 失败 → +1 → 1
...永远振荡在 0↔1，max=3 永不触发
```

## 根因

### Bug 1（主因）：`*consecutive_failures = 0` 在 Full 之前（mod.rs:251, 317）

```rust
// mod.rs:245-275（Micro+Full 分支）
} else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
    let micro_affected = micro::micro_compact(transcript, config);
    *consecutive_failures = 0;  // ← LINE 251: 在 Full 之前清零！
    let mut full_result = run_full_or_degrade(          // ← LINE 261: Full 尝试
        transcript, llm, config, before_visible_len,
        consecutive_failures, cwd, ...,
    ).await;
```

同样问题在 Smart 路径（mod.rs:317）。

`run_full_or_degrade` 内部本身正确处理了计数器（成功清零 line 388，失败 +1 line 394），外部提前清零使其内部逻辑完全被 bypass。

### Bug 2（次要）：过期 excluded 标记清除机制是死代码（mod.rs:371-384）

```rust
// run_full_or_degrade 内部
if *consecutive_failures > 0 {
    // 清除所有 excluded 标记
}
```

- Micro+Full / Smart+Full 路径中 `consecutive_failures` 始终为 0 → 永远不触发
- 只在 force=true 路径可能触发 → 会不分青红皂白清除之前成功 Full 的有效 excluded 标记
- 防范的场景（Full 中途失败残留 excluded 标记）不存在——`full_compact_inner` 只在成功路径设置 excluded

### 为什么之前的诊断是错的

之前 issue 的 5 "根因"中：3 个无关痛痒（min_floor、dead code、注释），1 个的修复引入了本次问题（Micro+Full 分支），从未调查过最核心的问题——Full compact 为什么会一直失败。

## 复现条件

- **复现频率**：必现（任何 budget ≥ 0.95 且 Full compact 失败的场景）
- **触发步骤**：
  1. 长时间对话使上下文接近 200K token 上限
  2. Micro compact 执行一次（budget ≥ 0.75）
  3. 后续轮次中 budget 升至 ≥ 0.95
  4. Full compact 尝试失败（compact_llm 不可用 / LLM 调用超时 / 空响应）
  5. 死循环开始
- **前置条件**：compact_llm 缺失或 LLM 调用失败

## 涉及文件

- `peri-agent/src/agent/compact_v2/mod.rs:251` —— Micro+Full 分支中提前清零（主 bug）
- `peri-agent/src/agent/compact_v2/mod.rs:317` —— Smart 路径中同样问题
- `peri-agent/src/agent/compact_v2/mod.rs:371-384` —— 死代码：过期 excluded 标记清除
- `peri-agent/src/agent/stages/compact.rs:202-204` —— token_tracker.reset() 在 Full 失败时仍触发（因 affected_count 被 micro_affected 凑 >0）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建 issue，5 个子代理对抗验证确认 |
| 2026-07-25 | Open | Fixed | agent | 修复：删除 mod.rs:251 和 317 两处提前清零 |

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent
- **用户原意**：消除 compact 的死循环——Micro compact 后每轮都触发 Full compact，且 Full compact 无效
- **修复内容**：删除 `peri-agent/src/agent/compact_v2/mod.rs` 中两处 `*consecutive_failures = 0`（line 251 Micro+Full 分支、line 317 Smart 分支）。`run_full_or_degrade` 内部已正确管理计数器（成功清零、失败 +1），无需外部干预。
- **涉及 commit**：无（未提交）
- **验证状态**：待验证
