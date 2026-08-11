> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-prompt-breakdown-index-accepts-invalid-values.md

# prompt-breakdown.ts 的 `--index` 接受 0/负数/非数字，异常值静默变 1 或崩溃

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`prompt-breakdown.ts` 的 `--index` 用 `parseInt(...) || 1` 解析：`--index 0` 与 `--index abc` 被静默当成 1；`--index -2` 通过上界检查后索引 `traces[-3]`，访问 `undefined.id` 直接抛异常。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- 约 37 行：`const index = parseInt(args[indexIdx + 1]) || 1;`
- 约 50 行：仅检查 `index > traces.length`，负数与 0 不被拒绝。
- `--index 0` → 解析为 1（静默）；`--index abc` → 1（静默）；`--index -2` → `traces[-3].id` 抛 TypeError。

## 复现条件

- **复现频率**：必现（传入非法值时）
- **触发步骤**：
  1. `bun .claude/skills/langfuse/scripts/prompt-breakdown.ts --index -2 --days 1`
  2. 观察抛 `TypeError: Cannot read properties of undefined`
- **环境**：任意过滤选项组合

## 期望改进方向

- 非正整数（0、负数、非数字）直接报错退出，与现有上界检查的退出方式一致（`console.error` + `process.exit(1)`）。

## 涉及文件

- `.claude/skills/langfuse/scripts/prompt-breakdown.ts` —— `--index` 解析与校验（约 36-53 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: --index 显式校验为正整数，0/负数/非数字打印用法说明并 exit 1，越界提示保留 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：--index 输入校验（拒绝 0/负数/非数字），修复记录见正文 |

## 修复记录

- **改动摘要**：`prompt-breakdown.ts` 中 `const index = parseInt(args[indexIdx + 1]) || 1` 替换为 `Number(args[indexIdx + 1])` + `Number.isInteger(rawIndex) && rawIndex >= 1` 校验；非法值（0、负数、非数字、缺值）打印 `Usage: bun prompt-breakdown.ts --index <N>  (N must be a positive integer)` 并 `process.exit(1)`。原有 `index > traces.length` 越界提示保留。
- **验证结果**：`bun build prompt-breakdown.ts` 通过（exit 0）。
