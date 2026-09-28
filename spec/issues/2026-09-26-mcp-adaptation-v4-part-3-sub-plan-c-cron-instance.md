# sub-plan C：Cron builtin MCP 实例

> 本文件属于 `2026-09-26-mcp-adaptation-v4-part-3-plan.md`，只覆盖 cron；lsp 由 sub-plan L 负责。主计划 §0、§3、§4、§5、§8、§9 是唯一裁决源；本文只补充 C-01…C-04 的施工细节，并已按 **A32**（tick 归属与关闭路径）重写关闭面。
>
> 本文件引用的路径一律为 repository-relative 完整路径（F11）。

## 0. 覆盖范围

| 裁决 | 冻结接口 | 任务 | 文件 owner（本文只约束这些） |
| --- | --- | --- | --- |
| A1、A4、A5、A20、A26 | IF-P3-01、IF-P3-02 | C-01 | `peri-acp-types/src/builtin_mcp.rs`、`peri-acp-types/src/builtin_mcp_test.rs` |
| A1、A2、A3、A20、A24、A25、A26、**A32** | IF-P3-05 | C-02、C-03 | `peri-middlewares/src/mcp/builtin/cron.rs`（新增）、`peri-middlewares/src/mcp/builtin/cron_test.rs`（新增）；`peri-middlewares/src/mcp/builtin/runtime.rs` 由 H-02 写入 |
| A3、A20、A24 | IF-P3-06 | C-02、C-03 | `peri-acp-types/src/cron.rs`（契约不改）；宿主消费点由 H-04 接线 |
| A1、A3、A4、A5、A20、A21（本实例无 LSP 部分）、A24、A25、A26、**A32** | IF-P3-01/02/05/06 | C-04 | `peri-middlewares/src/cron/mod.rs`、`peri-middlewares/src/cron/middleware.rs`（删除）、`peri-middlewares/src/cron/tools.rs`（复用，不改业务行为） |

### 0.1 施工边界

- `cron` 实例是唯一 MCP 工具适配器；`CronMiddleware` 不再提供工具。`CronScheduler`、`CronSchedulerPortHandle`、`CronSchedulerPort::subscribe` 与既有审批链继续保留。
- 业务实现只保留一份：`peri-middlewares/src/cron/tools.rs` 的三个 `BaseTool` 仍是注册、列表、删除的执行实现；`CronMcpServer` 只做 MCP 映射，**不含 tick**（A32）。
- effective name 由 `mcp::builtin::effective_tool_name()` 计算；声明表只保存冻结字面量，不得复制 sanitize / `mcp__` 拼接规则。
- 关闭语义必须**四类分列**（F8）：MCP 配置 `disabled: true`、MetaHarness `policy_key=false`、`PERI_MCP_BUILTIN=off`、物理 close 不是同一件事，见 §5。
- tick 验收范围只到 ACP 侧驱动 MCP 所用共享 scheduler 的那个 task；TUI 私有 scheduler 与自建 tick 属 A15 既有缺陷，不在本波（F10），见 §3.6。

## 1. C-01：声明表与 builtin 配置

### 1.1 `cron` 条目的逐字段冻结值

在 `peri-acp-types/src/builtin_mcp.rs` 增加 `CRON_TOOLS`，并在 `BUILTIN_MCP_INSTANCES` 追加一条：

```rust
BuiltinMcpInstance { name: "cron", instance: "cron", policy_key: "CronMiddleware", tools: CRON_TOOLS }
```

`CRON_TOOLS` 顺序即 `tools/list` 声明顺序，逐项为：

| 字段 | `cron_register` | `cron_list` | `cron_remove` |
| --- | --- | --- | --- |
| `original_name` | `"cron_register"` | `"cron_list"` | `"cron_remove"` |
| `effective_name` | `"mcp__cron__cron_register"` | `"mcp__cron__cron_list"` | `"mcp__cron__cron_remove"` |
| `direct` | `false` | `false` | `false` |
| `prompt_declaration` | `None` | `None` | `None` |

builtin overlay / transport 的生效值（现有 `McpServerConfig` 字段，不是新结构体）：

| 字段/形态 | 生效值 |
| --- | --- |
| 配置 key / server name | `"cron"` |
| `source` | `Some(ConfigSource::Builtin { instance: "cron".to_string() })`（`#[serde(skip)]`，不进 wire） |
| `TransportConfig` | `TransportConfig::Builtin { instance: "cron".to_string() }` |
| `protocol_version` | `None` |
| `system_mcp` | `Some(true)` |
| `system_mcp_tools` | `Some(vec![])`，不是三个原始工具名 |
| `disabled` | 默认注入为 `None`；用户片段 `disabled: true` 走 §5 第 1 类，不改写为物理销毁 |
| `subscriptions` / `url` / `command` | 默认条目不设置；用户以 `command`/`url` 接管保留名 `cron` ⇒ 加载期 typed error |

`system_mcp_tools == []` 保留 1R readiness 约束但不做 direct 提升；三工具进 deferred MCP bridge。`prompt_declaration: None` ⇒ 声明段零变化。

### 1.2 effective name 与测试挂载

- 计算侧只调用 `mcp::builtin::effective_tool_name("cron", ...)`；声明表测试逐字比较上述三个字面量。
- 挂载点已存在：`peri-acp-types/src/builtin_mcp.rs:137-139` 的 `#[path = "builtin_mcp_test.rs"] mod tests`；本波不新增第二个 test module。
- **既有测试必须按实例收口**（当前是 wave-1 假设，直接跑会红）：
  - `peri-acp-types/src/builtin_mcp_test.rs:83-95`（`tools_are_non_empty_and_unique_per_instance`）：`prompt_declaration.is_some()` 与 `{{name}}` 断言只对 web/artifact；cron 明确断言三个 `None`。
  - 同文件 `:175-181`（`policy_keys_are_unique_and_frozen`）：期望集合加 `CronMiddleware`。
  - 同文件 `:193-196`（`find_hits_only_implemented_instances`）：`cron` 改为命中且 `tools.len() == 3`；`workspace` 仍未实现。
  - 同文件 `:203-216`（`original_tool_name_of_effective_hits_frozen_literals`）：增加三个 cron effective name。
  - 同文件 `:239-250`（`wave1_tools_are_all_declared_direct`）：只对 web/artifact 断言 `direct == true`。
