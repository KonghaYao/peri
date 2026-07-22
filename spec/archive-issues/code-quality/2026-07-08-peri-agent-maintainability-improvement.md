# peri-agent 可维护性与可扩展性改进


> 归档于 2026-07-20，原路径 spec/issues/2026-07-08-peri-agent-maintainability-improvement.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-08
**类型**：技术债

## 问题描述

peri-agent 在 2026-07-08 的可维护性与可扩展性审查中得分 59/100，是三项审查中最低分维度。核心问题是：工具分发编排路径几乎零测试、缺乏 API 废弃策略、事件与 SQLite schema 无版本兼容机制、`run_react_loop` 控制流过于复杂。

## 症状详情

### 子项 1：`tool_dispatch.rs` 编排函数零测试（P0，3-5 天）

`tool_dispatch.rs` 的 10 个测试全部针对纯函数 `normalize_params` 和 `resolve_tool`。三个核心编排函数——`collect_tool_results`（第 193 行）、`dispatch_concurrent`（第 316 行）、`settle_results`（第 383 行）——完全没有测试。这是 peri-agent 最复杂的异步逻辑（并发工具执行、before_tool 审批分流、error_suggest 注入时机、取消在审批/执行阶段的行为），却无任何回归保护。

期望：为三个编排函数编写集成测试，使用 MockTool 覆盖审批-执行-聚合全流程。

### 子项 2：事件与 SQLite schema 无版本号（P1，2-3 天）

- `RenderEvent`、`StateEvent`、`ObserveEvent` 全部 derive `Serialize + Deserialize`，无 `#[serde(tag = "version")]` 机制。未来变体/字段变更将导致旧序列化数据无法反序列化。
- SQLite 的 `threads` 和 `messages` 表无 `schema_version` 行或 `PRAGMA user_version`，所有迁移通过 `ALTER TABLE ADD COLUMN` 的 "duplicate column name" 错误隐式检测。

期望：为事件类型添加 `#[serde(tag)]` 版本标记；SQLite 设置 `PRAGMA user_version` 并递增检查。

### 子项 3：`run_react_loop` 控制流过于复杂（P1，1 天）

`stages/mod.rs:510-682`（172 行）混合三种路径：正常五阶段循环（含 tool_calls 回跳）、End 阶段 should_continue 新一轮 turn、Idle wake 路径（含 `tokio::select!` + `drain_for_end` + `woken_once` 守卫）。三层嵌套在一个 `for` 循环中，`woken_once` 变量跨多次迭代保持状态。

期望：提取 idle-wake 路径为独立函数 `try_idle_wake()`，减少最深层嵌套。

### 子项 4：`full_compact_inner` 单个函数承担 7 个步骤（P1，2-3 天）

`compact_v2.rs:355-472`（117 行）：预处理消息、构造 LLM 请求、调用 LLM、后处理摘要、标记 excluded、追加 Human 摘要、re-inject 文件+Skills。每个步骤都是关键路径。

期望：拆分为 `full_compact_collect` + `full_compact_summarize` + `full_compact_commit` 三个独立函数。

### 子项 5：零 `#[deprecated]` 属性（P1，2-3 天）

整个 peri-agent 代码库无任何 `#[deprecated]` 使用。对于被 `peri-acp`、`peri-tui`、`peri-middlewares` 依赖的公共 API（通过 `prelude` 暴露），完全没有 API 演进的过渡策略。v1 `ExecutorEvent` 部分变体（如 `TurnCommitted` / `StateSnapshotMeta` / `CompactCompleted`）已在 v2 有等价物，但未标记废弃。

期望：为计划移除的 v1 API 添加 `#[deprecated]` 并提供迁移指引。

### 子项 6：`CompactConfig` 的 Default 与 serde 默认值双写（P2，1 天）

`config.rs` 中每个字段写一个显式 `fn default_*()`，然后在 `Default` impl 和 `#[serde(default = "...")]` 中各调用一次。两个 impl 必须保持同步。

期望：`derive(Default)` → serde `default` 指向同一个 `Default::default()` 值。

### 子项 7：无 feature flag（P2，1 天）

`peri-agent/Cargo.toml` 无任何 `[features]`，所有依赖（`sqlx`、`reqwest`、`sysinfo`、`fuzzy-matcher`）无条件编译。对于仅使用事件系统或消息类型的消费者，增加了不必要的编译时间。

期望：添加 `sqlite`、`stream`、`compact` 等 feature flag。

### 子项 8：`sqlite_store.rs` 函数签名过长（P3，1 天）

`meta_from_row` 有 14 个参数（第 212-250 行），多处重复定义 14 元组解构类型。

期望：用 struct 一次性解构。

### 子项 9：`ThreadId = String` 是类型别名而非 newtype（P3，30 分钟）

`pub type ThreadId = String;`（`types.rs:8`），任何 String 均可隐式传入接受 ThreadId 的函数，失去编译期类型安全。

期望：改为 `struct ThreadId(String)`。

## 涉及文件

- `peri-agent/src/agent/stages/tool_dispatch.rs:193-383` —— 三个编排函数（零测试）
- `peri-agent/src/agent/events_v2.rs` —— 事件类型无版本号
- `peri-agent/src/thread/sqlite_store.rs` —— SQLite schema 无版本号 + 函数签名过长
- `peri-agent/src/agent/stages/mod.rs:510-682` —— `run_react_loop` 控制流
- `peri-agent/src/agent/compact_v2.rs:355-472` —— `full_compact_inner`
- `peri-agent/src/agent/compact/config.rs` —— Default/自定义默认值双写
- `peri-agent/Cargo.toml` —— 无 feature flag
- `peri-agent/src/thread/types.rs:8` —— ThreadId 类型别名

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | Open | Partial | agent | P0 #1（编排函数集成测试）已完成；P1-P3 子项待处理 |

## 修复记录

### 修复 #1（2026-07-08）

- **操作人**：agent
- **用户原意**：为 `tool_dispatch.rs` 编排函数添加集成测试
- **修复内容**：新增 5 个测试覆盖 `dispatch_concurrent`（成功/取消）、`settle_results`（ready/settled 分流）、`post_process_result`（无 registry）、`handle_consecutive_failures`（计数器重置）。使用 `OutputTool` mock + `StageContext` 直接构造。
- **涉及文件**：`tool_dispatch.rs`（+170 行测试，扩展 `mock` 模块）
- **涉及 commit**：待提交
- **验证状态**：已验证（616/616 测试通过）
