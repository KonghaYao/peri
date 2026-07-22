# peri-agent 残留代码清理报告

> **状态更新（2026-07-22）**：22 项中已有 5+ 项通过后续 commits 处理（AcpAgentConfig 删除、退化模块清理、SubAgent EventBus 死代码清理、Langfuse v1/v2 统一路由、transport pending map 统一）。详见 git log `c5431160`、`fdb56164`、`5c1a29bc`、`48ebfed6`、`a6153771`。

**扫描日期**: 2026-07-18  
**目标 crate**: `peri-agent/src/` (90 个 .rs 源文件)  
**扫描维度**: 死代码 / 废弃模式 / 编码规范 / TODO&债务

---

## 1. 总体概要

| 严重程度 | 数量 | 涉及领域 |
|---------|:----:|---------|
| 🔴 高 (P0) | 6 | 废弃事件总线、重复类型定义、无消费者模块、测试文件位置违规 |
| 🟡 中 (P1) | 9 | v1→v2 桥接层、双套类型模型、仅测试消费的 pub 函数、clippy 注释缺失 |
| 🟢 低 (P2) | 7 | 冗余 `#[allow]` 标注、存根代码、Safe 字节索引、async 阻塞 I/O |
| **合计** | **22** | |

---

## 2. 高优先级问题 (P0/HIGH)

### P0-1: v1 ExecutorEvent 枚举仍为活跃通道，零 `#[deprecated]` 属性

**来源**: A-无直接对应 / B-1, B-9 / D-无

**文件**: `agent/events.rs:179-444`

尽管 v2 已建三层 EventBus（Render/State/Observe），约 50 个变体的 `ExecutorEvent` 枚举仍被 `AgentGroup`（`group/mod.rs:27`）、`subagent_event_forwarder.rs:25`、`events_v2_mapper.rs` 引用。`TurnCommitted`/`StateSnapshot`/`CompactStarted`/`CompactCompleted` 等在 v2 已有等价物，但枚举中**没有任何 `#[deprecated]` 属性**——整个 `peri-agent/src/` 中 `#[deprecated]` 使用量为零，导致 API 演进无过渡策略。

**影响**: 下游三个 crate（peri-acp、peri-tui、peri-middlewares）必须同时处理 v1 和 v2 事件通道，桥接开销（`events_v2_mapper.rs`）持续存在。

**建议**: 为每个在 v2 已有等价物的变体添加 `#[deprecated(since = "0.2.0", note = "Use events_v2::ObserveEvent::...")]`，待下游全部切换到 v2 通道后物理删除。

---

### P0-2: `group` 模块零外部消费者

**来源**: A-3 / B-6 / D-无

**文件**: `group/mod.rs`

`AgentGroup`、`AgentHandle`、`AgentPipeline`、`AgentId`、`CancelPolicy` 等全部公开类型在 `peri-agent` 外部无任何消费方。搜索 `peri_agent::group::` 在全仓库 `.rs` 文件中返回零匹配。仅在 `lib.rs` prelude 中 re-export 但无实际使用。该模块设计为 v2 架构的会话级 Agent 管理器，但 SubAgent 当前走 `execute_fork.rs`/`execute_bg.rs` 直接管理生命周期，不走 AgentGroup。

**影响**: 约 500+ 行代码无生产消费者，且 `AgentGroup` 仍绑定 v1 `ExecutorEvent`（`event_tx: UnboundedSender<ExecutorEvent>`），拖慢 v1 事件总线的下线进程。

**建议**: 
- 若确认 `AgentGroup` 长期无消费者，从 prelude 移除相关导出
- 或在设计文档中明确记录其定位（如"计划中的会话级 Agent 编排器"），避免被误认为可删除

---

### P0-3: `CancelPolicy` 三套互不兼容的重复定义

**来源**: A-4 / B-无 / D-无

| 定义位置 | 可见性 | 变体顺序 | Serde | 消费者 |
|---------|--------|---------|:-----:|--------|
| `group/mod.rs:34` | `pub` | `Independent`, `Cascade` | 无 | 无（group 模块无外部消费者） |
| `thread/types.rs:17` | `pub` | `Cascade`, `Independent` | ✅ | ThreadMeta 持久化（正确版本） |
| `subagent/tool/build_agent.rs:20` (peri-middlewares) | `pub(crate)` | — | — | SubAgent 工具 |

三份定义互不兼容（无法互相转换，Default 值相反），存在语义漂移风险。

**建议**: 删除 `group/mod.rs` 中的 `CancelPolicy`（group 模块本身无消费者），统一使用 `thread/types.rs` 版本（带有完整 serde 支持）。

---

### P0-4: 13 个文件内联测试超过 30 行，需外置为 `_test.rs`

