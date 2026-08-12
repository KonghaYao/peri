> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-langfuse-lib-ts-negative-input-cost.md

# langfuse lib.ts 成本估算在 cacheRead 大于 input 时出现负成本

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`estimateCost` 用 `t.input - t.cacheRead` 计算未命中缓存的输入 token。Anthropic 的 `input_tokens` 不含 cache-read token（两者分开上报），当 cacheRead 超过 input 时差值为负，`inputCost` 为负，拉低总成本显示，报告失真。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- lib.ts 约 285-292 行：`inputCost = ((t.input - t.cacheRead) / 1_000_000) * price.input`。
- `genTokens` 中 `input` 取 `u.input || u.prompt_tokens`，`cacheRead` 取 `u.cache_read_input_tokens`——两者语义独立。
- 示例：一次调用 input=500、cacheRead=8000（Anthropic 缓存命中场景），差值 -7500，成本被负值拉低。

## 复现条件

- **复现频率**：取决于数据（cacheRead 大于 input 时出现）
- **触发步骤**：
  1. 运行 `bun .claude/skills/langfuse/scripts/analyze.ts --report`
  2. 观察含大量缓存读取的 trace 成本异常偏低
- **环境**：Anthropic 类分开上报 cache token 的 provider

## 期望改进方向

- 将未命中缓存的输入 token 数钳制到 0（`Math.max(0, t.input - t.cacheRead)`）。
- 保持 cacheCost 与 outputCost 计算不变。

## 涉及文件

- `.claude/skills/langfuse/scripts/lib.ts` —— `estimateCost`（约 285-292 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: estimateCost 输入成本钳制到 0（Math.max(0, t.input - t.cacheRead)），避免 cacheRead > input 时负成本 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：cacheRead 超 input 时成本下限钳制，修复记录见正文 |

## 修复记录

- **改动摘要**：`lib.ts` 的 `estimateCost()` 中 `inputCost` 改为 `(Math.max(0, t.input - t.cacheRead) / 1_000_000) * price.input`，并加注释说明 Anthropic 类 provider 将 cache_read 单独计费。`cacheCost`、`outputCost` 计算保持不变。
- **验证结果**：`bun build lib.ts` 通过（exit 0）。
