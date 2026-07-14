# 候选 4：dispatch 薄壳群 + re-export 群合并为 dispatch registry

> 日期：2026-07-13 | 模块：`peri-acp/src/dispatch/` + 6 个 re-export 模块 | 类型：架构走读

---

## 1. 摘要

`peri-acp` 的 `dispatch/` 子树和 6 个跨模块 re-export 文件集中表现出一类典型的 shallow module 反模式：每个文件 10–42 LOC，封装的单个函数只是一个签名 + 一层 `match` / 一层 `.into()` / 一次 `tracing::warn!`，把它们整体删除并将同名函数搬进调用方，调用方复杂度不会上升。

本候选走 /grilling 流程逐一拷问这 12 个文件的 depth、leverage、locality，结论是：6 个 re-export 文件 + 1 个 dispatch 文件（`config_update`）通过 deletion test，应直接内联删除；剩余 5 个 dispatch 浅文件虽未通过 deletion test（存在真实跨 transport 复用），但当前分散在 6 个文件里是过度切分，应当并入一个集中式的 `dispatch/registry.rs`（加深方向 A）。re-export 全部内联属加深方向 C，与 A 独立。本设计给出 registry 的 Rust interface 草案、4 阶段迁移步骤、以及明确保留为深文件的 3 个模块（`prompt.rs` / `execute_command.rs` / `session_replay.rs`）边界。

---

## 2. 现状诊断

### 2.1 完整 12 文件清单

| # | 文件 | LOC | 接口数量 | 一句话职责 | 是否通过 deletion test |
|---|------|-----|----------|-----------|----------------------|
| 1 | `dispatch/init.rs:17` | 29 | 1 fn | 构造带全部 capabilities 的 `InitializeResponse` | ✅ 通过 |
| 2 | `dispatch/list_sessions.rs:9` | 35 | 1 async fn | `ThreadStore::list_threads` → `Vec<SessionInfo>`，附 `cwd` 过滤 | ✅ 通过 |
| 3 | `dispatch/config_update.rs:14` | 30 | 2 fn | 两层薄包装，最终委托到 `state_builders::build_config_options` | ✅ 通过 |
| 4 | `dispatch/commands.rs:8` | 34 | 1 fn | 拼 13 条内置命令 + skills → `Vec<AvailableCommand>` | ✅ 通过 |
| 5 | `dispatch/session_load.rs:13` | 27 | 1 async fn | `ThreadStore::load_context` + 错误吞咽（`Vec::new()`） | ✅ 通过 |
| 6 | `dispatch/session_fork.rs:13` | 42 | 1 async fn | `create_thread` + `append_messages`，返回 `(id, msgs)` | ✅ 通过 |
| 7 | `session/frozen.rs:20` | 34 | 1 fn | 纯 forward 到 `FrozenSessionData::build`（注释里已自认"薄包装"） | ✅ 通过 |
| 8 | `event/dto.rs:20` | 29 | re-export 5 类型 | 从 `peri-acp-types::summary` re-export | ✅ 通过 |
| 9 | `event/mapper_v2.rs:8` | 10 | re-export 4 符号 | 从 `peri_agent::agent::events_v2_mapper` re-export | ✅ 通过 |
| 10 | `hooks/mod.rs:9` | 9 | re-export 1 类型 | `pub use peri_middlewares::hooks::types::RegisteredHook` | ✅ 通过 |
| 11 | `lsp/mod.rs:9` | 9 | re-export 1 类型 | `pub use peri_lsp::config::LspServerConfig` | ✅ 通过 |
| 12 | `agent/mod.rs:11` | 11 | re-export 1 子模块 | `pub use builder::*`（mod.rs 本身仅做 module listing） | ⚠️ 部分通过（见 §2.4） |

**保留（deletion test 不过）的 3 个深文件**：

| 文件 | LOC | 保留理由 |
|------|-----|---------|
| `dispatch/prompt.rs` | 138 | 参数提取（`sessionId` / `message.content` / `attachments` 三分支 + `serde_json::from_value` 兜底）+ session lookup 错误处理 |
| `dispatch/execute_command.rs` | 226 | 13 个参数的执行上下文 + Immediate 命令分发 + 错误码体系（-32602 多分支）+ Passthrough/Transform 拒绝逻辑 |
| `dispatch/session_replay.rs` | 216 | ACP v1 spec 的协议映射：`BaseMessage` 4 变体 → `SessionUpdate` 6 变体（含 `periReplay=true` 标记）+ `ReplaySender` trait 抽象 |

### 2.2 dispatch 浅壳的完整签名

```rust
// dispatch/init.rs:17
pub fn build_initialize_response() -> InitializeResponse

// dispatch/list_sessions.rs:9
pub async fn list_sessions_as_info(
    thread_store: &dyn ThreadStore,
    cwd_filter: Option<&str>,
) -> Result<Vec<SessionInfo>>

// dispatch/config_update.rs:14
pub fn make_config_options(
    peri_config: &PeriConfig,
    provider: &LlmProvider,
    permission_mode: PermissionMode,
) -> Vec<SessionConfigOption>

pub fn make_config_option_update(
    peri_config: &PeriConfig,
    provider: &LlmProvider,
    permission_mode: PermissionMode,
) -> ConfigOptionUpdate

// dispatch/commands.rs:8
pub fn build_available_commands(skills: &[SkillMetadata]) -> Vec<AvailableCommand>

// dispatch/session_load.rs:13
pub async fn load_session_messages(
    thread_store: &dyn ThreadStore,
    thread_id: &str,
) -> Vec<BaseMessage>

// dispatch/session_fork.rs:13
pub async fn fork_session(
    thread_store: &dyn ThreadStore,
    source_thread_id: &str,
    source_messages: &[BaseMessage],
    cwd: &str,
) -> Result<(String, Vec<BaseMessage>)>
```