- 计算侧既有测试同样收口（`peri-middlewares/src/mcp/builtin/builtin_test.rs`）：
  - `:293-306`（`injection_policy_all_and_none_shapes`）：`!all.enables("cron")` 与 `vec!["web","artifact"]` 改为含 `cron`。
  - `:529-542`（`overlay_does_not_inject_reserved_unimplemented_instances`）：`cron` 从「不得注入」列表移出（只留 `lsp` / `workspace`），并断言 `cron` 注入后 `source == Some(ConfigSource::Builtin { .. })`。
  - `:217-247`（`closed_instances_maps_policy_keys_only`）：新增 `CronMiddleware` → `{"cron"}` 断言。
- 两表不变式不在本任务写第二份策略键；由 `peri-acp-types/src/meta_harness.rs` 的既有测试覆盖（`BUILTIN_INSTANCE_POLICY_KEYS` 由 S-02 迁键）。

## 2. C-02：`CronMcpServer`（工具面，不含 tick）

新增 `peri-middlewares/src/mcp/builtin/cron.rs`，由 `peri-middlewares/src/mcp/builtin/mod.rs` 挂载 `pub(crate) mod cron;`；测试文件 `peri-middlewares/src/mcp/builtin/cron_test.rs` 以 `#[cfg(test)] #[path = "cron_test.rs"] mod tests;` 挂载（缺挂载 = 假绿）。

```rust
pub(crate) struct CronMcpServer {                    // A32：handler 只持工具面
    tools: Vec<Arc<dyn BaseTool>>,
}
impl CronMcpServer {
    pub(crate) fn new(scheduler: Arc<parking_lot::Mutex<CronScheduler>>) -> Self;
}
```

- 三个工具顺序固定：`CronRegisterTool` → `CronListTool` → `CronRemoveTool`，均 `Arc::clone` 同一 `scheduler`。
- `list_tools` 只调用 wave 1 既有 `list_tools_of(&self.tools)`；`call_tool` 只调用既有 `invoke_tool_call(&self.tools, "", &request)`（`peri-middlewares/src/mcp/builtin/web.rs:60-116`）。**不新增**第二份 name lookup / 参数默认值 / 成功失败映射。
- `server_info()` 沿用 `web.rs:49-52` 的 `server_info("peri-cron-mcp")`（只声明 tools）；不得覆写 `discover`，不得增加 resources / logging / subscription。
- `get_info` 的 `Implementation::name` 用实例私有常量（建议 `CRON_SERVER_NAME: &str = "peri-cron-mcp"`），不改 registry 的实例名 `"cron"`。

| MCP 工具名 | 既有执行实现 | schema 来源 | 结果映射 |
| --- | --- | --- | --- |
| `cron_register` | `peri-middlewares/src/cron/tools.rs::CronRegisterTool::invoke` → `CronScheduler::register` | `CronRegisterTool::parameters()`（`required: ["expression","prompt"]`） | 成功 → `CallToolResult::success`；`Err` → IF-D14 固定脱敏 `CallToolResult::error` |
| `cron_list` | `peri-middlewares/src/cron/tools.rs::CronListTool::invoke` → `CronScheduler::list_tasks` | `CronListTool::parameters()`（空 object） | 同上 |
| `cron_remove` | `peri-middlewares/src/cron/tools.rs::CronRemoveTool::invoke` → `CronScheduler::remove` | `CronRemoveTool::parameters()`（`required: ["id"]`） | 同上 |

未知工具名仍是 `McpError::invalid_params("unknown tool: …")`；业务错误不把 `CronError` 原文、prompt、路径或 env 送入 MCP 文本。dispatch enum 变体与工厂接线归 H-01/H-05，本文只提供 `CronMcpServer`。

## 3. C-02 / C-03：tick 归属、关闭路径与可行性（A32）

### 3.1 归属（A32 第 1 条）

tick **不放在 handler 里**。每代 builtin transport 生成期由 pool 方法 `McpClientPool::spawn_builtin_transport` spawn tick，并由该代的**监督者**持有 guard：

```rust
// peri-middlewares/src/mcp/builtin/runtime.rs（owner H-02）
pub(crate) struct TickGuard { cancel: tokio_util::sync::CancellationToken, join: tokio::task::JoinHandle<()> }
impl TickGuard {
    pub(crate) fn spawn(scheduler: Arc<parking_lot::Mutex<CronScheduler>>) -> Self; // 1s interval，逐位搬运 peri-acp/src/host/assemble.rs:305-321
    pub(crate) fn is_finished(&self) -> bool;                                        // A32 断言① 的可观察量
    pub(crate) async fn shutdown(self, timeout: std::time::Duration) -> BuiltinTickExit;
}

#[derive(Debug)]
pub(crate) enum BuiltinTickExit { Joined, JoinFailed(String), AbortedAfterTimeout }

pub(crate) struct BuiltinInstanceSupervisor {   // 该代的监督者
    server_task: BuiltinServerTask,
    tick: Option<TickGuard>,
}
impl BuiltinInstanceSupervisor {
    pub(crate) fn new(server_task: BuiltinServerTask, tick: Option<TickGuard>) -> Self;
    pub(crate) async fn close(self, timeout: std::time::Duration) -> BuiltinCloseOutcome; // tick 先 cancel+join，再 converge server_task
    pub(crate) fn tick_is_finished(&self) -> Option<bool>;
}

#[derive(Debug)]
pub(crate) struct BuiltinCloseOutcome { pub(crate) tick: Option<BuiltinTickExit>, pub(crate) server: BuiltinServerExit }
```