**来源**: C-3 / 其他扫描无对应

规范要求：测试代码 ≥ 30 行 → 同目录 `_test.rs` 文件；< 30 行 → 同文件 `#[cfg(test)]`。

以下文件均违反此规则：

| 文件 | 测试行数 |
|------|:------:|
| `agent/events_v2.rs` | 734 |
| `agent/subagent_event_forwarder.rs` | 473 |
| `agent/events_v2_mapper.rs` | 431 |
| `agent/stages/mod.rs` | 410 |
| `agent/stages/tool_dispatch.rs` | 301 |
| `agent/agent_context.rs` | 213 |
| `agent/session/inbox.rs` | 186 |
| `agent/session/channel_owner.rs` | 160 |
| `agent/stages/reason.rs` | 115 |
| `agent/session/cron_owner.rs` | 113 |
| `agent/stages/receive.rs` | 91 |
| `agent/stages/middleware_runner.rs` | 81 |
| `agent/stages/act.rs` | 66 |

**影响**: 源文件体积膨胀，内联测试与实现代码混杂，降低可读性。部分文件测试代码超过主干代码的 2-3 倍。

**建议**: 逐文件分离为 `<module>_test.rs`，保持源文件干净。注意移动后 `mod tests` 声明和 `use super::*` 导入路径需要调整。

---

### P0-5: v2→v1 事件桥接层 `events_v2_mapper.rs` 存在语义丢失

**来源**: B-2, B-5 / A-无 / D-无

**文件**: `agent/events_v2_mapper.rs`

该模块将 v2 事件桥接为 v1 `ExecutorEvent`，供 TUI 消费。其中三处 `message_id: Default::default()`（L23-L53）是空值占位——v2 无 `message_id` 概念，桥接时填默认值会丢失语义。另 `inject_source_agent_id`（`agent/events.rs:477-503`）是硬编码的 match 四个变体的事后补丁函数，非构造时自然携带。

**与 P0-1 联动**: TUI 切换到 v2 事件通道后，此文件和 `inject_source_agent_id` 均应立即删除。

---

### P0-6: `ask_user` vs `interaction` 双套平行类型模型

**来源**: B-11 / A-无 / D-无

| 模块 | 核心类型 | 状态 |
|------|---------|------|
| `ask_user/mod.rs` | `AskUserQuestionData`/`AskUserOption`/`AskUserBatchRequest` | 仍活跃（peri-middlewares `pub use peri_agent::ask_user::*`） |
| `interaction/mod.rs` | `QuestionItem`/`QuestionOption`/`InteractionContext::Questions` | 新统一方案（HITL + AskUser 两条路径） |

两套独立类型模型表达同一概念，`interaction` 是"统一人机交互"的新方案，但 `ask_user` 仍被下游直接依赖。

**建议**: `ask_user` 类型迁移到 `interaction` 模型统一。短期在 `ask_user` 模块加 `#[deprecated]` 引导下游切换。

---

## 3. 中优先级问题 (P1/MEDIUM)

### P1-1: v1→v2 Middleware 桥接层

**来源**: B-3, B-4, B-10 / A-无 / D-无

涉及三个文件：
- `agent/stages/middleware_runner.rs` — 每次 hook 调用构造新的 `AgentContext` + clone `visible_messages`（O(n)）
- `agent/agent_context.rs` — 4 个 no-op 方法（`set_cwd()`、`set_current_step()`、`store()`、`own_thread_id()`）仅为适配兼容
- `MiddlewareState` trait 中对应方法也已过时但未标记 `#[deprecated]`

**建议**: 中期方案是 middleware trait 直接接受 v2 native 参数。短期为 no-op 方法添加 `#[deprecated]` 并标注替代方案。

---

### P1-2: `metrics::current_rss_mb()` / `total_system_memory_mb()` 仅测试消费

**来源**: A-7, A-8 / 其他扫描无对应

**文件**: `metrics/mod.rs:56,72`

两个 `pub fn` 仅被 `metrics/mod_test.rs` 中 4 个测试用例调用，无生产代码使用。

**建议**: 若无计划在短期内接入生产代码，标注 `#[doc(hidden)]` 防止外部误用，或改为 `pub(crate)`。

---

### P1-3: v1 compact 目录外壳

**来源**: B-8 / D-4 / A-无

**文件**: `agent/compact/mod.rs:1-23`

模块内容几乎全空（仅 re-export `CompactConfig` 和 `CONTINUATION_HINT`），文档标记"v1 compact 主体已物理删除"。目录名 `compact` 容易让人以为 v1 代码还活着。

**建议**: 将 `config.rs` 上移到 `compact_v2/` 下，并删除当前 compact 目录外壳。

---

