# trace-tokens.ts 的 `--index` 接受负值导致数组越界崩溃

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`trace-tokens.ts` 的 `--index` 用 `parseInt(...) || 1` 解析，负值不被拒绝。`--index -3` 产生 `index = -3`，通过上界检查后索引 `traces[-4]`（undefined），访问 `.id` 抛异常。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- 约 40 行：`const index = parseInt(args[indexIdx + 1]) || 1;`
- 约 53 行：仅检查 `index > traces.length`，不检查 `index < 1`。
- `--index -3` → `traces[-4]` 为 `undefined` → `.id` 抛 TypeError。

## 复现条件

- **复现频率**：必现（传入负值时）
- **触发步骤**：
  1. `bun .claude/skills/langfuse/scripts/trace-tokens.ts --index -3 --days 1`
  2. 观察抛 `TypeError: Cannot read properties of undefined`
- **环境**：任意过滤选项组合

## 期望改进方向

- 对小于 1 的索引值拒绝或钳制，复用现有的 out-of-range 报错与退出行为；合法的 1-based 索引行为不变。

## 涉及文件

- `.claude/skills/langfuse/scripts/trace-tokens.ts` —— `--index` 解析与校验（约 38-56 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: --index 显式校验为正整数，index < 1 或非数字时打印用法说明并 exit 1（与 prompt-breakdown 修复风格一致） |

## 修复记录

- **改动摘要**：`trace-tokens.ts` 中 `const index = parseInt(args[indexIdx + 1]) || 1` 替换为与 prompt-breakdown.ts 一致的显式校验：`Number(args[indexIdx + 1])` + `Number.isInteger(rawIndex) && rawIndex >= 1`；非法值（0、负数、非数字、缺值）打印 `Usage: bun trace-tokens.ts --index <N>  (N must be a positive integer)` 并 `process.exit(1)`。原有 `index > traces.length` 越界提示保留，合法的 1-based 索引行为不变。
- **验证结果**：`bun build trace-tokens.ts` 通过（exit 0）。