`BuiltinTransport` 增加一个字段并给出拆分入口（`peri-middlewares/src/mcp/builtin/runtime.rs:104-111`）：

```rust
pub(crate) type BuiltinClientIo = (tokio::io::ReadHalf<DuplexStream>, tokio::io::WriteHalf<DuplexStream>);
pub(crate) struct BuiltinTransport { pub(crate) io: BuiltinClientIo, pub(crate) server_task: BuiltinServerTask, pub(crate) tick: Option<TickGuard> }
impl BuiltinTransport { pub(crate) fn into_parts(self) -> (BuiltinClientIo, BuiltinInstanceSupervisor); }
```

- `spawn_builtin_link`（`runtime.rs:132-163`）沿用两参签名、`tick` 初始为 `None`；只有 pool 方法赋值。因此 `spawn_builtin_transport_with_handler(instance, handler)` 的 6 处测试调用点（`peri-middlewares/src/mcp/builtin/runtime_test.rs:136,319,609,701,702` 及 `:374` 一带）**不需要**改签名。
- tick 的 spawn 点（唯一）：

```rust
// peri-middlewares/src/mcp/client.rs（owner H-02）；IF-P3-04 已把构造入口改为 pool 方法
let tick = match (instance, ctx.cron.as_ref()) {
    (CRON_INSTANCE, Some(input)) if input.tick_enabled =>
        Some(TickGuard::spawn(Arc::clone(&input.scheduler))),
    _ => None,
};
```

`CRON_INSTANCE` 取 `peri-middlewares/src/mcp/builtin/cron.rs` 的 `pub(crate) const CRON_INSTANCE: &str = "cron"`，与注册表条目名比较；**不得**按 `mcp__cron__` 前缀或字符串前缀硬编码判定。
- `TickGuard::Drop` **只 `cancel()`**：不 abort、不阻塞、不承担正常收敛（A32 第 2 条兜底）。

### 3.2 关闭契约与调用点（A32 第 2 条）

`TickGuard::shutdown(self, timeout)`：`cancel.cancel()` → `tokio::time::timeout(timeout, self.join).await`；仅当超时才 `abort()` + await，并返回 `AbortedAfterTimeout`。**不**以「只 abort」收口；正常路径必须落在 `Joined`。超时复用既有 `BUILTIN_CONVERGE_TIMEOUT`（`peri-middlewares/src/mcp/builtin/runtime.rs:32-33`，1000ms），不新造第二套超时策略。

`BuiltinInstanceSupervisor::close` 的顺序固定为：① `tick.take()` → `shutdown(timeout).await`；② `server_task.converge(timeout).await`；③ 返回 `BuiltinCloseOutcome`。tick 先停可保证关闭过程中不再产生新触发。

收敛路径的调用点（已核实，全部位于 async 上下文，可 await）：

| # | 位置 | 动作 |
| --- | --- | --- |
| 1 | `peri-middlewares/src/mcp/client.rs:83-84` | 表值类型 `HashMap<String, BuiltinServerTask>` → `HashMap<String, BuiltinInstanceSupervisor>` |
| 2 | `peri-middlewares/src/mcp/client/lifecycle.rs:356-362` `register_builtin_task(server_name, supervisor)` | 入参改为 `BuiltinInstanceSupervisor`，返回值 `Option<BuiltinInstanceSupervisor>` |
| 3 | `peri-middlewares/src/mcp/client/lifecycle.rs:368-378` `close_builtin_task` | 改为 `supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await`，返回 `Option<BuiltinCloseOutcome>` |
| 4 | `peri-middlewares/src/mcp/client/lifecycle.rs:384-402` `close_builtin_tasks` | 逐项调用同一 `close`，仍返回「需 abort 才结束」的实例名 |
| 5 | `peri-middlewares/src/mcp/client/lifecycle.rs:404-408` `builtin_task_count` | 表值类型随 ①；新增 `#[cfg(test)] builtin_tick_is_finished(&self, server_name) -> Option<bool>` |
| 6 | `peri-middlewares/src/mcp/initialize.rs:310-317` | `let (io, supervisor) = transport.into_parts();` + `register_builtin_task(name.clone(), supervisor)`，`if let Some(previous) = … { previous.close(BUILTIN_CONVERGE_TIMEOUT).await; }` |
| 7 | `peri-middlewares/src/mcp/reconnect.rs:117-122` | 同 ⑥（`self.register_builtin_task(...)`） |
| 8 | `peri-middlewares/src/mcp/client/lifecycle.rs:130`（`remove_server`）、`:148`（`set_disabled`）、`:316`（`shutdown` 内 `close_builtin_tasks`） | 不需改代码：随 ③④ 自动获得 tick join |

- **reconnect 顺序无需重排即已满足 A32**：`peri-middlewares/src/mcp/reconnect.rs:57-63` 先关旧 service 并 `close_builtin_task`（⇒ 旧 tick cancel+join），`:105` 才 spawn 新 transport（⇒ 起新 tick）。验收只需断言，不允许把 spawn 提前到 `:63` 之前。
- **host shutdown**：`peri-acp` 侧经 `McpClientPool::shutdown()`（`peri-middlewares/src/mcp/client/lifecycle.rs:237-320`）落 ④，因此 tick join 在 host 关闭路径上自动成立；H-04 必须保证 `pool.shutdown()` 被 await（有界），不得改成 `begin_shutdown()` 后立即返回。
- **instance close**：即 `remove_server` / `set_disabled` 两条路径（表 ⑧），语义分别是「移除实例」与「配置禁用」，二者都销毁该代 transport/handler/tick。

