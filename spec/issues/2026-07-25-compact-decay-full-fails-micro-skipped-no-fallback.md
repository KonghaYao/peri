# Compact 退化链——Micro 被跳过、Full 失败、无 fallback、计数器跨域污染

> **状态**：Open | **优先级**：P0 | **类型**：Bug | **日期**：2026-07-25 | **来源**：/systematic-debugging + /ultra-batch（6 agent 对抗验证）

## 问题总结

用户观察：**micro compact 不会被触发，然后 full compact 触发，但是并没有 compact 的事情。**

经 6 个 agent 从代码追踪、替代假设、测试覆盖、配置验证、死刑机制 5 个维度对抗验证后，确认这是一个由 **5 条根因交织成的退化链**：

```
                 design/代码语义反转 (Root 4)
                          ↓
      Micro 有效的判定指标 → budget≥0.95 时 "跳过Micro直走Full"
                          ↓
      min_floor 5% 窗口 (Root 3) → reclaim_target 恒≥10000
                          ↓
        ┌─────────────────┼─────────────────┐
        ↓                                     ↓
   Micro 永远被判"不足"                  Full 失败时 affected=0
   (saved≈3000 < reclaim≈10000)          (Root 2: 没有 fallback)
        ↓                                     ↓
        跳过 Micro → 直走 Full                颗粒无收
              ↓
        consecutive_failures++  (Root 1: 跨域污染)
              ↓
        3 次死刑 → compact 永久静默 (Root 5: 无内部恢复)
              ↓
        token_tracker 永不 reset → budget 永远高位 → 死循环
```

**用户看到的现象**：
- "micro compact 不会被触发"：因为 budget≥0.95 时代码主动跳过 Micro apply（`mod.rs:245-262`）
- "full compact 触发"：跳过 Micro 后直走 Full（LLM 调用确实触发了）
- "但是并没有 compact 的事情"：Full 失败返回 `affected_count=0`，没有任何消息被 compact

---

## 根因 ①（P0）：`consecutive_failures` 跨域污染 + 3 次死刑

**文件**：`peri-agent/src/agent/compact_v2/mod.rs:117-129` + `peri-agent/src/agent/tool_dispatch.rs:565-593`

`consecutive_failures` 被 **compact 失败**和**工具调用失败**两个完全无关的领域共享同一个 `AtomicU32`。

```
compact 失败 → mod.rs:381: *consecutive_failures += 1
工具失败   → tool_dispatch.rs:568: fetch_add(1)
工具成功   → tool_dispatch.rs:592: store(0)  ← 意外归零（逃生门）
```

**后果**：
- 3 次连续的**工具调用失败**会误封 compact（即使 compact 从未失败过）
- 反过来工具成功又是意外的"复活门"——compact 的计数器被无关系事件覆盖
- 到达 3 后：`mod.rs:117-129` 直接 return，compact 永久跳过
- **死刑跳过的 strategy 字段被错误标为 `Micro`**（`mod.rs:121`），而不是 `Skip`，可能误导下游

**compact 自身无任何恢复机制**。路径分析：
- `consecutive_failures` 只在成功时清零（`mod.rs:229/266/291/304/375`）
- 没有任何递减、超时重置、或外部健康检查
- 唯一的"逃生门"是工具调用的副作用——不可依赖

---

## 根因 ②（P0）：Full 失败后 Micro 被跳过 → 颗粒无收

**文件**：`peri-agent/src/agent/compact_v2/mod.rs:245-262` + `:379-391`

```rust
// mod.rs:245-262
} else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
    // Micro 回收不足 + budget 高位 → 跳过 Micro apply，直接 Full
    run_full_or_degrade(...).await  // Full 失败 → affected_count = 0
}

// mod.rs:379-391 (error branch)
Err(e) => {
    *consecutive_failures += 1;
    CompactResult { affected_count: 0, ... }  // ← Micro 被跳过了，Full 也失败了
}
```

**问题**：这是一个单点故障。Micro 是零 LLM 开销、非破坏性的操作（只标 `truncated` 不删消息），完全应该先执行。但代码在 budget 高位时跳过它，把一切押在 Full 上。Full 失败则颗粒无收。

**Full 为何可能失败**（主 agent 路径有 `auxiliary_model`）：
- LLM 返回空响应 → `CompactEmptyResponse`
- Provider 超时 / 网络错误
- 并发调用导致 Token 速率限制

---

