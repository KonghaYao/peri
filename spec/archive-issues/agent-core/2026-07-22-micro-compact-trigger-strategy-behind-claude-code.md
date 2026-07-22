# Micro Compact 触发策略设计落后于 Claude Code，压缩效果不足

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-22

## 修复记录

### 修复 #1（2026-07-22）
- **操作人**：agent
- **commit**：`5ab45956 refactor(compact): redesign Micro Compact trigger strategy`
- **修复内容**：重新设计 Micro Compact 多层级触发策略，对标 Claude Code

## 问题描述

当前 Micro Compact 的触发策略远弱于 Claude Code——仅有一条简单的百分比阈值路径（70% Micro → 85% Full），缺少多层级触发策略、绝对 token 数判定、时间衰减、缓存感知压缩、预测性检查等关键机制。这导致 Micro Compact 的压缩效果有限：上下文使用率降幅不够、频繁触发 Full Compact（消耗 token）、无法利用 API 缓存编辑能力无损压缩。

## 对标分析

### 1. 整体架构对比

**Claude Code**：三层级触发策略，每层独立运作

```
microcompactMessages()  [每次 API 调用前]
  ├── Time-based MC      ← 时间衰减触发 (gap ≥ 60min)
  │   └── 直接 content-clear 旧工具结果，不保留在消息中
  ├── Cached MC          ← 缓存感知触发 (count-based threshold)
  │   └── 通过 API cache_edits 删除，不破坏 prompt cache
  └── (无) → fallthrough

autoCompactIfNeeded()    [query loop，micro 之后的独立路径]
  ├── Session Memory Compact  ← 优先尝试 (零 API 调用)
  │   └── 复用后台提取的 Session Memory 摘要
  └── Full Compact            ← 兜底 (LLM 摘要)
      ├── microcompactMessages() 预处理 micro 清理
      └── compactConversation()  LLM 结构化摘要
```

**Perihelion**：单一路径的百分比阶梯

```
run_compact()  [ReAct 循环每轮开头]
  ├── budget < 70%     → 跳过
  ├── 70% ≤ budget < 85% → Micro Compact
  │   └── 按 stale_steps 跳过最近 N 轮，白名单工具标 truncated
  └── budget ≥ 85%     → Full Compact
      └── LLM 摘要 + excluded 标记 + Re-inject
```

### 2. 触发判定：绝对 token vs 百分比

| | Claude Code | Perihelion |
|---|---|---|
| **判定方式** | 绝对 token 数 | 百分比（70%/85%） |
| **阈值计算** | `contextWindow - bufferTokens - maxOutputTokens` | `0.70 * contextWindow` |
| **buffer 随模型变化** | 800K window → 50K buffer<br>400K → 30K buffer<br>默认 → 13K buffer | 固定 70%/85%，不随模型变化 |
| **代码位置** | `autoCompact.ts:77-82` `getAutocompactBufferTokens()` | `compact_v2/mod.rs:56-68` `determine_compact_strategy()` |

**关键差异**：百分比策略在不同 context window 下的表现差异巨大。

| context window | 70% 触发时剩余 token | 85% 触发时剩余 token | Claude Code 触发时剩余 |
|---|---|---|---|
| 200K (Claude Sonnet) | 140K → 余 60K | 170K → 余 30K | ~187K → 余 ~13K |
| 128K (GPT-4o) | 89.6K → 余 38.4K | 108.8K → 余 19.2K | ~115K → 余 ~13K |
| 1M (Gemini) | 700K → 余 300K | 850K → 余 150K | ~950K → 余 ~50K |

对于 128K 窗口，Peri 在 70%（89.6K）过早触发 Micro Compact；对于 1M 窗口，85%（850K）触发 Full 时仍有 150K 空间，也过早。**Claude Code 的绝对 token 策略在不同窗口下行为一致**——总是保留固定大小的 buffer。

### 3. Micro Compact 层级设计