### 3.3 可行性结论（A32 第 3 条）

**可落地，无需替代设计。** 核实结论：

1. 收敛路径本来就是 `async`：`close_builtin_task` / `close_builtin_tasks` / `shutdown` / `remove_server` / `set_disabled` 全为 `async fn`，可在其中 await tick join；不存在「签名不支持 await」的阻塞。
2. guard 可挂在池条目上：`builtin_server_tasks` 是 `parking_lot::Mutex<HashMap<String, BuiltinServerTask>>`（`peri-middlewares/src/mcp/client.rs:83-84`），与 `services` 同期登记/移除（IF-D12），本就按「代」组织；把表值换成 `BuiltinInstanceSupervisor` 即得到 A32 要求的代监督者。
3. 每次关闭都先取出条目再收敛（`lifecycle.rs:369` 的 `remove`），因此「同一 scheduler 同时最多一个有效 tick 驱动」由「表内同名唯一 + 关闭必须先 join 旧代」共同保证。
4. **不需要**新增 `shutdown_instance(kind).await` 之类的旁路入口：关闭责任留在池的既有 close 方法内，避免出现第二条关闭路径。

需要接受的机械改动（全部已列出行号，prod 2 处 + 测试 5 处）：`register_builtin_task` 调用点 `initialize.rs:314`、`reconnect.rs:119`、`runtime_test.rs:595,611,673,707,710`；`close_builtin_task` 返回类型消费点 `runtime_test.rs:677-684`、`builtin_runtime_test.rs:2128-2135`（`matches!(exit, BuiltinServerExit::Quit(_))` → `matches!(outcome.server, …)`）。`.is_none()` 形态断言（`runtime_test.rs:687`、`runtime_test.rs:728`）不受影响。

### 3.4 `tick_enabled` 注入语义

- `tick_enabled` 来自组合根注入的 `CronInstanceInput`，不从 cron 配置推断、不由 handler 自行判断。
- `true`：生成期 spawn 一个 guard；`false`：`tick: None`，工具面照常可用。
- 保留既有 print/stdio 无 tick 的差异：`peri-acp/src/host/assemble.rs:128-130` 的 `drive_cron_tick` 是唯一来源（`peri-tui/src/launch.rs:186-188` 传 `true`；`peri-acp/src/host/stdio/mod.rs:150`、`peri-acp/src/host/workspace.rs:74`、`peri-tui/src/cli_print.rs:157` 传 `false`）。本波删的是 `HostTaskKind::CronTick`（`peri-acp/src/host/assemble.rs:308-320`）这个宿主 driver，不是 `drive_cron_tick` 字段语义。

### 3.5 可观察断言（A32 第 4 条，具名）

- ① `mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers`（新增）：
  - 前置：真 scheduler + 真 `CronMcpServer` + `TickGuard::spawn` 组代并 `register_builtin_task("cron", …)`；注册任务后 `force_next_fire_to_past`（`peri-middlewares/src/cron/mod.rs:157-166`）使下一 tick 必触发，先观测到 ≥1 次 trigger。
  - 断言：`pool.close_builtin_task("cron").await` 返回 `BuiltinCloseOutcome { tick: Some(BuiltinTickExit::Joined), server: BuiltinServerExit::Quit(_) }`（**不是** `AbortedAfterTimeout`）；随后等待 `> 2 × interval`（真实时钟，`> 2s` + margin）trigger 计数不增加；`pool.builtin_task_count() == 0`；重复关闭 `is_none()`。
  - 断言形式必须是「计数不增 + join 已返回」，不得只断言 token 已 cancel。
- ② `mcp::builtin::cron::tests::tick_reconnect_has_single_driver_per_interval`（新增）：旧代 `close` 后建新代（同一 scheduler），跨 ≥2 个 interval 逐 interval 计数**恰好 1** 次触发（计数相等，不是「未观察到重复」）；旧代 guard 不得再产生触发。
- ③ `mcp::builtin::cron::tests::tick_disabled_has_no_driver_but_tools_work`（新增）：`tick_enabled=false` ⇒ `tick` 为 `None`，超过一个 interval 计数为 0，工具调用仍成功。
- ④ 池级 `mcp::builtin::cron::tests::tick_guard_reports_join_state`（新增）：`TickGuard::is_finished()` 在 join 前为 `false`、`shutdown` 后为 `true`（A32 允许的等价可观察量）。
- 时间口径：`peri-middlewares` **未**启用 tokio `test-util`（`peri-middlewares/Cargo.toml:20` 只有 `features = ["process"]`；现状 `peri-middlewares/src/middleware/web_test.rs:432` 同样注明不使用 `start_paused`）⇒ 测试一律用真实时钟 + 有界等待，禁止 `#[tokio::test(start_paused = true)]`。

### 3.6 tick 验收范围（F10）

- 「宿主无 tick task」只指：`peri-acp/src/host/assemble.rs:308-320` 的 `HostTaskKind::CronTick`（驱动 MCP 所用共享 ACP scheduler）与 `peri-acp/src/host/task_scope.rs` 中该变体被删除。
- TUI 仍有**私有** scheduler 与自建 tick：`peri-tui/src/app/cron_state.rs:9-34`（`CronState::new` 自建 `CronScheduler`、`spawn_tick_task` 自建 1s interval），调用点 `peri-tui/src/app/mod.rs:97-99`。该 scheduler 由 `HostAssemblyInput` 之外的 TUI 侧持有，**不是** MCP/pool 所用 scheduler（`peri-acp/src/host/assemble.rs:301-325` 内部构造）；A15/§10 第 4 项裁决不修。
- 因此验收**禁止**使用「全仓 grep 零 tick / 零 interval」这类断言；只允许断言 ACP 侧 `HostTaskKind::CronTick` 不存在，且 MCP 所用 scheduler 的 driver 计数符合 ②。

