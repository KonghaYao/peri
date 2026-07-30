> 归档于 2026-07-30，原路径 spec/issues/2026-07-16-p1-4-compact-v2-split.md

# P1-4：compact_v2.rs ~900 行文件拆分

**状态**：Fixed
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-16
**来源**：`spec/issues/2026-07-16-architecture-upgrade-checklist.md` P1-4

## Problem Statement

`compact_v2.rs`（`peri-agent/src/agent/compact_v2.rs`）约 900 行，是 peri-agent 中最大的单文件。内含三个不同阶段的 Compact 策略实现：
- Micro Compact（标记 truncated）
- Full Compact（LLM 摘要 + excluded + re-inject）
- Smart Compact（未实现，空分支）

三种策略的代码逻辑独立，共用部分仅 `CompactResult` 结构体和顶层 `run_compact` 入口。放在同一文件导致：
- 难以定位特定策略的实现
- 新增策略（Smart）时需要全局理解文件结构
- P1-5 已提取 `determine_compact_strategy()` 公共函数，为进一步拆分做好了准备

## 建议方案

拆为 3 个子文件 + 1 个入口：

```
compact_v2/
  mod.rs          — CompactResult + run_compact 入口 + determine_compact_strategy
  micro.rs        — micro_compact() 实现
  full.rs         — full_compact_inner() + re_inject_v2() + extract 辅助函数
  smart.rs        — Smart Compact 占位（空分支标记 TODO）
```

## 风险

- **中高**：纯文件拆分，不改变逻辑，但 compact_v2 的 20+ 个私有函数和 `pub` 函数需正确分配子模块可见性
- `extract_file_info` / `extract_skill_names` 等公开辅助函数需通过 `pub use` 保持原有导出路径

## 实施要点

1. 创建 `compact_v2/` 目录，`mod.rs` 作为入口
2. `pub mod micro; pub mod full; pub mod smart;` 声明子模块
3. `micro_compact` → `micro.rs`，`full_compact_inner` + 辅助函数 → `full.rs`
4. `extract_file_info` / `extract_skill_names` 放 `full.rs`，用 `pub use` 重导出
5. stages/compact.rs 的调用路径不变（`crate::agent::compact_v2::run_compact`）

## 相关文件

- `peri-agent/src/agent/compact_v2.rs` — 待拆分
- `peri-agent/src/agent/compact/mod.rs` — 可能需要重新导出路径
- `peri-agent/src/agent/stages/compact.rs` — 调用方，路径需验证不变
