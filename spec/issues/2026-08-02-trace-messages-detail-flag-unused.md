# trace-messages.ts 的 `--detail` 标志被解析但从未使用

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`trace-messages.ts` 声明并解析了 `--detail` 标志（`showDetail`），但该变量在文件中从未被读取，`--detail` 不改变任何输出行为。帮助文本与头部注释却宣传该选项，误导使用者。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- 约 57 行：`const showDetail = args.includes("--detail");`
- 全文件仅此一处出现 `showDetail`，无任何消费点。
- 头部注释（约 6-7 行）与 `--help` 文本（约 14-15 行）均列出 `[--detail]`。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. `bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId> --detail`
  2. 输出与不带 `--detail` 完全一致
- **环境**：任意

## 期望改进方向

- 删除 `showDetail` 解析，并从头部注释与 `--help` 文本移除 `--detail`。
- 或实现其本来的详细输出语义（若该功能本应存在）。

## 涉及文件

- `.claude/skills/langfuse/scripts/trace-messages.ts` —— 参数解析（约 57 行）及文档注释

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: 删除未使用的 showDetail 变量，并从头注释与 --help 文本移除 --detail |

## 修复记录

- **改动摘要**：按 issue 期望方向选择删除。`trace-messages.ts` 移除第 57 行 `const showDetail = args.includes("--detail")`（全文件唯一出现点，无消费），并同步删除头部注释与 `--help` 文本中的 `[--detail]` 标记。脚本其余输出行为不变。
- **验证结果**：`bun build trace-messages.ts` 通过（exit 0）。