## 4. 事件出口与审批链

- handler 与宿主端口共用同一 `Arc<Mutex<CronScheduler>>`：`CronInstanceInput.scheduler` 由组合根构造一次，同时交给 `CronMcpServer`（工具面）与 `TickGuard::spawn`（驱动面）；宿主持 `CronSchedulerPortHandle` 包装同一 Arc。生产路径不得出现第二个 scheduler（`Arc::ptr_eq` 断言见 §8）。
- `CronScheduler::tick()` 的触发分发（`peri-middlewares/src/cron/mod.rs:118-155`）不改：primary `trigger_tx` + `extra_trigger_txs` 各发一份 clone。
- 订阅者 `SessionCronBridge::start`（`peri-acp/src/session/cron_bridge.rs:22-50`）继续经 `CronSchedulerPort::subscribe`（`peri-acp-types/src/cron.rs:49-64`）取触发并转 `CronContinuationRequest`。
- 审批门不变：`run_cron_continuation_scheduler` 调既有 `approve_scheduled_trigger`（`peri-acp/src/host/continuation.rs:151-200` 一带）；只有审批通过后才进 continuation。MCP `call_tool` 不写 inbox、不绕过审批。
- `QueuedMessage(MessageSource::CronTrigger)` 的产生点仍是宿主 `peri-acp/src/host/continuation.rs::enqueue_cron_trigger`（`:250-282`），不在 handler、不在 `tick()`。
- **不引入 MCP 通知**：不调用 server→client `notify_*`，不声明 subscriptions/capability（A16）；触发出口只有 `CronSchedulerPort::subscribe` 路线 B（A3）。

## 5. 关闭四类分列（F8）+ 五维矩阵

### 5.1 四类定义与边界

| 类 | 触发面 | 代码位置 | 语义 |
| --- | --- | --- | --- |
| 1. MCP 配置 `disabled: true` | 用户在 `.mcp.json` / settings 写 `{"cron": {"disabled": true}}` | `peri-middlewares/src/mcp/initialize.rs:247-256`（跳过连接、注册 `Disabled` handle）；运行时切走 `peri-middlewares/src/mcp/client/lifecycle.rs:137-175` `set_disabled` | 连接层禁用：不建 transport/handler/tick |
| 2. MetaHarness `policy_key=false` | `"CronMiddleware": false` | 投影点 `peri-middlewares/src/assembly/mcp.rs:71-76`（`closed_instances(&ctx.meta_harness_disabled)` → `with_builtin_closures`） | **只**影响本 turn 工具投影 |
| 3. `PERI_MCP_BUILTIN=off` | env | `peri-middlewares/src/mcp/builtin/mod.rs:70-86`（策略）；`:201-227` 规则 1/2 受策略控制 | 全局不注入：本波两实例零注入 |
| 4. 物理 close | instance close / reconnect / host shutdown | `peri-middlewares/src/mcp/client/lifecycle.rs:119-134`、`:137-175`、`:237-320`；`peri-middlewares/src/mcp/reconnect.rs:57-63` | 销毁该代 transport/handler/tick |

边界纪律：第 2 类的「保留 handler/pool/readiness」**只**适用于它；第 1、3 类不保留任何实例对象；第 4 类才停 tick。三者都不得用「最后一个 `Arc` 释放」代替显式关闭（A1/A24）。

### 5.2 五维矩阵

| 维度 | 1. `disabled: true` | 2. `CronMiddleware: false` | 3. `PERI_MCP_BUILTIN=off` | 4. 物理 close |
| --- | --- | --- | --- | --- |
| 工具可见性 | 无连接 ⇒ 无工具；不构成 system 依赖 | 本 turn 不挂 cron effective tools；声明表与 effective name 不变 | 零注入 ⇒ 零工具 | handler 不再服务；调用不可达 |
| tick | 无（从未构造 guard） | **保持**（按 `tick_enabled` 继续驱动同一 scheduler） | 无（无 cron 条目 ⇒ 无 transport） | `TickGuard::shutdown` cancel + 有界 join；`BuiltinCloseOutcome.tick` 记 `Joined`/`AbortedAfterTimeout` |
| readiness | 该实例不参与 1R 要求；builtin 关闭片段 `disabled + system_mcp` 在加载期被拒（`peri-middlewares/src/mcp/builtin/mod.rs:113-119`，测试 `peri-middlewares/src/mcp/builtin/builtin_test.rs:546-562`） | **保持**：readiness 是 pool 级事实，不由 MetaHarness 投影改写 | 该实例不注入 ⇒ 不参与 | close 后证据清除（`clear_evidence`，`lifecycle.rs:133`/`:174`），不再 ready |
| 物理生命周期 | 无对象；运行时切换经 `set_disabled` 关闭既有 service 并 join tick | **保持**：不销毁 scheduler / handler / transport / tick | 无对象可销毁 | instance close 销毁该代；reconnect 先停旧代再建新代；host shutdown 排空并逐项收敛 |
| 事件 | 无 tick ⇒ 无新触发；同一 scheduler 与订阅者仍在 | **保持**：`subscribe` 照常交付；审批门不变 | 无 tick ⇒ 无 `CronTrigger` 产生 | tick 停止 ⇒ 无新触发；已排队事件按既有 continuation 生命周期处理 |

## 6. C-04：删除面与保留面