签名模式高度一致：

- 接收 `&dyn ThreadStore` 或纯数据切片（skills / messages）。
- 无内部状态，无 `&self`。
- 返回 DTO（`SessionInfo` / `AvailableCommand` / `ConfigOptionUpdate`）或 `Result<DTO>`。
- 错误处理策略简单：要么 `context()` 一层包装，要么 `warn!` 后吞咽。

### 2.3 re-export 群的真实形态

```rust
// session/frozen.rs:20-34  （注释里已写明"薄包装"）
pub fn build_frozen_session_data(
    cwd: &str,
    language: Option<&str>,
    plugin_skill_roots: &[SkillRoot],
    plugin_agent_dirs: &[PathBuf],
    frozen_date: &str,
) -> FrozenSessionData {
    FrozenSessionData::build(
        cwd, language, plugin_skill_roots, plugin_agent_dirs, frozen_date,
    )
}
```

`event/dto.rs` 全文除 doc comment 外只有一段：

```rust
pub use peri_acp_types::summary::{
    CompactFileInfoDto, StopReasonDto, TodoItemDto, TodoStatusDto, TokenUsageDto,
    WorkflowProgressDto,
};
```

`event/mapper_v2.rs` 全文业务部分仅一段：

```rust
pub use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
```

`hooks/mod.rs` / `lsp/mod.rs` 各自只有一行 `pub use`。这 4 个文件本身已经承认是兼容层（dto.rs 注释 "re-export 保持向后兼容"；frozen.rs 注释 "保留此自由函数以兼容既有调用点"）。

### 2.4 唯一例外：`agent/mod.rs`

`agent/mod.rs` 通过 deletion test 的程度最弱：它声明了 3 个 pub 子模块（`builder` / `builder_v2` / `workflow_agent`），其中 `workflow_agent` 没有其他自然入口。如果删掉整个 `agent/` 目录把子模块平移到 `crate::`，会污染 crate 顶层命名空间。本候选建议 **保留 `agent/mod.rs` 作为 module listing 入口**，但把 `pub use builder::*` 这种 re-export 拆掉（调用方改用 `peri_acp::agent::builder::xxx`），让 mod.rs 退化为纯目录索引。

### 2.5 调用图证据：所有 dispatch 函数只被 peri-tui 调用

在 `peri-acp/src/` 全树 grep `dispatch::`，**零内部命中**——dispatch 函数全部从 `peri-tui` 的 transport 层反向调用：

```
peri-tui/src/acp_stdio/transport.rs:8       use peri_acp::dispatch;
peri-tui/src/acp_stdio/transport.rs:19      responder.respond(dispatch::build_initialize_response())
peri-tui/src/acp_stdio/notification.rs:9    use peri_acp::dispatch;
peri-tui/src/acp_stdio/notification.rs:23   dispatch::config_update::make_config_options(...)
peri-tui/src/acp_stdio/commands.rs:22       peri_acp::dispatch::build_available_commands(&skills)
peri-tui/src/acp_stdio/session/control.rs:14 peri_acp::dispatch::list_sessions_as_info(...)
peri-tui/src/acp_server/requests.rs:13      use peri_acp::dispatch::config_update::make_config_options;
peri-tui/src/acp_server/notify.rs:141       peri_acp::dispatch::build_available_commands(skills);
```

这是关键的 leverage 证据：这 6 个 dispatch 函数的"复用"只发生在两条 transport 路径之间（`acp_server/` MpscTransport + `acp_stdio/` StdioTransport），而 v2 设计文档 §2.1 已明确"传输层只做帧编解码，不参与业务"——dispatch 这一层是真实存在的 seam，但每个方法只是一个 fn 大小，分文件 vs 合并的边际收益倾向合并。

---

## 3. 约束

### 3.1 ACP 协议方法表（来自 `docs/design/peri-acp-protocol.md` §2）

registry 必须覆盖以下 JSON-RPC method：

| 方法 | 当前承载文件 | registry 行为 |
|------|-------------|--------------|
| `initialize` | `dispatch/init.rs` | 同步构造 `InitializeResponse` |
| `session/new` | （散落在 transport 层 + `session/frozen.rs`） | registry 不接管，仍由 transport 编排 frozen 数据 |
| `session/load` | `dispatch/session_load.rs` + `dispatch/session_replay.rs` | registry 提供 load_messages + replay_history 两个原子 |
| `session/list` | `dispatch/list_sessions.rs` | 同步包装 async 查询 |
| `session/prompt` | `dispatch/prompt.rs`（保留为深文件） | registry 转发到深文件 handler |
| `session/cancel` | （在 transport 内联） | registry 不接管 |
| `session/close` | （在 transport 内联） | registry 不接管 |
| `session/fork` | `dispatch/session_fork.rs` | registry 提供 fork 原子 |
| `session/execute-command` | `dispatch/execute_command.rs`（保留为深文件） | registry 转发 |
| `config/update` | （`session/switch-model` / `session/switch-provider` 共享 `make_config_option_update`） | registry 提供 config_options builder |
| `session/update`（通知） | `dispatch/session_replay.rs` | replay 序列通过 sender trait 推送 |

