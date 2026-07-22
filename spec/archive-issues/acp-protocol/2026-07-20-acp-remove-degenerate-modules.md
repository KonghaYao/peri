# 删除 peri-acp 退化模块（空壳/反模式）

**状态**：Fixed
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #3（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp/src/` 中存在 5 个接口面积不小于实现的退化模块和反模式：

| 模块 | 文件 | 实质内容 | 问题 |
|------|------|---------|------|
| `lsp/` | `mod.rs` 10 行 | `pub use peri_lsp::config::LspServerConfig;` | 纯 re-export，可直接用源 crate |
| `hooks/` | `mod.rs` 10 行 | `pub use peri_middlewares::hooks::types::RegisteredHook;` | 纯 re-export |
| `event/dto.rs` | 30 行 | `pub use peri_acp_types::*;` | 退化 re-export |
| `session/frozen.rs` | 35 行 | `FrozenSessionData::build(cfg)` 纯委托 | 薄包装，可内联到 executor_helpers |
| `event/v2_channel.rs` | 27 行 | `OnceLock<Sender>` 全局单例 | 全局可变状态，无法测试/重置，初始化顺序依赖 |

**影响**：
- 调用方需要知道这些文件的存在，但它们不提供任何抽象
- 增加了代码库的认知负载（11 个模块中 4 个是空壳）
- `v2_channel` 的全局 `OnceLock` 违反 Rust 所有权原则，初始化失败时 panic
- 删除测试验证：这些模块如果被删除，不会导致任何逻辑复杂度分散到调用方

## 建议方案

### 直接删除（lsp/、hooks/、dto.rs）
调用方直接 `use peri_lsp` / `use peri_middlewares` / `use peri_acp_types`。

### 内联（frozen.rs）
将 `build_frozen_session_data` 函数内联到 `executor_helpers.rs` 中，或直接在调用点调用 `FrozenSessionData::build()`。

### 重构（v2_channel.rs）
将 v2 事件通道改为从 `SessionContext` 持有的 channel 读取，消除全局 `OnceLock`。具体方案：
1. 在 `SessionContext` 中增加 `v2_event_tx: Option<Sender<V2Event>>`
2. TUI 入口在构造 `SessionContext` 时注入
3. `spawn_eventbus_forwarder` 从 `SessionContext` 读取 channel

## 涉及文件

| 文件 | 操作 |
|------|------|
| `lsp/mod.rs` | 删除，调用方直接 use `peri_lsp` |
| `hooks/mod.rs` | 删除，调用方直接 use `peri_middlewares` |
| `event/dto.rs` | 删除，调用方直接 use `peri_acp_types` |
| `session/frozen.rs` | 删除，逻辑内联到 `executor_helpers.rs` |
| `event/v2_channel.rs` | 删除，改为 SessionContext 持有 channel |
| `event/mod.rs` | 移除 `mod v2_channel` |
| `session/mod.rs` | 移除 `mod frozen` |

## 收益

- **认知负载**：移除 4 个空壳文件 + 1 个全局单例
- **locality**：`v2_channel` 的 channel 注入和执行在同一个 `SessionContext` 生命周期内
- **可测试性**：`v2_channel` 改为可注入后，forwarder 可独立测试
- 删除测试验证通过（复杂度不被分散）

## 前置依赖

- 合并 AcpAgentConfig + PromptExecutionContext（`2026-07-20-acp-merge-config-params.md`）——`frozen.rs` 的内联和 `v2_channel` 的重构依赖 `SessionContext` 的存在

## 风险

- `v2_channel.rs` 的重构需要确认当前 TUI 入口的 `set_v2_event_tx` 调用时机，确保迁移不破坏初始化顺序
- `lsp/` 和 `hooks/` 删除需要确认没有外部 crate 依赖这些 re-export 路径

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`fdb56164 refactor(peri-acp): 删除 5 个退化模块，净减少 146 行`
- **修复内容**：删除 lsp/、hooks/、dto.rs、frozen.rs、v2_channel.rs 退化模块，调用方直接使用源 crate