1. 删除 `peri-middlewares/src/cron/middleware.rs`（`:12-37`），不留 deprecated shim / `#[allow(dead_code)]`。
2. `peri-middlewares/src/cron/mod.rs` 删除 `pub mod middleware;`、`pub use middleware::CronMiddleware;`（`:1-8`）；保留 `CronScheduler`、`CronError`、`CronTask`、`CronTrigger`、`CronSchedulerPortHandle` 与 `pub mod tools;`。
3. `peri-agent/src/session/factory.rs`：删 `ChainSlot::Cron`（`:59-60`）与 slot 序列中的 `ChainSlot::Cron`（`:108-110`）。
4. `peri-middlewares/src/assembly.rs`：删 `ChainSlot::Cron if disabled.contains("CronMiddleware")` 与构造分支（`:280-289`）、删 `cron::{CronMiddleware, CronScheduler}` import 中的 `CronMiddleware`（`:28`）。`assembly/preparation.rs:43-55` 的端口 → `Arc<Mutex<CronScheduler>>` 还原保留（改为供 context 构造消费）。
5. 清 re-export 与旧断言：`peri-middlewares/src/lib.rs:73`、`:111`；`peri-middlewares/src/assembly_test.rs:457`、`:492`、`:643`、`:856`、`:1595`（槽位名/链名断言改为不含 `CronMiddleware`）。
6. `peri-middlewares/src/cron/tools.rs` 不删除、不复制、不改业务输出与参数校验；它成为 handler 的唯一工具实现。
7. 保留关系：`CronScheduler` + `CronSchedulerPortHandle`（状态/事件端口）、`cron/tools.rs`（三 `BaseTool`）、`peri-middlewares/src/mcp/builtin/cron.rs`（MCP adapter）、`TickGuard`/`BuiltinInstanceSupervisor`（代驱动与关闭）。声明表中的 `CronMiddleware` 关闭键继续存在，但它是 builtin 实例策略键，不再对应任何 middleware 结构。

## 7. A26：上下文注入时序（cron）

- 载体（IF-P3-04 冻结形状）：

```rust
pub struct BuiltinInstanceContext { pub cwd: String, pub cron: Option<CronInstanceInput>, pub lsp: Option<LspInstanceInput> }
pub struct CronInstanceInput { pub scheduler: Arc<parking_lot::Mutex<CronScheduler>>, pub tick_enabled: bool }
```

- 公开路径：类型定义在 `peri-middlewares/src/mcp/builtin/context.rs`（新），经 `peri-middlewares/src/assembly.rs` re-export 给 `peri-acp`（`builtin` 模块仍 `pub(crate)`，不直接对外开放）。
- 池侧存储与 setter（owner H-02）：

```rust
// peri-middlewares/src/mcp/client.rs；范式与 execution_cwd（:64）和 bind_execution_cwd（:177-192）一致
pub(crate) builtin_instance_context: std::sync::OnceLock<BuiltinInstanceContext>,
pub fn set_builtin_instance_context(&self, ctx: BuiltinInstanceContext) -> Result<(), BuiltinContextError>,
```

`OnceLock::set` 重复调用返回 `Err(value)` ⇒ 转 typed `BuiltinContextError::AlreadyInjected`（**首个上下文继续生效**），runtime `tracing::warn!` 记录并拒绝；错误文本只含实例名，不含路径/凭据（§9 规则 8）。
- **时序（必须早于 initialize）**：注入调用点与 `bind_execution_cwd` 同域——`peri-acp/src/host/assemble.rs:101-114` 的 `pending_mcp_pool(...)`；`run_initialize` 在 `:451-464` 才 spawn。同一文件内的行序即「早于 initialize」的可核实证据（H-04）。
- 构造责任：组合根（`peri-acp/src/host/assemble.rs:301-325` 构造 host 级 `Arc<Mutex<CronScheduler>>` 与 `tick_enabled = drive_cron_tick`）构造 context，**不**由 handler、不由 loader 构造第二份状态。
- 注入缺失的后果：`spawn_builtin_transport` 对 cron 返回 typed error（建议复用 `BuiltinSpawnError::HandlerNotWired` 之外的新变体或沿用 IF-P3-04 复测结果），`system_mcp=true` ⇒ 1R fatal，不得静默降级为「无 tick 的空实例」。

## 8. 测试清单与验收映射

口径：`0 tests` 与「只命中旧用例」均算失败；下表显式标注**既有**（须给出扩展点）与**新增**。命令在实施时串行执行。

### 8.1 声明、两表与三面契约（C-01）

| 具名测试 | 状态 | 可观察断言 | §8 行 |
| --- | --- | --- | ---: |
| `peri_acp_types::builtin_mcp::tests::cron_declarations_are_frozen` | 新增 | `find("cron")` 命中；name/instance/policy_key 逐字；三原始名/effective name、`direct=false`、`prompt_declaration=None`、顺序与数量 | 3、12、20 |
| `peri_acp_types::builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance` | 既有，按 §1.2 收口 | 唯一性不变；模板断言仅 web/artifact；cron 三工具为 None | 12 |
| `peri_acp_types::builtin_mcp::tests::policy_keys_are_unique_and_frozen` | 既有，加 `CronMiddleware` | 策略键集合与 `BUILTIN_INSTANCE_POLICY_KEYS` 一致 | 9、21 |
| `peri_acp_types::builtin_mcp::tests::find_hits_only_implemented_instances` | 既有，cron 改判命中 | `find("cron").tools.len() == 3`；`workspace` 仍未实现 | 3、20 |
| `peri_acp_types::builtin_mcp::tests::original_tool_name_of_effective_hits_frozen_literals` | 既有，加三 literal | 三个 `mcp__cron__*` → 原始名 | 11 |
| `peri_acp_types::meta_harness::tests::builtin_instance_policy_keys_match_declaration_table` | 既有 | `BUILTIN_INSTANCE_POLICY_KEYS ==` 声明表 policy key 集合（含 `CronMiddleware`） | 9 |
| `peri_acp_types::meta_harness::tests::middleware_names_and_builtin_policy_keys_are_disjoint` | 既有 | `CronMiddleware` 不在 `MIDDLEWARE_NAMES` | 9、21 |
| `mcp::builtin::tests::frozen_effective_literals_match_effective_tool_name` | 既有 | 计算值与三冻结字面量逐字相等 | 3、12 |
| `mcp::builtin::tests::injection_policy_all_and_none_shapes` | 既有，按 §1.2 收口 | `all().enabled_instances()` 含 `cron` | 1 |
| `mcp::builtin::tests::overlay_does_not_inject_reserved_unimplemented_instances` | 既有，cron 移出 | `cron` 注入且 `source` 为 `Builtin`；`lsp`/`workspace` 仍不注入 | 1 |
| `mcp::builtin::tests::declared_direct_tools_is_derived_from_registry` | 既有 | cron 派生 direct 集合为空，与 `system_mcp_tools == []` 一致 | 4、12 |
| `mcp::builtin::tests::builtin_prompt_declaration_matches_registry` | 既有，按 §1.2 收口 | cron 三工具返回 `None` | 12 |
| `mcp::builtin::tests::effective_tool_names_covers_registry` | 既有 | 覆盖 `mcp__cron__*` 三个 effective name，无裸名并存 | 3、16、24 |