**registry 不试图覆盖所有 method**——它只把"返回纯数据 / DTO"的那部分抽进来；涉及 session 状态、流式推送、async runtime 调度的（如 `session/new` 的 frozen 编排、`session/prompt` 的 `run_session_loop` 触发）仍然留在 transport 层调用深文件。

### 3.2 transport 无关

`dispatch/mod.rs` 头部注释（行 1-6）已经声明：

> Provides pure functions that implement ACP session lifecycle operations. Both TUI (MpscTransport) and stdio transports call these functions, keeping only JSON-RPC framing and session-state management in their respective transport layers.

registry 必须维持这条不变量：

- **registry handler 是纯函数**——不持有 `&self`、不访问全局状态、不调用 `tokio::spawn`。
- 所有 async handler 通过 `async fn` 而非 `impl Future + Send + Sync` 之类装箱，避免动态分发开销。
- 跨 transport 差异（如 IDE 不支持 `periReplay=true` 标记）通过参数注入而非全局 flag 表达。

### 3.3 JSON-RPC 分发必须是纯函数

v2 设计文档 §2.2 行 89：

> dispatch 函数是纯函数——接收输入参数，返回结果。不持有 session 状态。

registry 落地时这条约束直接转写为：

```rust
// dispatch/registry.rs 中的 handler trait
pub trait AcpMethodHandler: Send + Sync {
    type Params: serde::de::DeserializeOwned;
    type Result: serde::Serialize;
    async fn handle(&self, params: Self::Params, ctx: &AcpDispatchCtx<'_>)
        -> Result<Self::Result, AcpError>;
}
```

注意 `ctx: &AcpDispatchCtx` 是只读引用 + 显式注入（`ThreadStore` / `PeriConfig` / `LlmProvider`），保证 handler 不会偷偷从 thread-local 或全局 atom 取状态。

---

## 4. 依赖关系

### 4.1 前置依赖

**无**。本候选不需要候选 1/2/3 中的任何前置改造。

### 4.2 后置依赖

- **候选 5（mapper 重命名）**：本候选落地后 `event/mapper_v2.rs` 会被删除，候选 5 在此基础上做 `event/mapper.rs` → `event/mod.rs` 的扁平化收益更大。两候选建议同一 PR 链内完成。
- **外部 crate 兼容**：`peri-tui` 是 dispatch 的唯一消费方，可以同步迁移；理论上若有第三方 crate 通过 `peri_acp::dispatch::init::build_initialize_response` 引用（实际查 grep 无此情形），需要在 v0.x 阶段保留 forward shim。

### 4.3 平行候选

- **候选 3（中间件链迁出，builder.rs 瘦身）**：与本候选完全独立。builder.rs 不在 dispatch/ 子树，候选 3 不影响 registry 的 handler 签名。两候选可并行推进，不产生 merge conflict。
- **候选 1/2**：与本候选无交集。

---

## 5. 加深后的模块形状

### 5.1 `dispatch/registry.rs` Rust interface 草案

