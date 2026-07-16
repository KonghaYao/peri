# P1-1：StageContext 22 字段 god object 拆分

**状态**：Open
**优先级**：高
**类型**：架构改进
**创建日期**：2026-07-16
**来源**：`spec/issues/2026-07-16-architecture-upgrade-checklist.md` P1-1

## Problem Statement

`StageContext`（`peri-agent/src/agent/stages/mod.rs:71`）有 22 个字段，是 ReAct 循环的唯一运行时上下文。所有 5 个阶段（Compact/Receive/Reason/Act/End）通过 `StageContext` 共享全部状态。

当前结构缺乏职责边界，导致：
- 新增字段时难以判断归属（加到 builder 哪个 `with_*` 方法？）
- 阶段函数隐式依赖不清晰的字段子集
- 无子结构分组，阅读代码时无法快速定位某字段的使用者

> **设计验证结论**（2026-07-16 双 agent 对抗审查）：
> StageContext 的 clone 成本不是真正问题——22 字段中 18 个是 `Arc<T>` 或 `Option<Arc<T>>`，clone 仅增加引用计数（原子操作级别），每轮 LLM 调用动辄 500ms+，clone 开销可忽略。拆分的核心动机是**职责边界清晰化**。

## 最终方案

### 子结构定义（22 字段全覆盖）

#### SessionHandle — 会话级实体（生命周期 = 整个 Agent Session）

```rust
pub struct SessionHandle {
    pub turn: Arc<TurnContext>,
    pub transcript: Arc<RwLock<MessageTranscript>>,
    pub queue: MessageQueue,
    pub agent_id: AgentId,
    /// metrics/tracing 用键值对（AgentContext 在 from_stage 时克隆）
    pub session_context: Arc<RwLock<HashMap<String, String>>>,
}
```

| 字段 | 使用者 |
|------|--------|
| `turn` | 所有阶段（cancel 检查、step 推进、turn_id） |
| `transcript` | 所有阶段（读写消息、visible_snapshot） |
| `queue` | Receive、End、run_react_loop（drain 消息） |
| `agent_id` | 所有阶段（EventBus emit 路由） |
| `session_context` | AgentContext（from_stage 克隆）、tool_dispatch（metrics） |

#### RuntimeServices — LLM 调用 + 工具执行运行时服务

```rust
pub struct RuntimeServices {
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// LLM 可见 + 可执行的工具（Reason 读列表传 LLM，tool_dispatch 按名执行）
    pub tools: SharedToolMap,
    pub middleware_chain: Arc<MiddlewareChain>,
    pub event_bus: Arc<EventBus>,
    /// Deferred tools 外部注册表（ExecuteExtraTool 代理执行用；
    /// 主 agent 路径与 tools 指向同一 Arc，SubAgent 路径可能不同）
    pub shared_tools: Option<SharedToolMap>,
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
}
```

| 字段 | 使用者 |
|------|--------|
| `llm` | Reason（generate_reasoning） |
| `tools` | Reason（读列表传 LLM）、tool_dispatch（按名查找执行） |
| `middleware_chain` | 所有阶段（via middleware_runner 桥接） |
| `event_bus` | 所有阶段（emit render/state/observe 事件） |
| `shared_tools` | tool_dispatch（ExecuteExtraTool 代理） |
| `error_suggest_registry` | tool_dispatch（错误后注入建议） |
| `tool_registry_snapshot` | tool_dispatch（错误建议查询） |

#### CompactContext — Compact 系统上下文（含跨阶段计数器）

```rust
pub struct CompactContext {
    pub context_budget: Option<ContextBudget>,
    pub compact_config: Option<CompactConfig>,
    pub compact_llm: Option<Arc<dyn BaseModel>>,
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
    /// 会话级 Token 追踪器（Compact 写 reset/estimated_tokens，Act 读用于 StateSnapshot）
    pub token_tracker: Arc<RwLock<TokenTracker>>,
    /// 连续失败计数（tool_dispatch 递增/重置，Compact 读用于降级跳过，Act 读用于 StateSnapshot）
    pub consecutive_failures: Arc<AtomicU32>,
}
```