### 8.2 handler / schema / 错误 / tick（C-02、C-03）

| 具名测试 | 状态 | 可观察断言 | §8 行 |
| --- | --- | --- | ---: |
| `mcp::builtin::cron::tests::list_tools_uses_existing_cron_definitions` | 新增 | `tools/list` 三名字、description、required/properties 与三个 `BaseTool::definition()` 逐字相等 | 2、22 |
| `mcp::builtin::cron::tests::call_tool_routes_each_name_to_shared_base_tool` | 新增 | register/list/remove 分别写/读/删同一 scheduler；未知名 → invalid params | 6、22 |
| `mcp::builtin::cron::tests::call_tool_uses_if_d14_error_mapping` | 新增 | 业务 `Err` → 固定脱敏 `CallToolResult::error`；成功 → success content；文本不含 `CronError` 原文 | 22、23 |
| `mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers` | 新增 | A32 断言①（§3.5） | 7、15 |
| `mcp::builtin::cron::tests::tick_reconnect_has_single_driver_per_interval` | 新增 | A32 断言② | 7、15 |
| `mcp::builtin::cron::tests::tick_disabled_has_no_driver_but_tools_work` | 新增 | A32 断言③ | 7、10 |
| `mcp::builtin::cron::tests::tick_guard_reports_join_state` | 新增 | A32 断言④ | 7 |
| `cron::tests::test_tick_fires_trigger` | 既有 | `CronScheduler::tick` 交付一次 `CronTrigger` | 6、7 |
| `cron::tests::test_tick_skips_disabled` | 既有 | disabled 任务不产生事件 | 6、10 |
| `cron::tests::test_tick_removes_dead_extra_sender` | 既有 | 断开 receiver 被清理，不 panic | 6 |
| `cron::tests::test_cron_scheduler_port_downcast_restores_concrete` | 既有 | port 还原同一具体 scheduler | 6、15 |
| `cron::tools::tests::test_register_success`、`cron::tools::tests::test_register_rejects_empty_prompt` | 既有 | 工具实现未被第二实现替换 | 19、23 |

### 8.3 注入、删除面、关闭四类与端到端（跨 owner）

| 具名测试 | 状态 | 可观察断言 | §8 行 |
| --- | --- | --- | ---: |
| `mcp::builtin_runtime_tests::builtin_context_injection_precedes_initialize_and_rejects_repeat` | 新增 | 首次注入 Ok；重复注入 typed Err 且首个仍生效；注入早于 `run_initialize` | 2、15 |
| `mcp::builtin_runtime_tests::cron_tick_uses_injected_scheduler_identity` | 新增 | handler 工具面与 tick 用同一 `Arc`（`Arc::ptr_eq`），无第二 scheduler | 6、15 |
| `mcp_isolation_contract::instances_have_independent_transport_task_and_state` | 既有 | cron 与其他实例 transport/task/state 不共享；关闭 cron 不影响其他实例 | 15 |
| `assembly::tests::production_chain_has_only_lsp_sync_slot` | 新增 | production chain 无 `ChainSlot::Cron`/`CronMiddleware`；cron 端口仍在 | 21 |
| `assembly::tests::cron_policy_close_keeps_builtin_lifecycle` | 新增 | §5.2 第 2 类列（可见性关、其余保持） | 10、15 |
| `assembly::tests::cron_policy_close_is_not_physical_close` | 新增 | 第 2 类与第 4 类分列：策略关闭后 handler/tick 仍在，物理 close 后 join 完成 | 10 |
| `host::mcp_v4_wave2::cron_register_tick_approval_continuation` | 新增（H-04/V-03） | effective register → tick → subscribe → 审批 → `QueuedMessage(MessageSource::CronTrigger)` → continuation；无 MCP notification | 6 |
| `host::mcp_v4_wave2::reconnect_has_single_tick_driver` | 新增（H-04/V-03） | reconnect 后旧代 join 完成、新代每 interval 恰 1 次；ACP 侧无 `HostTaskKind::CronTick`（范围见 §3.6） | 7、15、21 |
| `host::mcp_v4_wave2::search_execution_uses_effective_names` | 新增（V-03） | 搜索/执行面裸名 XOR effective name；直连面不含三工具 | 3、16、24 |

## 9. 与代码冲突项