```rust
//! ACP method dispatch registry.
//!
//! 集中注册所有返回纯数据 / DTO 的 ACP 方法 handler。
//! 深文件（prompt / execute_command / session_replay）通过 wrapper 接入。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol_schema::v1::{SessionInfo, InitializeResponse, AvailableCommand};
use peri_agent::{messages::BaseMessage, thread::ThreadStore};
use peri_middlewares::{prelude::PermissionMode, skills::SkillMetadata};

use crate::provider::{LlmProvider, PeriConfig};
use crate::session::state_builders::build_config_options;
use crate::transport::types::AcpError;

/// 只读 dispatch 上下文，每次调用注入。handler 禁止从其他渠道取状态。
pub struct AcpDispatchCtx<'a> {
    pub thread_store: &'a dyn ThreadStore,
    pub peri_config: &'a PeriConfig,
    pub provider: &'a LlmProvider,
    pub permission_mode: PermissionMode,
    pub cwd: &'a str,
}

/// 已注册的 ACP 方法入口。
///
/// registry 不直接持有 async fn 指针（签名各异无法统一），而是按方法名
/// 分桶到具体 handler trait object。每个 handler 自己定义 Params/Result。
pub struct AcpMethodRegistry {
    initializers: HashMap<&'static str, Arc<dyn InitializerHandler>>,
    session_queries: HashMap<&'static str, Arc<dyn SessionQueryHandler>>,
    config_builders: HashMap<&'static str, Arc<dyn ConfigBuilderHandler>>,
}

impl AcpMethodRegistry {
    /// 构建默认 registry，把当前 6 个浅壳的函数注册进来。
    pub fn default() -> Self {
        let mut r = Self::empty();
        r.register_initializer("initialize", Arc::new(InitializeHandler));
        r.register_session_query("session/list", Arc::new(ListSessionsHandler));
        r.register_session_query("session/load.messages", Arc::new(LoadMessagesHandler));
        r.register_session_query("session/fork", Arc::new(ForkSessionHandler));
        r.register_config_builder("config/options", Arc::new(ConfigOptionsHandler));
        r.register_config_builder("config/option_update", Arc::new(ConfigOptionUpdateHandler));
        r.register_command_builder("session/available_commands", Arc::new(AvailableCommandsHandler));
        r
    }

    pub fn empty() -> Self {
        Self {
            initializers: HashMap::new(),
            session_queries: HashMap::new(),
            config_builders: HashMap::new(),
        }
    }

    pub fn register_initializer(&mut self, method: &'static str, h: Arc<dyn InitializerHandler>) {
        self.initializers.insert(method, h);
    }
    // 其余 register_* 同构，略
}

// ── handler trait 群 ────────────────────────────────────────────────

pub trait InitializerHandler: Send + Sync {
    fn build(&self) -> InitializeResponse;
}

pub trait SessionQueryHandler: Send + Sync {
    async fn list(&self, ctx: &AcpDispatchCtx<'_>, cwd_filter: Option<&str>)
        -> Result<Vec<SessionInfo>, AcpError>;
}

pub trait ConfigBuilderHandler: Send + Sync {
    fn build(&self, ctx: &AcpDispatchCtx<'_>) -> Vec<agent_client_protocol_schema::v1::SessionConfigOption>;
}

// ── 具体 handler（每个文件级 handler 体积 ≤ 30 LOC）────────────────

struct InitializeHandler;
impl InitializerHandler for InitializeHandler {
    fn build(&self) -> InitializeResponse {
        // 原 dispatch/init.rs:17-29 内容平移：构造带全部 capabilities 的响应
        // （load_session=true + list/close/resume/fork 四个 SessionCapabilities）
        // ... 约 13 行 builder 链式调用（此处省略）
        unimplemented!("see dispatch/init.rs:17-29 for source body")
    }
}

struct ListSessionsHandler;
impl SessionQueryHandler for ListSessionsHandler {
    async fn list(&self, ctx: &AcpDispatchCtx<'_>, cwd_filter: Option<&str>)
        -> Result<Vec<SessionInfo>, AcpError>
    {
        // 原 dispatch/list_sessions.rs:13-35 内容平移：
        //   ctx.thread_store.list_threads() → filter by cwd → map to SessionInfo
        // ... 约 20 行（此处省略）
        unimplemented!("see dispatch/list_sessions.rs:13-35 for source body")
    }
}

// 其余 handler（ConfigOptionsHandler / ConfigOptionUpdateHandler /
// AvailableCommandsHandler / LoadMessagesHandler / ForkSessionHandler）
// 同构：把原 dispatch/<file>.rs 的单个函数体搬进 impl 块。

设计要点：

1. **不强行统一 handler trait**——不同 ACP 方法参数/返回差异巨大，强行用 `Box<dyn Fn(Value) -> Result<Value>>` 会把类型安全交给 `serde_json::Value`，反而降低 depth。按"参数形态分桶"（`InitializerHandler` / `SessionQueryHandler` / `ConfigBuilderHandler`）既保留类型化签名，又能集中注册。
2. **`AcpDispatchCtx` 显式注入**——禁止 handler 通过 `crate::PERI_CONFIG_HANDLE` 之类的全局句柄取状态，所有依赖走 `&'a` 引用。这是从 v2 §2.2 "纯函数"约束推导出的硬性约束。
3. **深文件不进 registry**——`prompt.rs` / `execute_command.rs` / `session_replay.rs` 保持现状，transport 层继续直接调 `dispatch::prompt::handle_prompt(...)`。registry 不是吞并整个 dispatch/ 子树，只吞浅壳。
4. **registry 是单点**——所有 register 调用集中在 `default()` 一处，新增方法 = 改一处。这是 leverage 真正提升的来源（原方案 9 个文件分散，新增方法要新建文件 + 改 mod.rs + 改 transport 三处）。

### 5.2 保留的 3 个深文件不变

```rust
// dispatch/prompt.rs            —— 138 LOC，保留
pub fn extract_prompt_params(params: &Value) -> Result<(String, MessageContent, Option<Value>), AcpError>;
pub async fn handle_prompt(...) -> Result<(), AcpError>;

// dispatch/execute_command.rs   —— 226 LOC，保留
#[allow(clippy::too_many_arguments)]
pub async fn execute_command(
    params: &Value,
    session_history: Vec<BaseMessage>,
    cwd: &str,
    peri_config: &Arc<PeriConfig>,
    event_sink: &Arc<dyn EventSink>,
    // ... 共 13 个参数
) -> Result<Value, AcpError>;

// dispatch/session_replay.rs    —— 216 LOC，保留
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
) -> Result<(), ReplayError>;

pub trait ReplaySender: Send + Sync { /* ... */ }
pub enum ReplayError { /* ... */ }
```

这三个文件的 deletion test 明确不过：

- `prompt.rs` 的参数提取分支有 3 条 fallback（`sessionId` vs `session_id` vs 缺失），删掉后 transport 层会复制粘贴两份（Mpsc + Stdio）。
- `execute_command.rs` 有 13 个上下文参数和 4 个错误码分支，复制粘贴灾难。
- `session_replay.rs` 是 ACP 协议 spec 的直接实现，协议变更时这里就是单一修改点。