### P1-4: `#[allow(clippy::too_many_arguments)]` 缺少注释

**来源**: C-4 / 其他扫描无对应

**文件**: `thread/sqlite_store.rs:214`

规范要求每个 `#[allow(clippy::xxx)]` 都需注释说明为何抑制此警告。

**建议**: 添加注释说明 `meta_from_row` 函数为何需要多个参数。

---

### P1-5: Smart Compact 存根与降级逻辑

**来源**: B-7 / D-1 / A-无

**文件**: `agent/compact_v2/smart.rs:9`  
关联: `agent/compact_v2/mod.rs:164-174`（Smart 分支降级为 Micro）

`CompactStrategy::Smart` 枚举值已定义，事件层和测试层已覆盖该变体，但运行时全路径降级为 Micro Compact。`smart.rs` 是一个仅 9 行的空壳，注释描述设计意图但未实现。

**建议**: 属于纯增量功能，优先级低。实现后需移除降级逻辑 + 重跑相关测试。

---

### P1-6: `thread/sqlite_store.rs:43` — async 上下文中调用阻塞 `std::fs::create_dir_all`

**来源**: C-1 / 其他扫描无对应

`std::fs::create_dir_all` 在 `pub async fn new()` 内直接调用。虽然位于首个 `.await` 之前且为一次性初始化，影响极小，但规范上应使用 `tokio::fs::create_dir_all`。

---

### P1-7: `agent/compact_v2/full.rs:330` — 字节索引切片模式

**来源**: C-2 / 其他扫描无对应

`&text[..start]` 和 `&text[remove_end..]` 使用 `std::str::find` 返回的字节索引。搜索目标 `<analysis>`/`</summary>` 均为纯 ASCII 标签，技术上安全，但字节切片模式是代码坏味道——如果未来标签含非 ASCII 字符会 panic。建议用 `char_indices` 或显式添加 ASCII 断言。

---

### P1-8: `tool_dispatch.rs` 过时注释

**来源**: B-13 / 其他扫描无对应

**文件**: `agent/stages/tool_dispatch.rs:5`

注释提到"AgentState 调用 middleware chain"，实际代码已改用 `StageContext`。

---

### P1-9: `AgentGroup` 仍使用 v1 ExecutorEvent 通道

**来源**: B-6 / A-3 / D-无

**文件**: `group/mod.rs:27`

`event_tx: UnboundedSender<ExecutorEvent>` 直接绑定 v1 事件通道，拖慢 v1 事件总线下线。与 P0-2 联动。

---

## 4. 低优先级问题 (P2/LOW)

### P2-1: `AsyncOwners` 的冗余 `#[allow(dead_code)]`

**来源**: A-1 / D-2 / 无交叉

**文件**: `session/mod.rs:45`

`AsyncOwners` 在 `mod.rs:233` 构造，各字段通过公开方法间接访问，不是死代码。`#[allow(dead_code)]` 可能是开发初期添加后遗留。验证移除后是否触发 clippy，若无则删除。

---

### P2-2: `parse_assistant_message()` 的冗余 `#[allow(dead_code)]`

**来源**: A-2 / D-3 / 无交叉

**文件**: `llm/openai/adapter.rs:481`

该函数被 `openai/mod.rs:106` 调用、被 4 个 `openai_test.rs` 测试引用，明确不是死代码。`#[allow(dead_code)]` 可直接安全移除。

---

### P2-3: `inject_source_agent_id` 事后补丁函数

**来源**: B-5 / 与 P0-5 联动

**文件**: `agent/events.rs:477-503`

SubAgent 转发器在构造 `ExecutorEvent` 后调用此函数注入 `source_agent_id`。属硬编码的 match 四个变体，非构造时自然携带。改造方式：在 `events_v2_mapper` 转换时直接设置为 mapper 参数，删除独立函数。

---

### P2-4: Smart Compact 未实现 (见 P1-5 详情)

已在中优先级记录。

---

### P2-5: v1 compact 目录外壳 (见 P1-3 详情)

已在中优先级记录。

---

## 5. 非问题项说明

以下项目被扫描到但经过核实属于正常设计或非目标 crate 范围：