## 根因 ③（P1）：`reclaim_target` min_floor 过大 → Micro 永远"不足"

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:41-45`

```rust
pub fn target_reclaim_tokens(&self) -> u64 {
    let raw = self.estimated_tokens.saturating_sub(self.target_tokens());
    let min_floor = (self.context_window as u64 * 5) / 100;  // 200K → 10000
    raw.max(min_floor)
}
```

`min_floor = 5% × 窗口` 是为了修复 "reclaim_target=0 阻断 Full 升级"（issue `micro-compact-treadmill-reclaim-target-zero`），但引入了副作用：10000 tokens 的门槛对 Micro 过高。

200K 窗口下 reclaim_target 的分布：

| budget | raw reclaim | min_floor | **实际 reclaim** |
|--------|-------------|-----------|-----------------|
| 75% (150K) | 0 | 10000 | **10000** |
| 80% (160K) | 0 | 10000 | **10000** |
| 90% (180K) | 0 | 10000 | **10000** |
| 95% (190K) | 3000 | 10000 | **10000** |
| 98% (196K) | 9000 | 10000 | **10000** |

**Micro 典型回收量**：200-300 字符/msg × 4 字节/char ÷ 4 token/char × 10 条消息 ≈ 2000-3000 tokens。永远达不到 10000。

---

## 根因 ④（P1）：设计文档与代码实现语义完全相反

| 维度 | 设计文档 | 实际代码 | 影响 |
|------|---------|---------|------|
| **Micro→Full 规则** | Micro **有效后叠加** Full | Micro **不足时跳过** Micro 直走 Full | 语义反转 |
| **关系** | 串联（Micro + Full 叠加） | 二选一（跳过 Micro） | Full 失败则颗粒无收 |
| **决策指标** | `affected_count ≥ 5` | `estimated_tokens_saved ≥ reclaim_target` | 单位不同 |
| **`micro_min_affected`** | 核心参数 | **死代码**：`run_compact` 零引用 | 配置无效 |

**设计文档**：`docs/design/peri-agent-compact-v2.md:39-44`
```
先执行 Micro → 检查 affected_count → 再决定叠加 Full 或跳过
```

**实际代码**：`mod.rs:226-262`
```
先 dry-run 估算 → saved < reclaim → 跳过 Micro → 直走 Full
```

**关键差异**：设计是"先做再看效果"，代码是"预估后决定要不要做"。

---

## 根因 ⑤（P2）：测试覆盖缺口 + 模块注释过期

1. **无测试覆盖 Micro 叠加 Full 正常路径**（`trigger_test.rs`）：所有测试因短消息导致 `saved < reclaim`，只走 "跳过 Micro 直走 Full" 路径。`mod.rs:226-244` 分支零覆盖。

2. **`test_micro_effective_full_overlay` 名不副实**（`trigger_test.rs:143`）：测试名为"叠加 Full"但实际行为是"跳过 Micro 直走 Full"（注释也承认）。只断言 `strategy == Full`，不验 `affected_count > 0`，Full 即使全失败也通过。

3. **`mod.rs:4-9` 注释过期**：仍描述旧的 v2 设计（Micro 有效 + 叠加 Full），与当前代码的"Micro 不足 + 跳过 Micro"不符。

4. **无 budget=0.95 精确边界测试**：`auto_compact_threshold` 默认 0.95，但测试只用 0.80 和 0.98。

---

## 影响范围

- **所有主 agent 会话**：budget ≥ 0.95 时自动触发此退化链
- **Workflow Agent**：`context_budget` 硬编码 200K（`workflow_agent.rs:299`），不随模型变化
- **SubAgent / Fork Agent**：`compact_llm` 可能为 None（`execute_fork.rs:136`），Full 必定失败
- **工具连续失败时**：跨域污染可意外封禁 compact

---

## 修复计划

### 1. 分离 `consecutive_failures` 计数器（P0）

**文件**：`peri-agent/src/agent/stages/mod.rs`、`tool_dispatch.rs`

- 将 `compact.consecutive_failures` 从共享的 `AtomicU32` 拆为独立字段
- `tool_dispatch` 不再触碰 compact 的计数器
- compact 计数器的归零仅由 compact 成功路径触发

### 2. Micro 先 apply 再 Full（P0）

**文件**：`peri-agent/src/agent/compact_v2/mod.rs:245-262`

```rust
// 修复后：先应用 Micro（零 LLM 开销），再尝试 Full
} else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
    let affected = micro::micro_compact(transcript, config);  // 先做 Micro
    let mut full_result = run_full_or_degrade(...).await;
    full_result.affected_count += affected;  // 合并 Micro + Full 的贡献
    full_result
}
```

### 3. 降低 min_floor（P1）

**文件**：`peri-agent/src/agent/compact_v2/planner.rs:42-44`

```rust
// 从 5% 降到 2% 窗口，或改为动态
let min_floor = (self.context_window as u64 * 2) / 100;  // 200K → 4000
// 或直接使用 micro_min_affected × 平均消息 token 数
```

### 4. 恢复 `micro_min_affected` 或标记废弃（P1）

**文件**：`peri-agent/src/agent/compact_v2/config.rs:37-39`

二选一：
- **恢复**：在 `mod.rs:226` 使用 `affected_count >= micro_min_affected` 替代 `saved >= reclaim`
- **废弃**：添加 `#[deprecated]` 或从 config 中移除死代码

### 5. 补充测试 + 更新注释（P2）

- `trigger_test.rs`：补 Micro 叠加 Full 测试（用 mock LLM）、budget=0.95 边界测试、Full 失败 Micro 仍生效测试
- `mod.rs:4-9`：更新模块注释反映实际决策流程

---

## 修复记录

（由修复阶段追加，创建时留空）
