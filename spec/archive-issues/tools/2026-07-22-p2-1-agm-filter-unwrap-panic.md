> 归档于 2026-08-11，原路径 spec/issues/2026-07-22-p2-1-agm-filter-unwrap-panic.md

# P2-1：agm filter file_name().unwrap() panic 风险

**状态**：Fixed
**优先级**：低
**类型**：Bug
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 错误处理维度

## 最新情况（2026-08-11）

对应实现：filter.rs file_name().unwrap() → expect() 带语义消息

## Problem Statement

`agm/src/filter.rs:200` 中对 `path.file_name()` 调用 `.unwrap()`。当路径以 `/` 结尾或为根路径时，`file_name()` 返回 `None`，导致 panic。

虽然正常使用场景下不太可能触发（agm 处理的是 git 跟踪的文件路径），但缺少防御性处理违反项目"禁止裸 unwrap"的工程规范。

## 建议方案

替换为 `path.file_name().unwrap_or_default()` 或使用 `expect("path must have filename")` 提供清晰的错误消息。

## 涉及文件

- `agm/src/filter.rs:200`

## 风险

- **极低**：单行替换