1. **A32 覆盖本文旧稿**：旧稿把 tick 放在 `CronMcpServer` 内（沿用主计划 IF-P3-05 的伪码形状）。A32 改判为「代监督者持有 + 池 close 路径显式 join」，本文 §2/§3 已重写；主计划 §3 IF-P3-05 的伪码与 §4.2 `cron.rs` 行描述仍按旧形状书写，**以 A32 为准**，需要主计划侧同步（建议在 §3 IF-P3-05 追加一行覆盖说明）。
2. **`mcp/builtin/runtime.rs:71-111` 的表形状**：`BuiltinServerTask` 只承载 server task，`close_builtin_task`（`peri-middlewares/src/mcp/client/lifecycle.rs:368-378`）只 converge server task，`close_builtin_tasks`（`:384-402`）同——即当前**没有**任何 tick 关闭可言。A32 的监督者与 `BuiltinCloseOutcome` 属新增，不是「修正已有实现」。
3. **`_test.rs` 路径漂移（F11）**：主计划 §4.2 把 `builtin_apply_test.rs` / `builtin_runtime_test.rs` 列在 `peri-middlewares/src/mcp/builtin/` 下；实际位于 `peri-middlewares/src/mcp/builtin_apply_test.rs`、`peri-middlewares/src/mcp/builtin_runtime_test.rs`（挂载见 `peri-middlewares/src/mcp/mod.rs:73-84`，模块名 `mcp::builtin_apply_tests` / `mcp::builtin_runtime_tests`）。本文按真实路径引用；主计划该表需修正。
4. **注入点 owner 与时机冲突**：主计划 §4.3 把 `BuiltinInstanceContext` 的装配注入放在 `peri-middlewares/src/assembly/mcp.rs`（H-03）。但 `run_initialize` 在宿主装配期就 spawn（`peri-acp/src/host/assemble.rs:451-464`），而 `assembly/mcp.rs::add_mcp` 是 per-session 链装配，时序上**晚于** initialize ⇒ 按原位置注入会太晚。建议处置：注入落 `peri-acp/src/host/assemble.rs:101-114`（与 `bind_execution_cwd` 同域，H-04），`assembly/mcp.rs` 只读不注入；A26「必须早于 initialize」不变。
5. **`BuiltinSpawnError` 变体语义**：`peri-middlewares/src/mcp/builtin/runtime.rs:44-52` 的 `UnknownInstance` / `HandlerNotWired` 未覆盖「注册表已实现但上下文未注入」。IF-P3-04 已声明该错误变体以 H-02/H-05 对 runtime 的复测结果为准；本文只要求在 cron 上 fail-fast（不静默降级），具体变体名由 H-02 复测后定。
6. **TUI 私有 tick（F10）**：`peri-tui/src/app/cron_state.rs:26-34` 与 `peri-tui/src/app/mod.rs:97-99` 仍自建 scheduler + 1s tick，属 A15 既有缺陷；本文 §3.6 已限定验收范围。若验收脚本使用全仓 grep，会与 A15「不修」直接冲突，需在 V 侧明确排除。
7. **端口实现位置**：`peri-acp-types/src/cron.rs:49-64` 只定义 trait；实现是 `peri-middlewares/src/cron/mod.rs:199-232` 的 `CronSchedulerPortHandle`（`peri-acp-types/src/cron.rs` 顶注写「`Mutex<CronScheduler>` 实现该端口」，与代码不符）。施工以 wrapper 为准。

## 10. 施工顺序 + 提交切分

每步一个可验证状态；不得混入 lsp、workspace 或未登记装配文件。命令为精确过滤器，`0 tests` 或只命中旧用例均判失败；cargo 命令由实施者执行（本次只写文档）。

1. **Commit C-01（C-01）**：cron 声明三项、实例条目、effective literals；按 §1.2 收口契约测试。
   - `cargo test -p peri-acp-types --lib -- builtin_mcp::tests::cron_declarations_are_frozen`
   - `cargo test -p peri-acp-types --lib -- builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance`
2. **Commit C-02（C-02）**：`mcp/builtin/cron.rs` + `cron_test.rs` 挂载；工具面经共享 helper；**不含** tick。
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::list_tools_uses_existing_cron_definitions`
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::call_tool_routes_each_name_to_shared_base_tool`
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::call_tool_uses_if_d14_error_mapping`
3. **Commit C-03（C-03，依赖 H-02 的监督者 seam）**：`TickGuard` / `BuiltinInstanceSupervisor` / `BuiltinCloseOutcome` 落地，池表值与 close 路径改 `close().await`；补 §8.2 四条 tick 用例。
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers`
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::tick_reconnect_has_single_driver_per_interval`
   - `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::tick_disabled_has_no_driver_but_tools_work`
   - 影响面回归：`cargo test -p peri-middlewares --lib -- mcp::builtin::runtime_test`（既有收敛用例，确认表值类型改动后仍绿）
4. **Commit C-04（C-04）**：删除 `cron/middleware.rs` 与 ChainSlot 引用，保留 scheduler/port/tools。
   - `cargo test -p peri-middlewares --lib -- cron::tests::test_tick_fires_trigger`
   - `cargo test -p peri-middlewares --lib -- cron::tests::test_cron_scheduler_port_downcast_restores_concrete`
   - `cargo test -p peri-middlewares --lib -- cron::tools::tests::test_register_success`
5. **集成闸门（H-02/H-03/H-04/V-03）**：上下文注入 + dispatch 变体 + 宿主 `CronTick` task 删除后，才验证注入/关闭四类/端到端。
   - `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::builtin_context_injection_precedes_initialize_and_rejects_repeat`
   - `cargo test -p peri-middlewares --lib -- assembly::tests::cron_policy_close_is_not_physical_close`
   - `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::cron_register_tick_approval_continuation`
   - `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::reconnect_has_single_tick_driver`

提交切分只规定「一个 commit 一个可验证步骤」；本文与实施均不执行 `git commit`，也不执行 cargo。