| 路径 | Claude Code | Perihelion |
|---|---|---|
| **Time-based MC** | 距上次 assistant 消息 ≥ 60min 时触发，content-clear 旧工具结果（保留最近 5 个），直接修改消息内容 | 无 |
| **Cached MC** | 基于计数阈值触发，通过 `cache_edits` API 删除工具结果，不破坏 prompt cache；状态跨 turn 持久 | 无 |
| **Legacy MC** | 已删除（`tengu_cache_plum_violet` 始终 true） | 即 Peri 当前唯一的 Micro Compact 路径 |

**Time-based MC** 的关键设计思想（`timeBasedMCConfig.ts:4-16`）：
> 当距上次 main-loop assistant 消息的间隔超过阈值时，服务端 prompt cache 几乎肯定已过期，整个前缀会被重写。在 API 调用前清除旧工具结果可以缩小被重写的内容。

**Cached MC** 的关键设计思想（`microCompact.ts:299-306`）：
> 利用 `cache_edits` API 删除工具结果，不修改本地消息内容（`cache_reference` + `cache_edits` 在 API 层追加），从而保持 prompt cache 有效。基于 GrowthBook 实验配置的触发/保留阈值。

### 4. 预测性检查

**Claude Code**（`autoCompact.ts:88-94`）：
```typescript
export function estimateMaxTurnGrowth(model: string): number {
  const maxOutput = Math.min(getMaxOutputTokensForModel(model), MAX_OUTPUT_TOKENS_FOR_SUMMARY)
  return maxOutput + TOOL_RESULT_GROWTH_ESTIMATE  // 15K
}
```
在 API 调用前预测本轮最大增长，若当前 usage + 预估增长 ≥ 阈值则提前触发 compact。避免"API 返回后才发现超限"的被动触发。

**Perihelion**：无预测。在 ReAct 循环每轮开始时检查（`stages/compact.rs:60-67`），若 API 返回了大量内容超限，要等到下一轮才能触发 compact。

### 5. Micro 与 Full 的关系

| | Claude Code | Perihelion |
|---|---|---|
| **Micro 运行时机** | 每次 API 调用前（microcompactMessages） | ReAct 循环每轮开头 |
| **Micro 与 Full 关系** | 独立：Micro 每次都跑；Full 是独立触发路径，内部也会调 Micro 预处理 | 阶梯：70% Micro → 85% Full，连续关系 |
| **Full 内的 Micro 预处理** | `compact.ts` 在生成摘要前先调 `microcompactMessages()` 清理 | 无。Full Compact 直接对整个 transcript 做摘要 |

### 6. 调用集成位置

**Claude Code**：
- `microcompactMessages()` — 在 `query.ts` 的 `callModel()` 之前调用，是 API 调用链的一部分
- `autoCompactIfNeeded()` — 在 query loop 中，API 返回后检查

**Perihelion**：
- `run_compact()` — 在 `stages/compact.rs`，ReAct 循环每轮开始时统一触发
- 没有在 API 调用前单独运行的 micro 路径

## 涉及文件

### Perihelion 侧
- `peri-agent/src/agent/compact_v2/mod.rs:56-68` — `determine_compact_strategy()`：百分比判定逻辑
- `peri-agent/src/agent/compact_v2/config.rs:47-73` — `CompactConfig`：缺少 time-based / cached MC / token-threshold 配置
- `peri-agent/src/agent/compact_v2/micro.rs:25-115` — `micro_compact()`：仅 stale_steps + 白名单
- `peri-agent/src/agent/stages/compact.rs:14-160` — `run_compact()`：单一次序的 Micro → Full 调度
- `docs/design/peri-agent-compact-v2.md` — compact v2 架构设计文档

### Claude Code 参照侧
- `src/services/compact/autoCompact.ts` — 触发判定、autoCompactIfNeeded、预测性检查
- `src/services/compact/microCompact.ts` — microcompactMessages、Time-based MC、Cached MC
- `src/services/compact/timeBasedMCConfig.ts` — 时间衰减配置
- `src/services/compact/cachedMicrocompact.ts` — 缓存编辑状态管理
- `src/services/compact/compact.ts` — 主 compact 流程（含 Micro 预处理）

## 设计决策（已确认）

