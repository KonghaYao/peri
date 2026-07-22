# P1-6：API stability 缺少编译期强制执行

**状态**：Open
**优先级**：低-中
**类型**：架构改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 模块化与分层维度

## Problem Statement

`peri-agent/src/lib.rs:10-21` 通过注释声明了三级 API stability 约定：

```
// === API Stability Levels ===
// [stable]   — 公共 API，不能破坏
// [unstable] — 实验中，可能变更
// [internal] — 仅内部使用
```

但此约定**无编译期 enforce**：
- 无 `#[doc(hidden)]` 隐藏 internal API
- 无 feature gate 控制 unstable API 的可见性
- 依赖开发者自觉遵守，code review 无法自动检查

## 建议方案

1. **Internal API**：标记 `#[doc(hidden)]` + `pub(crate)` 限制可见性
2. **Unstable API**：用 feature gate（如 `#[cfg(feature = "unstable")]`）包裹，外部 crate 需显式启用
3. **Stable 边界审计**：将 `peri-agent/src/lib.rs` 的导出列表与 `pub use` 对齐，确保 `pub` 导出 = public API

## 涉及文件

- `peri-agent/src/lib.rs:10-21` — stability 声明
- `peri-agent/src/` 下所有 `pub` 导出

## 实施记录

### 阶段 1（2026-07-22 已完成）：`#[doc(hidden)]` + 审计

**修改文件**：
- `peri-agent/src/lib.rs` — prelude 分离为 stable 区 + `#[doc(hidden)]` 内部区；更新 stability 文档注释
- `peri-agent/src/agent/mod.rs` — 8 个内部子模块 + 4 个 re-export 标记 `#[doc(hidden)]`
- `peri-agent/src/session/mod.rs` — 3 个 transcript 内部类型标记 `#[doc(hidden)]`

**加了 `#[doc(hidden)]` 的 API（18 个类型）**：

| 类型 | 所在模块 | 原因 |
|------|---------|------|
| `ExecutorEvent` | `agent::events` | 事件系统桥接类型，外部不应直接构造 |
| `AgentEventHandler` | `agent::events` | 事件处理器桥接 trait |
| `FnEventHandler` | `agent::events` | 内部事件处理适配器 |
| `Event`, `EventBus`, `EventBusConfig` | `agent::events_v2` | v2 事件总线，ACP 桥接专用 |
| `EventHandles`, `ObserveEvent`, `RenderEvent`, `StateEvent`, `TurnErrorReason` | `agent::events_v2` | v2 事件基础设施 |
| `AgentState` | `agent::state` | 内部 Agent 状态，非公共 API |
| `ContextBudget`, `TokenTracker` | `agent::token` | 内部 token 追踪 |
| `AgentCancellationToken` | `agent` (re-export) | tokio_util 桥接类型 |
| `AgentGroup` | `group` | 内部 Agent 管理 |
| `BatchItem`, `HitlDecision` | `hitl` | 已废弃，迁移到 `interaction` |
| `LoggingMiddleware`, `MetricsMiddleware` | `middleware::base` | 内部中间件实现 |
| `MessageFlags`, `StagedData`, `TranscriptEntry` | `session::transcript` | 内部 transcript 数据 |

**加了 `#[doc(hidden)]` 的子模块（8 个）**：
- `agent::agent_context` — `AgentContext` v2 封装
- `agent::compact_v2` — compact 内部实现
- `agent::events_v2` — v2 事件系统
- `agent::events_v2_mapper` — v2→v1 事件映射
- `agent::session` — 异步 owner（SessionInbox, CronOwner, ChannelOwner）
- `agent::stages` — ReAct 循环引擎
- `agent::state` — AgentState 定义
- `agent::subagent_event_forwarder` — SubAgent 事件转发
- `agent::token` — TokenTracker / ContextBudget 定义

**编译验证**：`cargo build -p peri-agent -p peri-acp -p peri-tui` ✅ / `cargo test -p peri-agent --lib` 624 passed ✅

**没有变为 `pub(crate)` 的原因**：所有 internal/bridge 类型均被 peri-acp / peri-middlewares 通过 full path 引用，无法收紧为 `pub(crate)`。

---

### 阶段 2 计划：Feature Gate 方案

**目标**：用 `#[cfg(feature = "unstable")]` 控制 unstable API 的编译期可见性，外部 crate 必须显式 `peri-agent = { features = ["unstable"] }` 才能使用。

**设计**：

```toml
# Cargo.toml
[features]
default = []
unstable = []  # 启用后解锁 5 个模块的类型导出
```

**候选 unstable 类型**（目前无稳定替代方案，暴露但标注 unstable）：

| 类型/模块 | 当前可见性 | feature gate 后 |
|-----------|-----------|----------------|
| `goal::*`（GoalController/GoalStateView 等） | `pub` 可见 | 需 `unstable` feature |
| `interaction::*`（UserInteractionBroker 等） | `pub` 可见 | 需 `unstable` feature |
| `error_suggest::*`（ErrorSuggestRegistry 等） | `pub` 可见 | 需 `unstable` feature |
| `metrics::emit` | `pub fn` 可见 | 需 `unstable` feature |
| `agent::session::*`（SessionInbox 等） | `#[doc(hidden)]` | 需 `unstable` feature |

**实施顺序**：
1. 新增 `unstable` feature flag（Cargo.toml）
2. 用 `#[cfg(feature = "unstable")]` 包裹上述模块的 `pub mod` 声明
3. peri-acp / peri-middlewares 的 Cargo.toml 添加 `peri-agent = { features = ["unstable"] }`
4. peri-tui 不添加 feature → 编译期阻断非法依赖

**风险**：
- 约 6 个 crate（peri-acp, peri-middlewares, peri-workflow, peri-tui 等）需同步更新 Cargo.toml
- `goal::*` 和 `interaction::*` 是 BRIDGE trait，拆出可能导致循环依赖——考虑创建 `peri-bridge-types` crate 或保持现状仅标注文档