### 5.3 re-export 文件迁移路径

| 文件 | 迁移目标 | 迁移动作 |
|------|---------|---------|
| `session/frozen.rs` | `session/executor::FrozenSessionData::build`（已是真实位置） | 删除 frozen.rs，调用点改 `peri_acp::session::executor::FrozenSessionData::build(...)` |
| `event/dto.rs` | `event/mod.rs` 内联 re-export，或调用点直接 `peri_acp_types::summary::XxxDto` | 推荐后者——`peri-acp-types` 本来就是为外部消费方设计的 crate |
| `event/mapper_v2.rs` | `event/mod.rs` 内联 re-export | 4 行 `pub use` 平移到 mod.rs，删除 mapper_v2.rs |
| `hooks/mod.rs` | crate 顶层 `lib.rs` 加 `pub use peri_middlewares::hooks::types::RegisteredHook;` 或删除整模块让调用方 `peri_middlewares::hooks::types::RegisteredHook` | 后者更干净；grep 显示外部调用极少 |
| `lsp/mod.rs` | 同上，让调用方直接 `peri_lsp::config::LspServerConfig` | 删除整模块 |
| `agent/mod.rs` | 保留 mod.rs 作为 module listing，删除 `pub use builder::*` | 调用点从 `peri_acp::agent::build_agent` 改为 `peri_acp::agent::builder::build_agent` |

---

## 6. seam 后面剩什么

### 6.1 文件数对比

| 状态 | dispatch/ 子树 | re-export 群 | 合计 |
|------|---------------|-------------|------|
| 现状 | 9 个 .rs（含 mod.rs 和 commands_test.rs） | 6 个 .rs | 15 |
| 目标 | 4 个 .rs（`registry.rs` + `prompt.rs` + `execute_command.rs` + `session_replay.rs`，`mod.rs` 保留为 module listing） | 0 个（全部内联或删除） | 5 |
| 净减 | -5 | -6 | **-10** |

### 6.2 dispatch/mod.rs 形态变化

```rust
// 目标形态
pub mod execute_command;
pub mod prompt;
pub mod registry;
pub mod session_replay;

// 便利 re-export：仅保留深文件入口（浅壳已并入 registry）
pub use prompt::{extract_prompt_params, handle_prompt};
pub use execute_command::execute_command;
pub use registry::AcpMethodRegistry;
pub use session_replay::{replay_session_history, ReplayError, ReplaySender};

#[cfg(test)]
mod commands_test; // 测试平移到 registry_test.rs
```

### 6.3 hooks/ 和 lsp/ 模块的命运

如果 `hooks/mod.rs` 和 `lsp/mod.rs` 仅做一层 re-export，删除后整个 `hooks/` / `lsp/` 目录都会消失。这是预期的——这两个领域（hook 系统集成、LSP 中间件集成）的真正实现在 `peri-middlewares` 和 `peri-lsp`，`peri-acp` 只是被动的类型消费者，没有自己的逻辑可放。

### 6.4 transport 层调用形态变化

```rust
// 现状（peri-tui/src/acp_stdio/transport.rs:8-19）
use peri_acp::dispatch;
responder.respond(dispatch::build_initialize_response());

// 目标（两种方案二选一）
// 方案 A：调用方持有一个 registry 实例
let registry = peri_acp::dispatch::AcpMethodRegistry::default();
let init = registry.initializer("initialize").unwrap().build();
responder.respond(init);

// 方案 B：保留顶层便利函数，内部转发到 registry
responder.respond(peri_acp::dispatch::registry::default_initialize_response());
```

方案 A 是"真 registry 模式"，适合未来需要插件化注册自定义 ACP 方法；方案 B 是"集中化 + 便利 API"，适合当前没有插件需求的阶段。本候选推荐 **方案 B 作为初始落地**，方案 A 留作演进选项——见 §8 风险讨论。

---

## 7. 测试面

### 7.1 现有测试迁移

| 测试文件 | LOC | 迁移动作 |
|---------|-----|---------|
| `dispatch/commands_test.rs` | 49 | 重命名为 `registry_test.rs`，3 个 `test_build_available_commands_*` 测试改为对 `AvailableCommandsHandler` 直接断言 |
| `event/dto_test.rs` | （存在） | DTO 已迁 `peri-acp-types`，测试一并平移到 `peri-acp-types` crate |
| 其余浅文件 | — | **没有测试**（薄壳单测无意义），删除文件即可 |

### 7.2 新增 registry 完整性测试

```rust
// dispatch/registry_test.rs（新增）
#[test]
fn test_registry_registers_all_default_methods() {
    let r = AcpMethodRegistry::default();
    // 每个默认方法必须可查
    assert!(r.initializer("initialize").is_some());
    assert!(r.session_query("session/list").is_some());
    assert!(r.session_query("session/load.messages").is_some());
    assert!(r.session_query("session/fork").is_some());
    assert!(r.config_builder("config/options").is_some());
    assert!(r.config_builder("config/option_update").is_some());
    assert!(r.command_builder("session/available_commands").is_some());
}

#[tokio::test]
async fn test_initializer_handler_returns_v1_capabilities() {
    let h = InitializeHandler;
    let resp = h.build();
    assert_eq!(resp.protocol_version, ProtocolVersion::V1);
    let caps = resp.agent_capabilities;
    assert!(caps.load_session.unwrap());
    assert!(caps.session_capabilities.unwrap().list.is_some());
    assert!(caps.session_capabilities.unwrap().fork.is_some());
}

#[tokio::test]
async fn test_list_sessions_handler_filters_by_cwd() {
    // mock ThreadStore + 两个不同 cwd 的 thread
    // 断言 cwd_filter = Some("/foo") 时只返回 /foo 的 session
}

#[test]
fn test_config_options_handler_delegates_to_state_builders() {
    // 断言 handler 输出 == build_config_options(直接调用)
    // 这是 delegation 不变量回归
}
```