| 字段 | 使用者 |
|------|--------|
| `context_budget` | Compact（判断阈值）、Act（读用于 StateSnapshot） |
| `compact_config` | Compact（阈值配置 + auto_compact_enabled） |
| `compact_llm` | Compact（Full Compact LLM 摘要） |
| `compact_pre_hook` | Compact（插件回调） |
| `compact_post_hook` | Compact（插件回调） |
| `token_tracker` | Compact（读+写）、Act（只读） |
| `consecutive_failures` | Compact（读+写）、Act（只读）、tool_dispatch（写） |

> **命名说明**：`token_tracker` 和 `consecutive_failures` 虽被 Act 和 tool_dispatch 读写，但它们的**语义归属**是 compact 系统的副作用追踪。属于紧凑上下文中的 "跨阶段共享的计数器"。

#### AsyncContext — 异步传输控制（仅 run_react_loop idle 路径）

```rust
pub struct AsyncContext {
    pub idle_inbox: Option<Arc<SessionInbox>>,
    pub idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
```

| 字段 | 使用者 |
|------|--------|
| `idle_inbox` | run_react_loop（idle 时 await_wake） |
| `idle_should_wait` | run_react_loop（判断是否应等待异步任务） |

#### StageContext — 聚合根

```rust
pub struct StageContext {
    pub session: SessionHandle,       // 5 字段
    pub runtime: RuntimeServices,     // 7 字段
    pub compact: CompactContext,      // 7 字段
    pub async_ctx: AsyncContext,      // 2 字段
    /// 中间件 recall 拦截缓冲区（middleware_runner 写，executor 读）
    pub recall_buffer: Arc<RwLock<Vec<String>>>,
}
```

**22 → 5 聚合字段 + 1 残量字段**。`system_prompt` 死字段（v2_bridge 确认零代码读取）在本次拆分中移除。

### 阶段访问映射

| 阶段 | 需要哪些子结构 |
|------|----------------|
| Compact | `session`（turn/transcript/agent_id）+ `runtime`（event_bus/middleware_chain）+ `compact`（全部 7 字段） |
| Receive | `session`（queue/transcript/agent_id）+ `runtime.event_bus` |
| Reason | `session`（turn/transcript/agent_id）+ `runtime`（llm/tools/event_bus/middleware_chain） |
| Act | `session`（turn/transcript/agent_id）+ `runtime`（event_bus/middleware_chain/tools）+ `compact`（只读 budget/tracker/failures） |
| End | `session`（queue/transcript/turn/agent_id） |
| tool_dispatch | `session`（transcript/agent_id）+ `runtime`（tools/shared_tools/error_suggest）+ `compact`（consecutive_failures） |

### 设计决策（由双 agent 对抗审查驱动）

| 决策 | 原因 |
|------|------|
| **StageInput 类型不变**（仍为 `pub context: StageContext`） | 测试 fixture 零改动（9 个 `make_context()` + 30+ 用例）。阶段函数内部解构访问 |
| **middleware_runner 保持接收 `&StageContext`** | 10 个 hook 函数是 v1→v2 桥接层，需要几乎所有子结构。与聚合根耦合是合理的架构边界 |
| **Builder API 不变** | `with_xxx()` 方法保留，由 builder 内部路由到正确子结构。builder_v2.rs 和 v2_bridge.rs 调用方零改动 |
| **不消除 `context.clone()`** | 全部 Arc 浅拷贝，每轮 5 次 clone < 5μs。后续可按需优化为子结构引用传递 |
| **移除 `system_prompt` 死字段** | v2_bridge.rs:148 明确注释 "stage 内零代码读取"。同步清理 builder_v2.rs:252-253 和 StageContextBuilder::with_system_prompt |
| **`recall_buffer` 留在聚合根顶层** | 中间件 runner 跨所有阶段 drain，不属于任何子域。跟着 `middleware_chain` 走，但 `middleware_chain` 在 RuntimeServices 中而 recall 语义上是独立的数据通道 |

## 实施计划（4 Step，每步可独立编译通过）

### Step 1：定义子结构 + 聚合根

**目标**：添加子结构定义，StageContext 同时持有新旧两套字段（指向相同 Arc）。移除 `system_prompt`。

**改动文件**：
- `peri-agent/src/agent/stages/mod.rs` — 新增 4 子结构定义、重组 StageContext 结构体、修改 Builder 内部路由
- `peri-agent/src/agent/agent_context.rs` — `from_stage()` 改为访问 `ctx.session.session_context` 等子字段
- `peri-acp/src/agent/builder_v2.rs` — 移除 `system_prompt` 赋值行（行 252-253）
- `peri-agent/src/agent/stages/mod.rs` — Builder 移除 `with_system_prompt()` 方法

