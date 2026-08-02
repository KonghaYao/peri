# analyze.ts 中 `--days <N>` 的值被当作 trace 数量上限

**状态**：Open
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

`bun analyze.ts 20 --days 7 --report` 本应分析最近 20 条 trace（位置参数 `20`），实际只分析了 7 条。命令行数值参数解析相互干扰，导致分析数量与用户意图不一致，报告结论可能基于过小的样本。

来源：code review（`target/review.md`，Major）。

## 症状详情

- `analyze.ts` 的 `main()` 参数循环中，`default` 分支把**任何**纯数字 token 都解析为 `limit`（`parseInt(args[i])` 且 `n > 0` 时覆盖）。
- 该循环只识别 `--trace-id / --tools / --growth / --report` 四个选项，`--days` 的值 `7` 会落入 `default` 分支，把 `limit` 从 `20` 覆盖为 `7`。
- `run()` 直接使用这个局部 `limit` 调 `fetchTracesFiltered({ limit })`，`parseFilterArgs` 解析出的正确 limit（会跳过选项值）没有被使用。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 执行 `bun .claude/skills/langfuse/scripts/analyze.ts 20 --days 7 --report`
  2. 观察输出中的 "Analyzing N traces..."，N 为 7 而非 20
- **环境**：任何带 `--days`/`--from`/`--to` 等带数值选项的组合

## 期望改进方向

- 移除本地扫描对数字 token 的覆盖逻辑，或使用 `parseFilterArgs` 的结果作为最终 limit。
- 若需保留 analyze.ts 默认 10 条的语义，需显式指定（`parseFilterArgs` 默认 50）。

## 涉及文件

- `.claude/skills/langfuse/scripts/analyze.ts` —— 参数解析与 `run()` 入口（约 497-526 行）
- `.claude/skills/langfuse/scripts/lib.ts` —— `parseFilterArgs`（约 145-192 行），已正确跳过选项值

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: main() 参数循环跳过 --days/--from/--to/--tag/--user/--session/--name/--model 等选项值，--limit 单独解析，位置数字参数不再被 flag 值覆盖 |

## 修复记录

- **改动摘要**：`analyze.ts` 的 `main()` 参数循环新增带值选项跳过逻辑（`--days/--from/--to/--tag/--user/--session/--name/--model` 统一 `i++` 跳过其值），并新增 `--limit <N>` 分支显式解析；`default` 分支保留对位置数字参数（如 `20`）的解析。`bun analyze.ts 20 --days 7 --report` 现在 limit 为 20 而非 7；`--limit 30` 单独生效；无参数时默认仍为 10。`run()` 仍使用局部 `limit`，与修复后解析结果一致。
- **验证结果**：`bun build analyze.ts` 通过（exit 0）；bun 内联脚本模拟参数解析，`["20","--days","7","--report"]` → limit=20、`["--limit","30","--days","7"]` → limit=30，均符合预期。
