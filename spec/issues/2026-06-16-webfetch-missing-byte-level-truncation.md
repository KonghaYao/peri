# WebFetch 缺字节级截断，单行长内容未被拦截

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-06-16

## 问题描述

WebFetch 抓取网页后的 `truncate_content` 只按**行数**截断（上限 2000 行），没有字节/字符数上限。当抓取 minified JS/CSS 等单行超大文件（如 `addon-webgl.js` 几百 KB 的压缩源码）时，行数未超限（只有 1 行），全部内容直接吐回给 LLM，造成上下文浪费。

## 症状详情

| 对比维度 | 期望行为 | 当前行为 |
|----------|---------|---------|
| 多行长内容 | 按行数截断 ✓ | 按行数截断 ✓ |
| 单行长内容（几百 KB） | 触发字节级截断 | 不截断，全量返回 |
| 多行 + 字节超限 | 先按行截断，再按字节兜底截断 | 仅按行截断 |

Bash 工具（`terminal.rs`）已有两层保护：`MAX_OUTPUT_LINES`（2000 行）+ `MAX_OUTPUT_CHARS`（100000 字节）。WebFetch 缺第二层字节级截断。

## 复现条件

- **复现频率**：必现（抓取任何单行超大文件都会触发）
- **触发步骤**：
  1. 让 Agent 用 WebFetch 抓取一个 minified JS/CSS 文件的 URL
  2. 文件是单行或少于 2000 行但总字节数远超合理上限
  3. 观察返回给 LLM 的内容——未被截断

## 涉及文件

- `peri-middlewares/src/middleware/web_fetch.rs` —— `truncate_content()` 函数仅按行数截断，缺少字节级上限
- `peri-middlewares/src/middleware/terminal.rs` —— `truncate_output()` 已有行+字节双层截断的先例，可作为参考实现

## 历史关联

- `spec/archive-issues/2026-06-10-webfetch-truncation-no-disk-persist.md`（Fixed）—— 修复了 WebFetch 截断后不落盘的问题，但未涉及截断逻辑本身的字节级缺陷

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-16 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