| 项目 | 说明 |
|------|------|
| `CancelPolicy` — `thread/types.rs:17` 版本 | 正确版本，带完整 serde 支持，用于 ThreadMeta 持久化。**不处理**。 |
| `CancelPolicy` — `subagent/tool/build_agent.rs:20` 版本 | peri-middlewares 内部专用，`pub(crate)` 可见性。**不处理**。 |
| `ask_user/mod.rs` 和 `hitl/mod.rs` 模块 | 纯数据类型，分别被 peri-middlewares 通过 `pub use` 消费。**活跃代码，不处理**。 |
| `interaction` 模块 | "统一人机交互"新方案，UserInteractionBroker 等多方活跃消费。**活跃代码，不处理**。 |
| `agent/compact/config.rs` 的 10 个 `default_*()` 函数 | 全部被 `#[serde(default = "...")]` 或 `Default` impl 消费。**无死代码**。 |
| 所有 `_test.rs` 文件 | 对应的源模块均存在且被 mod.rs 引用。**无孤立测试文件**。 |
| 中间件链 (`middleware/chain.rs`, `base.rs`) | `LoggingMiddleware`/`MetricsMiddleware` 均为活跃使用。**不处理**。 |
| `#[async_trait]` 覆盖 / `std::sync::RwLock` 使用 / `println!`/`eprintln!`/`dbg!` 宏 | 三项目标 crate 内零违规。**合格**。 |
| `background.rs:167` 的 `#[deprecated]` | 位于 `peri-middlewares`，非目标 crate。**已标记，无需额外操作**。 |
| `.len()` 当终端列宽 / 导入通配符排序 | 零违规。**合格**。 |
| 测试中的 `#[ignore]` | 目标 crate 内未发现。两个 `#[ignore]` 在 peri-middlewares（需 network），不在范围。 |
| FIXME / HACK / XXX 标记 | 目标 crate 内零发现。**合格**。 |

---

## 6. 建议的清理计划

### 第一阶段（本周 — 低风险快速清理）

| # | 事项 | 涉及文件 | 工作量 |
|---|------|---------|:---:|
| 1 | 移除 `AsyncOwners` 的 `#[allow(dead_code)]`，编译验证 | `session/mod.rs` | 5min |
| 2 | 移除 `parse_assistant_message()` 的 `#[allow(dead_code)]` | `llm/openai/adapter.rs` | 2min |
| 3 | 为 `sqlite_store.rs` `#[allow(clippy::too_many_arguments)]` 添加注释 | `thread/sqlite_store.rs` | 2min |
| 4 | 更新 `tool_dispatch.rs` 过时注释 | `agent/stages/tool_dispatch.rs` | 1min |
| 5 | 将 `std::fs::create_dir_all` 改为 `tokio::fs::create_dir_all` | `thread/sqlite_store.rs` | 2min |

**零风险，总工作量 < 15 分钟。**

---

### 第二阶段（本周 — 测试文件分离）

| # | 事项 | 涉及文件 | 工作量 |
|---|------|---------|:---:|
| 6 | 分离 13 个超过 30 行的内联测试到 `_test.rs` | 见表 P0-4 | 2-3h |

**注意**: 每个文件分离后需跑 `cargo test -p peri-agent --lib -- <test_name>` 验证。批量搬移，不修改测试逻辑。

---

### 第三阶段（下周 — 类型系统清理）

| # | 事项 | 涉及文件 | 工作量 |
|---|------|---------|:---:|
| 7 | 删除 `group/mod.rs` 中的 `CancelPolicy`，统一使用 `thread/types.rs` 版本 | `group/mod.rs` | 30min |
| 8 | 为 `metrics::current_rss_mb()` / `total_system_memory_mb()` 添加 `#[doc(hidden)]` | `metrics/mod.rs` | 2min |
| 9 | 为 `AgentContext` 的 4 个 no-op 方法添加 `#[deprecated]` | `agent/agent_context.rs` | 5min |
| 10 | 在 `ask_user/mod.rs` 添加 `#[deprecated]`，引导切换到 `interaction` | `ask_user/mod.rs` | 5min |

---

### 第四阶段（月内 — 架构债务治理）

| # | 事项 | 涉及文件 | 工作量 |
|---|------|---------|:---:|
| 11 | 确认 `AgentGroup` 的定位（保留并文档化 or 从 prelude 移除） | `group/mod.rs`, `lib.rs` | 1h |
| 12 | v1→v2 middleware 桥接层性能评估，设计 native v2 接口 | `middleware_runner.rs`, `agent_context.rs` | 2-3h |
| 13 | 为 v1 `ExecutorEvent` 在 v2 已有等价物的变体逐变体添加 `#[deprecated]` | `agent/events.rs` | 1h |
| 14 | 探索 TUI 直连 v2 事件通道的可行性（消除 `events_v2_mapper.rs`） | 跨 crate | 1-2d |

---

### 长期（不设截止日）

| # | 事项 |
|---|------|
| 15 | 实现 Smart Compact（`compact_v2/smart.rs`） |
| 16 | 将 `compact/config.rs` 上移到 `compact_v2/`，消除 v1 目录外壳 |
| 17 | 全量下线 v1 `ExecutorEvent`，物理删除相关文件 |

---

*报告生成方式: 四个独立扫描结果交叉引用、去重、合并后人工编排。*