测试标准遵循 `docs/design/testing-standards.md` P0：registry 完整性是协议契约，错误路径测试必须断言 AcpError 错误码（-32603 / -32602）而非仅 `is_err()`。

### 7.3 被淘汰的测试

- `dispatch/commands_test.rs` 中的 `test_build_available_commands_no_skills_no_leak` 仍有意义（断言 skill 前缀不泄漏），保留为 `registry_test.rs` 的一部分。
- 其余浅文件原本来就没有测试，没有淘汰动作。

### 7.4 深文件的测试不变

`prompt.rs` / `execute_command.rs` / `session_replay.rs` 的测试（如果有）保持原样。这三个文件不动，其测试也不动。

---

## 8. 风险与回滚

### 8.1 风险 1：dispatch 文件消失后调试可读性

**担心**：新人看到 ACP `initialize` 请求时找不到"initialize 处理在哪"，因为 `dispatch/init.rs` 不存在了。

**应对**：

- registry 是单点，`registry.rs::default()` 一处就能看到全部方法注册表，比当前散落 6 个文件更可发现。
- 每个 handler struct 名字明确（`InitializeHandler` / `ListSessionsHandler`），IDE 跳转从 method 字符串到 struct 实现路径短。
- 在 `registry.rs` 顶部 doc comment 中列出方法 → handler 表，作为导航地图。

**净评估**：可读性提升而非下降。

### 8.2 风险 2：re-export 删除影响外部 crate 引用

**担心**：`peri_acp::event::dto::CompactFileInfoDto` 等 re-export 被外部 crate 引用，删除后引用断裂。

**证据**：在 `peri-tui` 全树 grep，**零命中**。`peri-tui`（`peri-acp` 的唯一外部消费者）已经直接从源 crate 引用，从未走 `peri-acp` 的 re-export 路径：

```
peri-tui/src/acp_stdio/context.rs:21  use peri_middlewares::hooks::RegisteredHook;
peri-tui/src/acp_stdio/context.rs:19  use peri_lsp::config::LspServerConfig;
peri-tui/src/acp_server/mod.rs:62     pub plugin_hooks: Vec<peri_middlewares::hooks::RegisteredHook>;
peri-tui/src/acp_server/mod.rs:65     pub plugin_lsp_servers: Vec<peri_lsp::config::LspServerConfig>;
peri-tui/src/launch.rs:175            Vec<Vec<peri_middlewares::hooks::RegisteredHook>>
```

`hooks/mod.rs` 和 `lsp/mod.rs` 是 **dead re-export**——没有任何调用方经过它们。删除零风险。

**回滚预案**：若 PR 合并后发现某个未察觉的第三方 fork 依赖这些 re-export，可以加 `#[deprecated(note = "use peri_middlewares::hooks::types::RegisteredHook directly")]` 作为过渡，但当前 monorepo 内不存在此情况，无需 deprecated 阶段。

### 8.3 风险 3：方案 A vs B 的演进决策

**担心**：先做方案 B（便利函数 + 内部 registry），未来想演进到方案 A（真 handler trait 注入）时需要二次大改。

**应对**：

- 方案 B 的便利函数内部就是 `AcpMethodRegistry::default().initializer("initialize").build()`，未来改方案 A 只需把 `default()` 改成注入参数，便利函数保持兼容。
- 在 §5.1 草案中 `AcpMethodRegistry` 已经设计为可注入（`register_*` 方法 public），方案 B → 方案 A 是平滑演进。

### 8.4 风险 4：registry 集中后的"上帝对象"倾向

**担心**：所有 dispatch 函数集中到 registry，可能演化成 god module。

**应对**：

- registry 自身 LOC 上界明确——它只持有 handler 注册表 + `default()` 构造，**不持有任何业务逻辑**。业务逻辑在每个 handler struct 内，handler struct 仍可独立测试。
- 如果未来 handler 数量超过 20 个，可按 `InitializerHandler` / `SessionQueryHandler` 等 trait 拆分到 `registry/initializer.rs` / `registry/session_query.rs` 子模块。当前 7 个 handler 不足以触发拆分。
- handler struct 永远 ≤ 30 LOC（超过即说明业务逻辑下沉，应当抽到独立深文件，参考 prompt.rs 模式）。

### 8.5 回滚

整个迁移分 4 阶段（见 §9），每阶段独立可回滚：