**验证**：`cargo build -p peri-agent --lib && cargo build -p peri-acp --lib`

### Step 2：阶段文件内部路径更新

**目标**：所有阶段函数 + tool_dispatch + middleware_runner 中 `ctx.xxx` → `ctx.{sub}.xxx`。

**改动文件**（搜索替换为主）：

| 文件 | 旧访问 → 新访问 |
|------|-----------------|
| `peri-agent/src/agent/stages/compact.rs` | `ctx.turn`→`ctx.session.turn`, `ctx.context_budget`→`ctx.compact.context_budget`, `ctx.compact_llm`→`ctx.compact.compact_llm`, `ctx.token_tracker`→`ctx.compact.token_tracker`, `ctx.consecutive_failures`→`ctx.compact.consecutive_failures`, `ctx.event_bus`→`ctx.runtime.event_bus`, `ctx.transcript`→`ctx.session.transcript`, `ctx.agent_id`→`ctx.session.agent_id`, `ctx.middleware_chain`→`ctx.runtime.middleware_chain` |
| `peri-agent/src/agent/stages/receive.rs` | `ctx.queue`→`ctx.session.queue`, `ctx.transcript`→`ctx.session.transcript`, `ctx.event_bus`→`ctx.runtime.event_bus` |
| `peri-agent/src/agent/stages/reason.rs` | `ctx.llm`→`ctx.runtime.llm`, `ctx.tools`→`ctx.runtime.tools`, `ctx.transcript`→`ctx.session.transcript`, `ctx.event_bus`→`ctx.runtime.event_bus` |
| `peri-agent/src/agent/stages/act.rs` | `ctx.context_budget`→`ctx.compact.context_budget`, `ctx.token_tracker`→`ctx.compact.token_tracker`, `ctx.consecutive_failures`→`ctx.compact.consecutive_failures`, `ctx.event_bus`→`ctx.runtime.event_bus`, `ctx.transcript`→`ctx.session.transcript`, `ctx.middleware_chain`→`ctx.runtime.middleware_chain` |
| `peri-agent/src/agent/stages/end.rs` | `ctx.queue`→`ctx.session.queue`, `ctx.transcript`→`ctx.session.transcript` |
| `peri-agent/src/agent/stages/tool_dispatch.rs` | `ctx.tools`→`ctx.runtime.tools`, `ctx.shared_tools`→`ctx.runtime.shared_tools`, `ctx.error_suggest_registry`→`ctx.runtime.error_suggest_registry`, `ctx.tool_registry_snapshot`→`ctx.runtime.tool_registry_snapshot`, `ctx.consecutive_failures`→`ctx.compact.consecutive_failures`, `ctx.event_bus`→`ctx.runtime.event_bus`, `ctx.transcript`→`ctx.session.transcript`, `ctx.middleware_chain`→`ctx.runtime.middleware_chain` |
| `peri-agent/src/agent/stages/middleware_runner.rs` | `ctx.middleware_chain`→`ctx.runtime.middleware_chain`（共 10 处） |

**注意**：`middleware_runner.rs` 的函数签名保持 `fn(ctx: &StageContext)`，仅内部访问路径更新。

**验证**：`cargo build -p peri-agent --lib`

### Step 3：构造代码 + executor 更新

**目标**：builder_v2.rs、v2_bridge.rs、executor_helpers.rs 的构造和访问代码适配。

**改动文件**：
- `peri-acp/src/agent/builder_v2.rs` — Builder API 不变，自动路由到子结构。确认 `recall_buffer` Arc clone 路径
- `peri-middlewares/src/subagent/v2_bridge.rs` — 同上，Builder API 不变
- `peri-acp/src/session/executor_helpers.rs` — `recall_buffer` 访问路径 `ctx.recall_buffer`→`ctx.recall_buffer`（不变，仍在顶层）。确认 clone 模式
- `peri-agent/src/agent/stages/mod.rs` — `run_react_loop` 中 `ctx.idle_inbox`→`ctx.async_ctx.idle_inbox` 等

**验证**：`cargo build -p peri-agent --lib && cargo build -p peri-acp --lib && cargo build -p peri-middlewares --lib`

### Step 4：删除旧字段

**目标**：StageContext 上只保留 5 个聚合子结构 + `recall_buffer`。所有旧平铺字段移除。