| 决策 | 结论 |
|------|------|
| 范围 | 仅 Micro Compact 优化 |
| 运行位置 | 仍在 ReAct 循环的 compact 阶段 |
| 判定方式 | 百分比（不做模型特定窗口） |
| Micro 压缩机制 | 保持现有：stale_steps + 白名单 + truncated 标记 |
| Micro 截断对象 | 从仅 tool_result 扩展为 tool_result + tool_use arguments（Write/Edit 等大输入工具的 input 也截断） |
| 工具过滤 | 从白名单（`micro_compactable_tools`）改为黑名单（`micro_excluded_tools`，默认空，截断所有工具） |
| Time-based MC | 暂不引入 |
| Cached MC | 暂不引入 |
| 预测性检查 | 暂不引入 |

## 新触发流程（草稿，待确认）

```
compact 阶段
  │
  ├── budget < 75% → 跳过
  │
  └── budget ≥ 75% → 跑 Micro
        │
        ├── Micro 压缩量 ≥ 阈值（affected_count 足够多）
        │     │
        │     ├── budget 仍 ≥ 95% → 叠加跑 Full
        │     └── budget < 95% → Micro 就够了，结束
        │
        └── Micro 压缩量 < 阈值（affected_count 太少，Micro 效果差）
              │
              └── 直接跑 Full（Micro 升级）
```

**决策要点**：

1. **Micro 阈值从 70% 提升到 75%**：减少过早触发
2. **Micro → Full 关系**：Micro 优先跑，跑完后根据"压缩量"决定是否升级为 Full
3. **压缩量计数器**：`affected_count`（Micro 实际截断的消息数）低于阈值时表示 Micro 效果差，改为跑 Full
4. **Full 独立阈值**：95% 仍保留，Micro 不够 + budget ≥ 95% 时叠加 Full

## 设计决策（全部确认）

| 决策 | 结论 |
|------|------|
| 范围 | 仅 Micro Compact 优化 |
| 运行位置 | 仍在 ReAct 循环的 compact 阶段 |
| 判定方式 | 百分比（不做模型特定窗口） |
| Micro 压缩机制 | 保持现有：stale_steps + 白名单 + truncated 标记 |
| Micro 阈值 | 75%（从 70% 提升） |
| Full 阈值 | 95%（从 85% 提升） |
| 压缩量阈值 | 绝对数量，`affected_count < 5` 时判定 Micro 效果差 |
| Micro→Full 升级逻辑 | Micro 效果差时直接跑 Full（truncated 标记无需清理——Full 的 excluded 覆盖同批消息） |
| 同时命中 | budget ≥ 95% 时 Micro 先跑（效果差则直接 Full；效果好则叠加 Full） |

### 最终触发流程

```
compact 阶段
  │
  ├── budget < 75% → 跳过
  │
  └── budget ≥ 75% → 跑 Micro (micro_compact)
        │
        ├── affected_count ≥ 5（Micro 有效）
        │     │
        │     ├── budget ≥ 95% → 叠加跑 Full  ──── 两条都跑了
        │     └── budget < 95% → 结束             ──── 仅 Micro
        │
        └── affected_count < 5（Micro 无效）
              │
              └── 直接跑 Full                        ──── 仅 Full（Micro 升级）
```

### 配置变更

`CompactConfig` 新增字段：

```rust
/// Micro Compact 压缩量阈值：affected_count 低于此值时判定 Micro 效果差，
/// 直接升级为 Full Compact（truncated 标记无需清理——Full 的 excluded 覆盖同批消息）。
#[serde(default = "default_micro_min_affected")]
pub micro_min_affected: usize,  // 默认 5

/// 黑名单工具——这些工具不参与 Micro 截断。默认空，即默认截断所有工具。
/// 与旧 `micro_compactable_tools` 白名单相反：白名单手动指定截断谁，黑名单手动指定不截断谁。
#[serde(default)]
pub micro_excluded_tools: Vec<String>,  // 默认 []
```

`micro_compact_threshold` 默认值从 0.70 改为 0.75。
`micro_compactable_tools`（白名单）→ 删除，替换为 `micro_excluded_tools`（黑名单）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建：Micro Compact 触发策略对标 Claude Code 分析 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
