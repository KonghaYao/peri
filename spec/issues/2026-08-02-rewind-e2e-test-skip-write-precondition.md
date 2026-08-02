# rewind v2 E2E 在 Write 工具未调用/目标文件缺失时静默通过

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`rewind-v2.test.ts` 的"回滚后恢复文件内容"用例以"LLM 是否调用了 Write 工具"作为前置条件：LLM 未调用 Write 时仅打印 NOTE 日志并直接通过；目标文件不存在时同样跳过删除检查。这使该用例在代理行为偏离（未写文件）或环境异常（文件未生成）时仍然绿，无法捕获真实回归。

来源：code review（`target/review.md`，Minor；容忍度属有意设计，见下）。

## 症状详情

- `e2e/tests/scenarios/rewind-v2.test.ts` 约 105-133 行：
  - else 分支：未检测到 Write 调用 → `console.log(NOTE: LLM 未调用 Write)` → 通过；
  - 目标文件缺失时删除校验被跳过，同样通过。
- 设计意图：LLM 行为有随机性（可能用其他方式改文件），宽松处理避免 flaky。
- 代价：Write 调用存在但目标文件名不同、或文件根本未生成时，断言全部落空，用例成为空转。

## 复现条件

- **复现频率**：LLM 未按预期调用 Write（或文件名不匹配）时
- **触发步骤**：
  1. 运行 rewind v2 E2E 场景
  2. 使 LLM 不调用 Write（或写入其他文件）
  3. 观察用例仍 PASS，且无失败信息
- **环境**：E2E 场景（真实 LLM）

## 期望改进方向

- 至少断言"检测到 Write 调用"作为硬前置（未调用则 FAIL 并输出工具调用记录），再按目标文件存在性做删除校验；或显式标记该用例为"观察性"用例并在报告中提示。

## 涉及文件

- `e2e/tests/scenarios/rewind-v2.test.ts` —— 回滚恢复文件内容用例（约 105-133 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: Write 调用改为硬前置（未调用则 FAIL + 输出工具调用记录）；目标文件缺失时显式标记跳过原因 |

## 修复记录

- **改动**：`e2e/tests/scenarios/rewind-v2.test.ts`
  - else 分支（预算为空）：从「NOTE 日志 + 静默通过」改为硬前置 FAIL——先 `console.error` dump 当前屏幕（含该轮工具调用记录），再以带说明文案的 `expect(screenAfterEnter).toMatch(/回退将撤销|Rewind will revert/)` 断言失败，消除空转通过；
  - 目标文件删除校验：文件存在时保持原有 waitFor 删除断言；文件缺失时输出 WARN 明确说明「该轮 Write 未写入目标文件，删除校验被跳过」（走到此处预算必非空，成因即 LLM 写入其他文件名或文件未生成），不再静默。
- **验证**：E2E 需要真实 LLM 无法本地运行；以 `npx tsc --noEmit -p tsconfig.json`（e2e 目录）类型检查通过（exit 0）。
- **权衡说明**：issue 已注明「LLM 容错 vs 空转通过」是有意设计。本次保留容错（未改为文件缺失也 FAIL），仅把「Write 调用存在」设为硬前置——LLM 未调用 Write 时用例必红；LLM 用了其他方式/文件名写文件时仍按观察性处理并显式提示，避免 flaky。