| 阶段 | 回滚成本 | 回滚动作 |
|------|---------|---------|
| Phase 1（建 registry 不删旧文件） | 极低 | 删除 `registry.rs`，无影响 |
| Phase 2（transport 改用 registry） | 低 | git revert transport 改动，旧路径仍可用 |
| Phase 3（删 6 个浅 dispatch 文件） | 中 | git revert，需重跑 mod.rs 重新导出 |
| Phase 4（删 6 个 re-export 文件） | 低 | 独立 PR，revert 单文件 |

---

## 9. 迁移步骤

### Phase 1：建立 registry.rs，保留旧文件作为薄 forward

**目标**：新增 `dispatch/registry.rs`，不删任何旧文件，不改动 transport 层。registry 内部直接调用旧文件函数。

```rust
// dispatch/registry.rs（Phase 1 版本）
use crate::dispatch::{init, list_sessions, config_update, commands, session_load, session_fork};

pub struct AcpMethodRegistry { /* ... */ }

impl AcpMethodRegistry {
    pub fn default() -> Self {
        // handler 内部转调 init::build_initialize_response() 等
    }
}
```

**验证**：

- `cargo build -p peri-acp` 通过。
- 新增 `registry_test.rs`，断言 registry 完整性。
- 旧测试（`commands_test.rs`）全部通过。

**完成判据**：registry 落地 + 完整性测试绿灯。

### Phase 2：transport 层改用 registry

**目标**：peri-tui 的 `acp_server/` 和 `acp_stdio/` 全部改为通过 registry 或 registry 暴露的便利函数调用。

```rust
// peri-tui/src/acp_stdio/transport.rs
// 旧：responder.respond(dispatch::build_initialize_response());
// 新：
responder.respond(dispatch::registry::default_initialize_response());

// peri-tui/src/acp_stdio/commands.rs
// 旧：let cmds = peri_acp::dispatch::build_available_commands(&skills);
// 新：
let cmds = peri_acp::dispatch::registry::default_available_commands(&skills);
```

**验证**：

- `cargo build --workspace` 通过。
- 手动启动 TUI + Stdio 路径，验证 `initialize` / `session/list` / `config/update` 行为不变。
- `cargo test --workspace` 全绿。

**完成判据**：transport 层不再直接 import `dispatch::init` / `dispatch::list_sessions` 等子模块。

### Phase 3：删除 6 个浅 dispatch 文件

**目标**：物理删除 `dispatch/init.rs` / `list_sessions.rs` / `config_update.rs` / `commands.rs` / `session_load.rs` / `session_fork.rs`，handler 逻辑从 Phase 1 的 forward 改为内联到 registry.rs（或拆到 `registry/` 子目录）。

**验证**：

- `dispatch/` 目录只剩 `mod.rs` + `registry.rs` + `prompt.rs` + `execute_command.rs` + `session_replay.rs` + `commands_test.rs`（重命名为 `registry_test.rs`）。
- `cargo build --workspace` 通过。
- 所有 transport 层调用形态不变（Phase 2 已完成迁移）。

**完成判据**：dispatch/ 子树文件数从 9 降到 4。

### Phase 4：删除 6 个 re-export 文件（独立可做）

**目标**：删除 `session/frozen.rs` / `event/dto.rs` / `event/mapper_v2.rs` / `hooks/mod.rs` / `lsp/mod.rs` 中的 re-export，agent/mod.rs 删除 `pub use builder::*`。

**子任务**：

1. `session/frozen.rs`：grep 调用点（应只有 1-2 处），改为 `FrozenSessionData::build(...)`，删除文件。
2. `event/dto.rs`：调用点改为 `peri_acp_types::summary::XxxDto`，删除文件，dto_test.rs 平移到 `peri-acp-types`。
3. `event/mapper_v2.rs`：4 行 `pub use` 平移到 `event/mod.rs`，删除文件。
4. `hooks/mod.rs`：删除整个 `hooks/` 目录（已确认零外部引用）。
5. `lsp/mod.rs`：删除整个 `lsp/` 目录（已确认零外部引用）。
6. `agent/mod.rs`：保留 module listing，删除 `pub use builder::*`，调用点从 `peri_acp::agent::build_agent` 改为 `peri_acp::agent::builder::build_agent`。

**验证**：

- 每个子任务独立 commit，独立可 revert。
- `cargo build --workspace` + `cargo test --workspace` 全绿。

**完成判据**：6 个 re-export 全部消失，`peri-acp` crate 根模块数从 N 减到 N-5。

### Phase 间依赖

```
Phase 1 ──┐
          ├──► Phase 3 ──► (完成)
Phase 2 ──┘

Phase 4 ─────────────────────────► (独立可并行)
```

Phase 4 与 Phase 1-3 无依赖，可作为独立 PR 提前或延后完成。建议 **Phase 4 与候选 5（mapper 重命名）合并 PR**，因为两者都触及 `event/` 子树。

---

## 10. 推荐方向与 ADR 建议

### 10.1 方向选择

| 方向 | 评估 |
|------|------|
| **方向 A：dispatch registry** | **推荐**。集中注册表 + 显式 ctx 注入，可发现性、leverage、可测试性同时提升。从 9 文件降到 4 文件，新增 ACP 方法只需改 registry 一处。 |
| 方向 B：内联到 transport 层 | **不推荐**。会让 MpscTransport 和 StdioTransport 各自复制一份 dispatch 逻辑，违反 v2 §2.1 "传输层只做帧编解码" 约束，leverage 反而下降。 |
| **方向 C：re-export 内联** | **推荐（独立于 A）**。6 个 re-export 文件全是死代码或自认薄包装，删除是纯收益。本候选已 grep 证明 hooks/lsp re-export 零引用。 |