**改动文件**：
- `peri-agent/src/agent/stages/mod.rs` — 从 StageContext 结构体删除全部旧字段；删除 `StageContextInner` helper；简化 `new()` 和 `build()`
- `peri-agent/src/agent/stages/mod.rs` — 更新 `turn_id()` / `cwd()` / `visible_messages()` 便捷方法

**验证**：`cargo build --workspace --lib && cargo test -p peri-agent --lib`

## 改动面估算

| 类别 | 文件数 | 行数 |
|------|:---:|:---:|
| StageContext 定义 + Builder 重组 | 1 | ~150 |
| AgentContext 桥接 | 1 | ~15 |
| 阶段文件（5）+ tool_dispatch + middleware_runner | 7 | ~60 |
| 构造代码（builder_v2 + v2_bridge + executor_helpers） | 3 | ~30 |
| run_react_loop | 1 | ~20 |
| 旧字段清理 | 1 | ~30 |
| **合计** | **13** | **~305** |

## 不在此次范围

- 不修改 StageInput 类型签名（保持 `context: StageContext`）
- 不消除 `context.clone()`（后续 P2 按需优化）
- 不修改 middleware_runner 函数签名
- 不修改 SubAgent 调用路径（execute_fork/execute_bg/define/spawner 通过 v2_bridge 封装，零改动）

## 相关文件（全量 22 文件引用）

| 文件 | 角色 | 改动 |
|------|------|:---:|
| `peri-agent/src/agent/stages/mod.rs` | StageContext 定义 + Builder + run_react_loop | ✓ 核心 |
| `peri-agent/src/agent/stages/compact.rs` | Compact 阶段 | ✓ 路径 |
| `peri-agent/src/agent/stages/reason.rs` | Reason 阶段 | ✓ 路径 |
| `peri-agent/src/agent/stages/act.rs` | Act 阶段 | ✓ 路径 |
| `peri-agent/src/agent/stages/receive.rs` | Receive 阶段 | ✓ 路径 |
| `peri-agent/src/agent/stages/end.rs` | End 阶段 | ✓ 路径 |
| `peri-agent/src/agent/stages/tool_dispatch.rs` | 工具分发 | ✓ 路径 |
| `peri-agent/src/agent/stages/middleware_runner.rs` | 中间件桥接（10 函数） | ✓ 路径 |
| `peri-agent/src/agent/agent_context.rs` | MiddlewareState 薄封装 | ✓ 路径 |
| `peri-acp/src/agent/builder_v2.rs` | 主 agent 构造 | ✓ 构造 |
| `peri-acp/src/session/executor_helpers.rs` | recall_buffer drain | ✓ 路径 |
| `peri-middlewares/src/subagent/v2_bridge.rs` | SubAgent 构造 | ✓ 构造 |
| `peri-agent/src/agent/events_v2.rs` | 文档注释引用 | 注释 |
| `peri-agent/src/lib.rs` | 文档注释 | 注释 |
| `peri-agent/README.md` | 使用示例 | 文档 |
| `peri-middlewares/src/subagent/tool/execute_fork.rs` | 调用 v2_bridge | ✗ 封装 |
| `peri-middlewares/src/subagent/tool/execute_bg.rs` | 调用 v2_bridge | ✗ 封装 |
| `peri-middlewares/src/subagent/tool/define.rs` | 调用 v2_bridge | ✗ 封装 |
| `peri-middlewares/src/subagent/spawner.rs` | 调用 v2_bridge | ✗ 封装 |
| `peri-acp/src/agent/workflow_agent.rs` | 调用 v2_bridge | ✗ 封装 |
| `peri-acp/src/agent/builder.rs` | `build_agent` 提取 AgentComponents | ✗ 不涉及 |
| `peri-acp/src/session/executor.rs` | execute_prompt 入口 | ✗ 不涉及 |

## 验收标准

- [ ] `cargo build --workspace --lib` 通过
- [ ] `cargo test -p peri-agent --lib` 通过（约 80+ 测试用例）
- [ ] StageContext 只有 6 个字段（5 子结构 + recall_buffer）
- [ ] `system_prompt` 字段和 `with_system_prompt()` builder 方法已移除
- [ ] 无 `pub` 字段冗余（子结构和旧字段不并存）
- [ ] run_react_loop 可正常运行（手工人肉验证）