**最终推荐**：方向 A + 方向 C 并行推进。方向 B 不采纳。

### 10.2 ADR（Architecture Decision Record）建议

**需要 ADR**。理由：

1. 方向 A 引入新的 architectural primitive（`AcpMethodRegistry` + handler trait 群），是 crate 级架构决策，未来新增 ACP 方法都要遵循这个模式。
2. 方向 C 删除整个 `hooks/` 和 `lsp/` 子目录，影响 `peri-acp` 的 module 拓扑，需要记录"为什么 peri-acp 不再有自己的 hook/lsp 入口"的决策依据。
3. registry 的"按参数形态分桶 handler trait"（而非统一 `Box<dyn Fn(Value)>`）是显式的类型安全取舍，应在 ADR 中说明 trade-off。

**ADR 草案标题**：`ADR-peri-acp-04: dispatch registry consolidation and re-export inlining`

**ADR 内容建议结构**：

- Context：当前 12 个浅文件的现状诊断
- Decision：方向 A + 方向 C
- Alternatives：方向 B（被否决）、维持现状（被否决）
- Consequences：dispatch/ 子树从 9 → 4 文件；hooks/ 和 lsp/ 目录消失；新增 ACP 方法的标准流程改为 register_* 一处
- Compliance：registry_test.rs 完整性测试作为不变量守护

---

## 附录 A：deletion test 详细记录

对 6 个浅 dispatch 文件逐个跑 deletion test（"删掉并把同名函数搬进调用方，调用方复杂度是否同等？"）：

| 文件 | 删除后搬迁目的地 | 调用方复杂度变化 | deletion test 结论 |
|------|-----------------|-----------------|-------------------|
| `dispatch/init.rs` | `transport.rs::handle_initialize`（唯一调用点） | 1 行调用 → 13 行 caps 构造，transport 层从"帧处理"侵入"capabilities 声明"，破坏 v2 §2.1 seam | 不通过——但方向 A 不是搬到调用方而是集中到 registry，registry 内部 `InitializeHandler` 仍持 13 行逻辑，seam 不破坏 |
| `dispatch/list_sessions.rs` | `acp_server/requests.rs` + `acp_stdio/session/control.rs` 两处 | 35 行 × 2 份复制，cwd_filter 改动需双改 | 不通过——真实跨 transport 复用，registry 化作为 `ListSessionsHandler::list` 集中 |
| `dispatch/config_update.rs` | 调用方直接调 `state_builders::build_config_options` | 长度不变，`make_config_options` 本身就是 forward | **通过**——纯 forward 无价值。registry 化时仍保留 handler 以提供协议语义命名 |
| `dispatch/commands.rs` | `acp_server/notify.rs` + `acp_stdio/commands.rs` 两处 | 34 行 × 2 份复制，13 条内置命令字符串重复 | 不通过——跨 transport 复用 |
| `dispatch/session_load.rs` | transport 层 session/load 处理函数 | error 吞咽逻辑（`warn!` + `Vec::new()`）分散，统一改策略时漏改 | 不通过——错误处理策略应集中 |
| `dispatch/session_fork.rs` | transport 层 session/fork 处理函数 | 42 行 × 2 份复制 | 不通过——跨 transport 复用 |

**综合**：

- 5 个文件（init / list_sessions / commands / session_load / session_fork）有真实跨 transport 复用或跨层 seam，不能简单删除搬调用方——**但当前分散在 6 个文件是过度切分**，集中到 registry.rs 是更好的 leverage。
- 1 个文件（config_update）是纯 forward，deletion test 通过，可删但保留在 registry 提供协议语义命名。
- 6 个 re-export 全部 deletion test 通过，零外部引用证据已 grep 确认（§8.2）。

---

## 附录 B：handler LOC 上界守护

为防止 registry 演化为 god module，建议在 `dispatch/registry.rs` 顶部加注释守护：

```rust
//! # Handler LOC 上界
//!
//! 每个 handler struct 的 impl 块必须 ≤ 30 LOC。超过即说明业务逻辑下沉，
//! 应当抽到独立深文件（参考 prompt.rs / execute_command.rs / session_replay.rs 模式）。
//! 此约束由 review 时人工检查，不强制 CI（LOC 限制易绕过且收益低）。
```

Review 检查清单：

- [ ] 新增 handler struct 的 impl 块 LOC
- [ ] 新增 handler 是否有 ≥ 3 个分支的 match / 多层 serde 兜底
- [ ] 新增 handler 是否引入跨 transport 差异 flag

任一不通过则要求抽深文件，不直接并入 registry。

---

## 附录 C：与候选 5（mapper 重命名）的协同

候选 5 涉及 `event/mapper.rs` 和 `event/mapper_v2.rs` 的命名冲突解决。本候选 Phase 4 删除 `event/mapper_v2.rs`（4 行 re-export 平移到 `event/mod.rs`），直接消除命名冲突的一半来源，使候选 5 的工作量从"重命名两个文件"降为"重命名一个文件"。建议 PR 链：本候选 Phase 1-3 → 本候选 Phase 4 + 候选 5（合并 PR）。