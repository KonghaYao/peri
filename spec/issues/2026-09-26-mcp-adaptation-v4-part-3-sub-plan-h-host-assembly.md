# sub-plan H：宿主语义与装配

> 2026-09-28 合并回归修复澄清 A1/A32：组合根以会话环境为单位构造 scheduler，builtin 工具、宿主端口与该会话 bridge 同源；部署只传播 tick 开关与 continuation 入口，不以部署 scheduler 覆盖会话实例。每个 pool 的 supervisor 仍是对应 scheduler 的唯一 tick owner，关闭会话停止该驱动。

> 所属主计划：`spec/issues/2026-09-26-mcp-adaptation-v4-part-3-plan.md`（唯一裁决源）。
> 本文只规划宿主语义、builtin 装配、tick/上下文生命周期、归一与文档同步；不改 cron/LSP 业务工具实现，不重写 `CronSchedulerPort`，不新增第二个 LSP 同步端口。
> **本轮（2026-09-26）已把 A32 / A33 两条新裁决与 F1–F12 事实核查修正落进 §2/§3/§5/§7/§9/§10/§11**；原稿的「待裁决 / 可实施性不确定」表述已替换为冻结设计，剩余未闭合项集中列在文末 K1–K8。
> 施工基线以主计划 §0、§3、§4、§5、§6、§8–§11、W0 验收记录与本轮裁决为准；仍未闭合项集中见文末「与代码冲突 / 未闭合项」。

## 覆盖范围表

| 接口/任务 | 本文施工面 | 依赖/前置 | 主要验收 |
| --- | --- | --- | --- |
| IF-P3-03 / H-02 | 默认注入、1R readiness、关闭集、上下文注入（A33） | C-01、L-01、H-01 | §8-1/2/4/5/10 |
| IF-P3-04 / H-02 | `BuiltinInstanceContext`、pool 方法、一次性注入、三态生命周期 | C-02、L-02 | §8-2/7/15 |
| **A32** tick 归属 | `TickGuard` / `BuiltinInstanceSupervisor` / `BuiltinCloseOutcome`、pool 单 spawn 点、`supervisor.close` 收敛 | C-02、H-02 | §8-7/15/19 |
| **A33** 注入点 | 宿主装配（H-04）构造并注入，早于 `run_initialize`；`assembly/mcp.rs` 不参与 | H-02、H-04 | §8-2/15 |
| IF-P3-07 / H-01/H-05 | dispatch 搬迁与 cron/lsp 变体接线 | H-01；H-05 另等 C-02/L-02 | §8-15/20 |
| IF-P3-10 / S-02 | 两表、静态工具表与三条不变式 | C-01；H-03 | §8-9/24 |
| IF-P3-11 / S-01/S-03 | 归一消费点（模型可见文本含 F9）与 TUI 回归（F5） | H-03 | §8-11/12/17 |
| IF-P3-12 / H-03/H-04 | 能力声明、隔离、host 组合根接线 | H-02、L-03 | §8-2/5/13/15 |
| S-04/S-05 | 文本预审、代码索引、参考文档、skill、`CLAUDE.md` | H-04、S-01 | §8-17；`DOC-UPDATE-001` |
| 裁决 | A1/A7/A8/A9/A10/A11/A24/A26/A27/A28/**A32/A33** | 主计划优先 | 本文各节与文末清单 |

## 1. 不变量与边界

- 组合根只构造一份 cron scheduler 与一份 host 级 LSP pool；实例上下文由**宿主装配**构造并注入（A33）；builtin 实例是唯一 MCP 工具适配器，宿主只经端口接收 cron 事件、投影 UI、执行 LSP 同步。
- `cron` 三工具和 `LSP` 均 `direct: false`、`prompt_declaration: None`；`system_mcp: Some(true)`、`system_mcp_tools: Some([])` 保持 1R readiness，但不提升 direct、不做 required 工具校验。
- `lsp` 的工具快照谓词是合并后的有效配置 `has_servers()`，不是 language server 进程 ready；handler 构造必须晚于配置合并，不支持热更新。
- `CronMiddleware` / `LspMiddleware` 不再是工具链能力槽位：cron 与 LSP 工具都由 builtin MCP 提供；`LspSyncMiddleware` 只负责 Write/Edit 后同步。
- **关闭四类分开（F8）**：① MCP 配置 `disabled: true` → 跳过连接（注册为 `Disabled`）；② MetaHarness `policy_key: false` → 只关本 turn 工具投影，**保留** handler/pool/readiness；③ `PERI_MCP_BUILTIN=off` → 不注入 builtin（零注入，不是"仍 connected"）；④ 物理 close → 停任务、释放资源。**「保留 handler/pool/readiness」只适用于 ②**，不得写成通用语义。
- tick 归属（A32）：cron 1s tick 由 pool 在建立 transport 时 spawn 一次，绑定到**该代 transport 的 supervisor**；handler（`CronMcpServer`）不持有 tick，`Drop` 只作兜底。
- 命名面：模型面 / ACP 事件 / transcript 用 effective name（`mcp__cron__cron_*`、`mcp__lsp__LSP`）；实际 MCP `tools/call.name` 仍是原始工具名（`peri-middlewares/src/mcp/tool_bridge.rs:87-90` 以 `(original, effective)` 成对构造）。归一不改写传输身份。
- W0 证据引用 acceptance §2（直连 18 项、摘要 7 项、搜索面裸名 XOR），本文不重跑；覆盖登记映射：H-01/H-05 → R11/R13/R17；H-02 → R2–R4/R18/R24–R26；H-03/H-04 → R1/R7/R15/R19/R23/R25；S-01/S-03 → R5/R14；S-02 → R8/R21；S-05 → R20/R27。
- IF-P3-12：cron/lsp `server_info` 仅声明 tools，不声明 resources、不启用订阅、不覆写 discover；每实例独立 duplex/task，scheduler 与 LSP pool 状态不混用；capability root / 凭据隔离仍为 **UNVERIFIED**。

## 2. H-01/H-05：dispatch 重构与接线边界

### 2.1 H-01 搬迁步骤（IF-P3-07）

1. 新建 `peri-middlewares/src/mcp/builtin/dispatch.rs`，从 `peri-middlewares/src/mcp/builtin/web.rs` 原样搬出 `BuiltinServerHandler`、`builtin_server_handler`、`impl ServerHandler`；先只保留现有 `Web` / `Artifact` 变体，保持行为等价。
2. 在 `peri-middlewares/src/mcp/builtin/mod.rs` 挂载 `dispatch`；`peri-middlewares/src/mcp/builtin/runtime.rs` 的生产调用改为 `super::dispatch::builtin_server_handler`，不在 runtime 重新实现字符串分派。
3. `dispatch.rs` 的 `impl ServerHandler` 只做 enum 分派（`server_info` / tools/list / call_tool 转发）；不覆写 `discover`，不改变 IF-D14 映射（映射本体仍在 `web.rs::invoke_tool_call`）。
4. `peri-middlewares/src/mcp/builtin/web.rs` 收敛为 `WebMcpServer` 与共享助手（`server_info` / `rmcp_tool_from_base` / `list_tools_of` / `invoke_tool_call`）；删除 enum、工厂与其测试导入。`peri-middlewares/src/mcp/builtin/artifact.rs` 只保留实例与共享助手。
5. 把"工厂只覆盖已实现实例"的断言从 `peri-middlewares/src/mcp/builtin/web_test.rs` 迁到 `peri-middlewares/src/mcp/builtin/dispatch_test.rs`，并保留未知实例 = `None`（不静默回退）。
6. `dispatch_test.rs` 用 `#[path]` 挂到 `peri-middlewares/src/mcp/builtin/mod.rs`，过滤器 `mcp::builtin::dispatch`；迁移期 `web_test.rs` 不得再通过旧路径验证工厂。
7. 交接事实（F12）：三分类 transport 的事实源是 `peri-middlewares/src/mcp/transport.rs` 的 `TransportKind`（`:15`，`TransportConfig::kind()` `:46-50`）；`peri-middlewares/src/mcp/client/transport.rs` 只是 client 侧 handler/transport 适配（`serve_client_auto` :18、`build_http_transport` :59、`build_authed_transport` :87），不承载分类判定。旧稿把 `mcp/client/transport.rs` 当分类事实源是错的，已更正。

### 2.2 H-05 的新增变体边界

- H-05 在 C-02 的 `CronMcpServer` 与 L-01/L-02 的 LSP 实例/端口形状可用后，把 `Cron(CronMcpServer)`、`Lsp(LspMcpServer)` 变体及 factory 分支接入 `dispatch.rs`；工具类型与业务行为归 C/L owner。
- `peri-middlewares/src/mcp/builtin/dispatch.rs` 与 `dispatch_test.rs` 的文件写入权始终归 H-01；H-05 是集成阶段的逻辑接线，H-01 负责落地/复核，避免两个 owner 同时编辑。
- 顺序硬门：H-01 的搬迁与接口交接先于 C-02/L-01 消费 dispatch；L-02 无 H-01 依赖可先行；H-02 依赖 C-01/L-01/H-01；H-05 必须等 C-02/L-02/H-01/H-02 与 L-01 实例完成。
- 编译交接：`peri-middlewares/src/mcp/builtin/mod.rs` 的模块挂载 owner 是 H-02，但 C-02/L-01 的模块测试需要先有挂载；C-04 删除 `peri-middlewares/src/cron/middleware.rs` 与 H-03 删除 import/槽位之间也不得留悬空引用。两处须由主计划 owner 确认原子边界（见文末未闭合项 K2）。
- 工厂签名冻结：`builtin_server_handler(instance: &str, ctx: &BuiltinInstanceContext) -> Option<BuiltinServerHandler>`；`cwd` 不再单独分派。

## 3. H-02：实例上下文、tick 归属与生命周期

### 3.1 实例上下文（IF-P3-04 / A33）

公开类型与工厂只经 `peri-middlewares/src/assembly.rs`（`peri_middlewares::assembly`）暴露；`peri-middlewares/src/mcp/mod.rs:7` 保持 `pub(crate) mod builtin`，**不**为 `peri-acp` 放开 `mcp` 模块。

```rust
pub struct BuiltinInstanceContext {
    pub cwd: String,                                  // artifact 解析根 / LSP root 来源
    pub cron: Option<CronInstanceInput>,              // 见 IF-P3-05 与下方 A32
    pub lsp: Option<LspInstanceInput>,                // 见 IF-P3-08
}
pub struct CronInstanceInput {
    pub scheduler: Arc<Mutex<CronScheduler>>,         // 组合根同一份，不复制
    pub tick_enabled: bool,                           // 由 drive_cron_tick 投影
}
pub struct LspInstanceInput { pub pool: Arc<LspServerPool> }
```

逐字段施工约束：

- `cwd`：host 单 cwd（与 pool `execution_cwd` 同源）；不借此支持多 cwd。
- `cron.scheduler`：同一 `Arc`，`Arc::ptr_eq` 可观察；builtin 内不得另建 scheduler。
- `cron.tick_enabled`：`HostAssemblyInput.drive_cron_tick: bool`（`peri-acp/src/host/assemble.rs:130`）的投影；`true` 仅 TUI（`peri-tui/src/launch.rs:188`），print/stdio/session workspace 为 `false`（`peri-tui/src/cli_print.rs:157`、`peri-acp/src/host/stdio/mod.rs:150`、`peri-acp/src/host/workspace.rs:74`）。代码中不存在 `DriveCronTick` 类型，也不新增。
- `lsp.pool`：host 唯一 pool（经 `peri_resources::lsp` 门面构造，宿主投影为 `Arc<dyn LspPoolPort>`）；无 LSP 配置时仍注入空配置 pool（`Some(LspInstanceInput)`），不得沿用旧 `create_session_lsp_pool` 返回 `None` 造成 context 缺失。
- **关闭集（A33）**：`closed_instances(&HashSet<String>) -> BTreeSet<String>`（`peri-middlewares/src/mcp/builtin/mod.rs:188`）由注册表 `policy_key` 派生，随上下文一起由宿主装配传入。⚠️ 主计划 IF-P3-04 现行字段清单未列该字段，需主计划补字段或明确其承载位置（未闭合项 K1）。
- `tick: Option<TickGuard>`、`cancel`、`join` 属 A32 的 transport 侧 supervisor，不是 context 字段，也不在 handler 内。

### 3.2 A32：tick 归属与关闭收敛（已冻结）

**新增类型（`peri-middlewares/src/mcp/builtin/runtime.rs`）**：

```rust
pub(crate) struct TickGuard { cancel: CancellationToken, join: JoinHandle<()> }
impl TickGuard { fn spawn(...) -> Self; async fn shutdown(self) -> ...; fn is_finished(&self) -> bool; }

pub(crate) struct BuiltinInstanceSupervisor { /* tick: Option<TickGuard>, server: BuiltinServerTask */ }
impl BuiltinInstanceSupervisor { fn new(...) -> Self; async fn close(self) -> BuiltinCloseOutcome; fn tick_is_finished(&self) -> bool; }

pub(crate) struct BuiltinCloseOutcome { pub tick: Option<...>, pub server: ... }
// BuiltinTransport 增 tick 字段与 into_parts()
```

冻结语义：

1. **spawn 点唯一**：`McpClientPool::spawn_builtin_transport(&self, instance)`（pool 方法）。仅当上下文 `cron.tick_enabled == true` 且实例为 `cron` 时 spawn tick，tick 立刻绑定到本次 transport 的 supervisor；不存在第二个 spawn 点。
2. **收敛路径唯一**：`peri-middlewares/src/mcp/client/lifecycle.rs` 的 `close_builtin_task` / `close_builtin_tasks` 改为 `supervisor.close(...).await`，顺序**先 tick cancel + 有界 join，再收敛 server task**；`BuiltinCloseOutcome` 同时给出两侧结果。`abort` 仍不是正常路径。
3. **调用者自动覆盖**：`peri-middlewares/src/mcp/initialize.rs`、`peri-middlewares/src/mcp/reconnect.rs`、`McpClientPool::remove_server`（`peri-middlewares/src/mcp/client/lifecycle.rs:119`）、`McpClientPool::set_disabled`（同文件 `:137`）、pool `shutdown`（同文件 `:237`）都经上述两个方法，无需各自新增关闭逻辑。
4. **handler 不持 tick**：`CronMcpServer` 只持 `Arc<CronScheduler>` 供工具面使用；`Drop` 仅兜底 cancel，不承担正常收敛。
5. **重复 spawn 保护**：同一 scheduler 任一时刻至多一个 tick 驱动；reconnect 先 close 旧 supervisor（tick join 完成）再建新一代。
6. **机械改动清单（已核实调用点）**：
   - `register_builtin_task`：生产 2 处 —— `peri-middlewares/src/mcp/initialize.rs:314`、`peri-middlewares/src/mcp/reconnect.rs:119`；测试 5 处 —— `peri-middlewares/src/mcp/builtin/runtime_test.rs:595,611,673,707,710`（共 7 处）。
   - `close_builtin_task` 消费点：生产 5 处 —— `peri-middlewares/src/mcp/initialize.rs:329`、`peri-middlewares/src/mcp/reconnect.rs:63`、`peri-middlewares/src/mcp/reconnect.rs:133`、`peri-middlewares/src/mcp/client/lifecycle.rs:130`、`peri-middlewares/src/mcp/client/lifecycle.rs:148`；测试 4 处 —— `peri-middlewares/src/mcp/builtin/runtime_test.rs:678,687,728`、`peri-middlewares/src/mcp/builtin_runtime_test.rs:2129`。
   - ⚠️ A32 所称「`close_builtin_task` 的 2 处消费点」与现场计数（生产 5 + 测试 4）不一致；按上列逐点机械改动，并以主 agent 复核为准（未闭合项 K3）。

### 3.3 A33：注入点与注入时序（已冻结）

- **注入者是宿主装配**：`peri-acp/src/host/assemble.rs`（H-04）构造 `BuiltinInstanceContext` 并注入 pool，必须**早于** `McpClientPool::run_initialize(...)` 的调用（现行唯一生产调用点 `peri-acp/src/host/assemble.rs:455`）与其后台 spawn。
- **`peri-middlewares/src/assembly/mcp.rs` 不参与实例上下文注入**：该文件只负责 MCP 中间件装配（projection / deferred bridge / 关闭集投影），不得再作为 context 注入 owner（本轮修正原稿 §4.1）。
- **一次性注入**：首次成功；重复注入（含相同 `Arc` 再注入）返回 typed `Err`（建议名 `AlreadyInjected`，错误名以 H-02 实现为准），首个生效，runtime 记录并拒绝；并发两次恰一成功。
- **晚注入拒绝**：`initialize` 已开始后首次注入必须拒绝（建议 typed `InitializationStarted`）；实现用同一短锁保护 setter 与初始化标志（锁内禁止 await），不能只依赖 `OnceLock::set`。
- **可见性**：`peri-acp` 经 `peri_middlewares::assembly` 的公开工厂/类型拿到 context；不得 import `peri_middlewares::mcp::builtin`。
- **失败面**：无 context / 握手失败 / 超时沿既有 `insert_failed + commit_discovery_failure` 收口，`system_mcp` 闸门 fatal，不伪造 ready。
- **工具执行上下文缺口（F1）**：`peri-middlewares/src/mcp/tool_bridge.rs::invoke` **已**使用 `ToolContext`（`dispatcher` / `session_id` / `turn_generation` / `invocation_id` / `cancellation`，`:245` 起），不要写成「bridge 不使用 ctx」。真实缺口是 builtin 共享 helper `peri-middlewares/src/mcp/builtin/web.rs::invoke_tool_call`（`:91-108`）自行构造 `ToolContext::new(&[], cwd)`——**宿主 cwd/session 上下文没有传进 builtin 工具执行**。H-02/H-05 必须把 host 侧上下文接到该 helper（形状由 C-01/H-05 定，不得只传 `&[]`）。

### 3.4 三态对象矩阵（A24/A26 + A32）

| 状态 | 保留 | 显式销毁/关闭 |
| --- | --- | --- |
| host shutdown | 组合根只保留可读取的关闭结果 | 两实例 transport、supervisor（先 tick join 再 server task）、host LSP pool 与全部 language server；有界等待并断言无 orphan |
| instance close（MCP `disabled:true` / `remove_server`） | 组合根 scheduler/pool 与实例注册状态 | 该实例 transport + supervisor；不销毁 pool |
| MetaHarness 策略关闭（`policy_key:false`） | handler/pool/readiness 与 tick 全部保留，只关本 turn 工具投影与同步目标 | 无物理销毁 |
| reconnect | 同一 scheduler、同一 LSP pool、同一 context | 旧 supervisor（tick join 先行）→ 旧 server task → 新 generation；任一时刻同 scheduler 至多一个 tick |

## 4. H-03：装配面与 owner 边界

### 4.1 `assembly.rs` / `ChainSlot` / `AssemblyContext`

- `peri-agent/src/session/factory.rs`：删除 `ChainSlot::Cron` 与蓝本中的 Cron 槽位；保留 `ChainSlot::Lsp` 作为「同步中间件挂载点」。其余槽位不重排。
- `peri-middlewares/src/assembly.rs`：删除 `CronMiddleware` import、`ChainSlot::Cron` match 与 cron middleware 构造；`cron_scheduler_concrete` 仍作为 builtin context 输入。
- `ChainSlot::Lsp` 只调用 `peri-middlewares/src/assembly/lsp.rs::add_lsp` 装 `LspSyncMiddleware`；LSP builtin tool 不在 chain slot 内重复注册。
- `AssemblyContext`：保留 cron scheduler 端口语义；`lsp_servers` 是配置快照，`lsp_pool` 是 host pool 投影。`BuiltinInstanceContext` 不塞进每 session context。
- `peri-middlewares/src/assembly/mcp.rs`：只做中间件装配与关闭集投影，**不注入实例上下文**（A33，见 §3.3）；`open_builtin_bridges` 语义不重写。
- `peri-middlewares/src/assembly/lsp.rs`：LSP 装配唯一 owner；导出 `load_merged_lsp_servers` 与 host pool 构造/投影；不建第二个 pool、不实现 LSP 工具业务、不让 port 读文件。
- `peri-middlewares/src/assembly/preparation.rs` 与 `peri-middlewares/src/assembly/workflow.rs`：继续消费四面过滤结果；字段重命名只做编译性同步。

### 4.2 `assembly_test.rs` 同步（H-03 单一写入）

`peri-middlewares/src/assembly_test.rs` 至少同步：① `production_blueprint` 无 `ChainSlot::Cron`、仍含一个 `ChainSlot::Lsp`（`slot_name` 不再匹配 Cron）；② 有配置时 LSP 槽位只产生 `LspSyncMiddleware`，无配置不产生；③ `LspMiddleware:false` / `LspSyncMiddleware:false` / 双 false 交叉矩阵（工具面、同步目标、pool/readiness、物理生命周期）；④ context 的 scheduler/pool `Arc::ptr_eq` 与重复注入 typed error；⑤ 链名序列 / known-key union / 静态工具名集合只验证新契约（cron 工具与 LSP builtin tool 都不是 middleware 静态工具）。

## 5. H-04：ACP host 接线

### 5.1 `peri-acp/src/host/assemble.rs` 逐点改动

1. 保留组合根创建 cron scheduler；**删除** `HostTaskKind::CronTick` 的 spawn 与 `drive_cron_tick` 宿主循环（现行 `peri-acp/src/host/assemble.rs:305-321`）；`drive_cron_tick` 仅投影为 `context.cron.tick_enabled`。
2. LSP 配置合并前移：把现行 `load_merged_lsp_servers(...)` 块（`peri-acp/src/host/assemble.rs:550-559`，位于 MCP pool 创建之后）移到 MCP pool 初始化之前。
3. 顺序冻结：读取 global + plugin → `plugin_lsp_servers` 快照 → 创建 host `LspServerPool`（root = host `cwd`）→ 构造 `BuiltinInstanceContext` → 注入 pool → `run_initialize`（`:455`）。
4. host shutdown / session close 语义见 §5.2；host pool 归 host，session 不再持有。
5. `execution_cwd` 必须是单一 host cwd（`peri-middlewares/src/mcp/client.rs:64,177-180` 的 `OnceLock` 即现成边界）；多 cwd 共享 host root_uri 是已裁决的功能退化，不伪装 per-session。
6. 既有行为核对：global 配置无插件仍生效；global < plugin；bare 跳过插件/LSP/外部 MCP 配置，但保留 builtin workspace 池（2026-09-28 合并回归修复）；无 session resources 的宿主层仍不建 MCP 池；LSP 无配置仍 ready 但 tools/list 为空；有配置但未拉起 server 仍出现 LSP tool。

### 5.2 session、prompt 与 shutdown

- `peri-acp/src/host/requests/session_lifecycle.rs`：删除 `create_session_lsp_pool`（`:491-495`）及 `:181`、`:628`、`:1171` 等 session pool 构造点；session state 的 `lsp_pool` 改为 host pool 投影或移除；`session/delete` / `close_owned_session`（`:895-925`）不得 shutdown host pool。
- `peri-acp/src/host/prompt.rs`：从 host deployment/context 取唯一 pool，传入 `AssemblyContext`/`SessionContext`；每 turn 只 clone `Arc`，不按 session cwd 重建。
- host shutdown（`peri-acp/src/host/shutdown.rs`）：直接对 host 唯一 pool `shutdown().await` 一次（不再从 session state 收集，现行 `:43-55,125-135` 需改），随后断言 language server 全部收敛、无 orphan；不靠 Arc drop。
- 同一 shutdown 事务经既有 `begin_shutdown → service close → supervisor.close` 关闭 builtin transport/handler（A32）；cron tick 由 supervisor 显式 join。
- `peri-acp/src/host/task_scope.rs`：删除 `HostTaskKind::CronTick` 成员与相关计数/断言；其它 Startup/PluginCleanup 任务不受影响。
- session close 只关 session-owned workflow/环境资源；不关共享 scheduler、host LSP pool、其它 session 的 builtin transport。

## 6. S-01/S-02：两表迁移与归一

### 6.1 三张表逐字对照

`peri-acp-types/src/meta_harness.rs` 当前 `MIDDLEWARE_NAMES`：

```text
"DefaultSystemPromptMiddleware", "LangMiddleware", "AgentsMdMiddleware",
"AgentDefineMiddleware", "PluginMiddleware", "SkillsMiddleware", "SkillPreloadMiddleware",
"AtMentionMiddleware", "ImageMiddleware", "FilesystemMiddleware", "GitAttributionMiddleware",
"GitWatchMiddleware", "TerminalMiddleware", "TodoMiddleware", "CronMiddleware",
"HookMiddleware", "PermissionMiddleware", "HumanInTheLoopMiddleware", "SubAgentMiddleware",
"McpMiddleware", "WorkflowMiddleware", "PtcMiddleware", "ToolSearch", "LspMiddleware",
"GoalMiddleware"
```

迁移后逐字为（删 `CronMiddleware` / `LspMiddleware`，增 `LspSyncMiddleware`）：

```text
"DefaultSystemPromptMiddleware", "LangMiddleware", "AgentsMdMiddleware",
"AgentDefineMiddleware", "PluginMiddleware", "SkillsMiddleware", "SkillPreloadMiddleware",
"AtMentionMiddleware", "ImageMiddleware", "FilesystemMiddleware", "GitAttributionMiddleware",
"GitWatchMiddleware", "TerminalMiddleware", "TodoMiddleware", "HookMiddleware",
"PermissionMiddleware", "HumanInTheLoopMiddleware", "SubAgentMiddleware", "McpMiddleware",
"WorkflowMiddleware", "PtcMiddleware", "ToolSearch", "LspSyncMiddleware", "GoalMiddleware"
```

`BUILTIN_INSTANCE_POLICY_KEYS`：`["WebMiddleware", "ArtifactMiddleware"]` → `["WebMiddleware", "ArtifactMiddleware", "CronMiddleware", "LspMiddleware"]`。

`MIDDLEWARE_TOOL_NAMES` 只删除 `"LSP"`，cron 三工具不入表；其余条目与顺序逐字不变（`Read`, `Write`, `Edit`, `Glob`, `Grep`, `folder_operations`, `Bash`, `SkillTool`, `DiscoverSkillsTool`, `AskUserQuestion`, `Agent`, `AgentResult`, `Workflow`, `TodoWrite`, `ToolSearch`, `SearchExtraTools`, `ExecuteExtraTool`, `goal`, `DiscoverMCP`, `mcp_read_resource`）。

### 6.2 三条不变式的新形态

`meta_harness::tests::builtin_instance_policy_keys_match_declaration_table` / `middleware_names_and_builtin_policy_keys_are_disjoint` / `known_key_union_covers_builtin_policy_keys` 均保留并强化：① policy 表无重复且等于四实例 `policy_key` 集合，`CronMiddleware`/`LspMiddleware` 不得回到槽位表；② 两表交集为空，`LspSyncMiddleware` 在槽位表；③ 并集覆盖四实例，且槽位名/链名/静态工具名断言不把 `cron_register`/`cron_list`/`cron_remove`/`LSP` 当 middleware 工具。

### 6.3 归一消费点（含 F9）

| 点位 | 处置 |
| --- | --- |
| permission 判定 | `default_requires_approval` / `is_edit_tool` 判定型归一，四个 effective name 与裸名逐项等价；未知 `mcp__*` 保守语义不变。 |
| **permission 模型文本（F9）** | `peri-middlewares/src/permission/mod.rs` 的 `sensitive_tool_entries()` 当前仍写裸名 `cron_register`（`:203`）且该表直接生成模型可见 Markdown（`format_sensitive_tools` `:211`）。**保留判定规则 / 14 项 / 顺序**，但显示名改为从注册表解析 effective name（范式：同文件 `builtin_tool_effective_name` `:116` 已用于 web 两工具）。owner：S-01；S-05 同步文本。 |
| subagent | `is_mutation_tool` 与 session 工具视图对 effective register/remove/LSP 与裸名等价；`parent_tools` / workflow 工具面按注册表 policy key 关闭，不按前缀硬编码。 |
| hooks | `peri-middlewares/src/hooks/matcher.rs` 当前已是匹配型「原样优先、未命中再归一」；本波只加 cron/lsp probe，不改事件名。 |
| TUI | `peri-tui/src/kit/tool_display.rs` 与 `peri-tui/src/truncate.rs` **已归一**（F5，见 §7），本波不改改造逻辑。 |
| 事件投影 | `peri-acp/src/event/tool_projection.rs` 只在推导 `ToolKind` 时归一；payload/transcript 保留 effective name；`mcp__lsp__LSP` 与 `LSP` 分类一致。 |
| 参数展示 | `peri-agent/src/tools/invocation.rs::TOOL_PARAM_ALIASES` 无 LSP 条目且已有原样优先 fallback，不新增伪 alias；`ToolFilterPolicy::canonical` 同一规则。 |
| 搜索 | `ToolIndex`/`SearchExtraTools`：每工具满足裸名 XOR effective name；`ExecuteExtraTool` 只接受迁移后 effective name；摘要面因 `mcp__` 过滤整体缺席，不作等价判据（W0 命中函数：`wave2_baseline_first_request_and_deferred_summary` / `wave2_baseline_lsp_tool_visible_when_server_configured`）。 |

## 7. S-03 TUI：只做回归断言（F5）

- `peri-tui/src/kit/tool_display.rs` 与 `peri-tui/src/truncate.rs` **已**经 `original_tool_name_of_effective` 实现「原样匹配失败后重试」；**本波不新增归一改造**，删除一切「TUI 需新增归一」表述。
- 变更范围仅限测试：`peri-tui/src/kit/tool_display_test.rs`、`peri-tui/src/truncate_test.rs` 在 C-01 注册表新增 cron/lsp 条目后补回归断言——四个 effective name 与裸名分支产生相同摘要 / 截断；未知 `mcp__lsp__Other` 仍走通用分支；源码无 `mcp__cron__`/`mcp__lsp__`/`mcp__artifact__` 硬编码前缀。
- LSP 同步的展示面不因改名退化（`operation` 截断仍走 LSP 分支），由上述回归覆盖。

## 8. S-05：文档与索引

| 范围 | 更新要点 |
| --- | --- |
| `docs/code-index/peri-acp-types.md:119-126` | 声明表新增四实例/四 effective name；两表迁移；归一 helper 消费点增加 cron/lsp。 |
| `docs/code-index/peri-middlewares.md:28-30` | builtin 路由（`dispatch.rs`）、A32 tick/supervisor 生命周期、关闭四面与槽位表，标明 H-01 单 owner。 |
| `docs/code-index/peri-agent.md:90-101` | 蓝本删 Cron 槽位、tool catalog 与参数归一的现状说明。 |
| `docs/code-index/peri-acp.md` | host seam：host 级 LSP pool、`run_initialize` 前的 context 注入（A33）、宿主 CronTick 删除。 |
| `docs/reference/mcp-ecosystem.md:134-141,678-715` | 含 `cron`/`lsp` 的 builtin 现状、四个 effective name、配置门控、关闭四类（F8）、多 cwd 退化与 wave 3 依赖。 |
| `docs/design/middleware-system.md:107-148` | 槽位表删 Cron、LSP 改 sync，说明两表分离与 `LspSyncMiddleware`。 |
| `peri-middlewares/src/skills/builtin/skills/cron/SKILL.md:13,23-29,57` | 裸名调用改 effective name，写明审批/搜索/关闭语义与 IF-D14 错误文本退化。 |
| `CLAUDE.md:48-70`、`peri-middlewares/CLAUDE.md:5,24-25,37-39,59-61`、`peri-acp/CLAUDE.md:5,29,32` | builtin 唯一适配器、host LSP pool/单 cwd、无 `HostTaskKind::CronTick`、策略关闭 ≠ 物理销毁、A32 supervisor 关闭顺序与验证过滤器。 |

关闭语义矩阵（用户配置键；**只列策略面，物理/注入面分开写**）：

| 操作 | 工具可见性 | LSP 同步目标 | cron tick | readiness | 物理生命周期 |
| --- | --- | --- | --- | --- | --- |
| `CronMiddleware:false` | cron 工具关闭 | 不适用 | 保持运行 | 保持 1R ready | 不销毁实例/scheduler/supervisor |
| `LspMiddleware:false` | LSP 工具关闭 | 关闭 | 不适用 | 保持 1R ready | 不销毁 host LSP pool |
| `LspSyncMiddleware:false` | LSP 工具仍可见 | 仅同步关闭 | 不适用 | 不变 | 不关闭 host LSP pool |
| MCP 配置 `disabled:true` | 该实例不连接 | 不适用 | 不 spawn | 不参与 readiness | 注册为 `Disabled` |
| `PERI_MCP_BUILTIN=off` | 四实例零注入 | 不适用 | 不 spawn | 无 builtin ready 要求 | 无 builtin 对象 |
| 物理 close / host shutdown | 全部关闭 | 全部停止 | tick cancel + join | host 结束 | transport/supervisor/language server 有界关闭 |

S-04 先出逐文件清单，S-05 落地；acceptance 只追加现场证据。

## 9. 改动面与路径清单（F11：全部 repository-relative 完整路径）

**本波必须改（H 相关，含 A32/A33 新增点）**

- builtin 行为层：`peri-middlewares/src/mcp/builtin/dispatch.rs`（新增）、`peri-middlewares/src/mcp/builtin/dispatch_test.rs`（新增）、`peri-middlewares/src/mcp/builtin/web.rs`、`peri-middlewares/src/mcp/builtin/web_test.rs`、`peri-middlewares/src/mcp/builtin/artifact.rs`、`peri-middlewares/src/mcp/builtin/artifact_test.rs`、`peri-middlewares/src/mcp/builtin/mod.rs`、`peri-middlewares/src/mcp/builtin/runtime.rs`、`peri-middlewares/src/mcp/builtin/runtime_test.rs`
- C/L 新增模块：`peri-middlewares/src/mcp/builtin/cron.rs`、`peri-middlewares/src/mcp/builtin/cron_test.rs`、`peri-middlewares/src/mcp/builtin/lsp.rs`、`peri-middlewares/src/mcp/builtin/lsp_test.rs`
- MCP 池/初始化：`peri-middlewares/src/mcp/client.rs`、`peri-middlewares/src/mcp/client/lifecycle.rs`、`peri-middlewares/src/mcp/initialize.rs`、`peri-middlewares/src/mcp/reconnect.rs`
- MCP 层测试（**位于 `peri-middlewares/src/mcp/`，不在 `mcp/builtin/`**）：`peri-middlewares/src/mcp/builtin_apply_test.rs`、`peri-middlewares/src/mcp/builtin_runtime_test.rs`
- 契约层：`peri-acp-types/src/builtin_mcp.rs`、`peri-acp-types/src/builtin_mcp_test.rs`、`peri-acp-types/src/meta_harness.rs`、`peri-acp-types/src/ports.rs`
- 装配面：`peri-middlewares/src/assembly.rs`、`peri-middlewares/src/assembly/lsp.rs`、`peri-middlewares/src/assembly/mcp.rs`、`peri-middlewares/src/assembly_test.rs`、`peri-agent/src/session/factory.rs`
- **旧类型 re-export（F11 ⑤）**：`peri-middlewares/src/lib.rs`（`:73` `CronMiddleware`、`:78` `LspMiddleware`）、`peri-middlewares/src/lsp/mod.rs`（`:5` `pub use middleware::LspMiddleware`）
- 中间件删除/改造：`peri-middlewares/src/cron/mod.rs`、`peri-middlewares/src/cron/middleware.rs`（删除）、`peri-middlewares/src/lsp/middleware.rs`、`peri-middlewares/src/lsp/mod.rs`、`peri-middlewares/src/lsp/formatters.rs`、`peri-middlewares/src/permission/mod.rs`
- LSP 实现侧（端口合流）：`peri-lsp/src/pool.rs`、`peri-lsp/src/pool_test.rs`
- host：`peri-acp/src/host/assemble.rs`、`peri-acp/src/host/mod.rs`、`peri-acp/src/host/task_scope.rs`、`peri-acp/src/host/shutdown.rs`、`peri-acp/src/host/prompt.rs`、`peri-acp/src/host/requests/session_lifecycle.rs`、`peri-acp/src/host/requests_test.rs`（mock + session/delete 断言）、`peri-acp/src/host/stdio/run_server_integration_test.rs`
- 事件与 TUI 测试：`peri-acp/src/event/tool_projection.rs`、`peri-acp/src/event/mapper_test.rs`、`peri-tui/src/kit/tool_display_test.rs`、`peri-tui/src/truncate_test.rs`
- 集成测试：`peri-middlewares/tests/mcp_isolation_contract.rs`
- 文档：`docs/code-index/peri-acp-types.md`、`docs/code-index/peri-middlewares.md`、`docs/code-index/peri-agent.md`、`docs/code-index/peri-acp.md`、`docs/reference/mcp-ecosystem.md`、`docs/design/middleware-system.md`、`peri-middlewares/src/skills/builtin/skills/cron/SKILL.md`、`CLAUDE.md`、`peri-middlewares/CLAUDE.md`、`peri-acp/CLAUDE.md`

**审计后可不改（须在 acceptance 记「已核对 + 依据」）**

- `peri-acp/src/provider/config.rs`：known-key 并集消费方，表迁移后自动成立。
- `peri-acp/src/session/frozen.rs`：MetaHarness 冻结投影，不改形状。
- `peri-middlewares/src/mcp/middleware.rs`：只消费 `closed_instances`，判定不变。
- `peri-middlewares/src/mcp/tool_bridge.rs`：`(original, effective)` 双名与 `TOOL_CALL_TIMEOUT` 已就位。
- `peri-middlewares/src/mcp/system_tools.rs`：空数组语义已支持。
- 四个 `drive_cron_tick` 调用者：`peri-acp/src/host/stdio/mod.rs:150`、`peri-acp/src/host/workspace.rs:74`、`peri-tui/src/cli_print.rs:157`、`peri-tui/src/launch.rs:188`（字段保留，仅投影为 `tick_enabled`）。

## 10. 测试清单与验证命令纪律（F3/F4）

| 具名测试 | 断言可观察量 | 验收 |
| --- | --- | --- |
| `mcp::builtin::dispatch::tests::all_registered_instances_have_handler`（新增） | 四实例都有 handler 分支；未知实例 `None` | §8-20 |
| `mcp::builtin::dispatch::tests::call_tool_uses_shared_result_mapping`（新增） | IF-D14 三种映射仍唯一 | §8-22 |
| `mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances`（既有扩展） | 两实例 ready；注入先于 `run_initialize` | §8-2 |
| `mcp::builtin_runtime_tests::builtin_context_duplicate_is_typed_error`（新增） | 重复/并发注入恰一成功，首个 Arc 保留 | §8-2/15 |
| `mcp::builtin_runtime_tests::policy_close_five_dimensions`（新增） | 五维矩阵 + 关闭四类分开 | §8-10 |
| `mcp::builtin::cron::tests::cron_server_maps_three_tools_without_tick`（已落地 `c99847f2`；A32 后 tick 不在 handler） | handler 只持工具面、无 tick 驱动；`tick_enabled=false` 由代监督者侧建模 | §8-7 |
| `mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers` / `tick_reconnect_has_single_driver_per_interval`（已落地 `c99847f2`，原计划名 `tick_shutdown_and_reconnect` 已拆分作废） | `supervisor.close` 先 tick join；2×interval 无新 tick；重连后单驱动 | §8-7/15 |
| `mcp::builtin::runtime::tests::supervisor_close_orders_tick_before_server_task`（新增） | `BuiltinCloseOutcome` 两侧结果 + 顺序 | §8-15/19 |
| `assembly::tests::production_chain_has_only_lsp_sync_slot`（新增） | 无 Cron 槽位；LSP 槽位只有 sync | §8-21 |
| `assembly::tests::lsp_pool_port_injected_registers_middleware`（既有扩展） | host pool 同一 Arc；仅 sync 入链 | §8-8/10 |
| `assembly::tests::middleware_names_match_production_blueprint`（既有） | 槽位/链名/静态工具新契约 | §8-9/24 |
| `meta_harness::tests::builtin_instance_policy_keys_match_declaration_table`、`middleware_names_and_builtin_policy_keys_are_disjoint`、`known_key_union_covers_builtin_policy_keys`、`cron_tools_are_not_middleware_static_tools`（后一新增） | 两表不变式 | §8-9/24 |
| `host::mcp_v4_wave2::lsp_handler_constructed_after_config_merge`（新增） | 合并先于 pool/handler；无配置空工具 | §8-5 |
| `host::mcp_v4_wave2::cron_register_tick_approval_continuation`（新增） | effective register → tick → 审批 → `QueuedMessage(MessageSource::CronTrigger)` | §8-6 |
| `host::mcp_v4_wave2::reconnect_has_single_tick_driver`（新增） | 旧 supervisor 收敛、新代单驱动；**范围仅限 ACP 共享 scheduler 的驱动任务**（F10） | §8-7 |
| `host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result`（新增） | didChange→didSave 顺序、失败降级、工具结果不变 | §8-8 |
| `host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown`（新增） | 单 cwd 等价 / 多 cwd 退化 / shutdown 无 orphan | §8-13 |
| `host::shutdown::tests::host_shutdown_closes_unique_lsp_pool`（新增） | 只关一次、不遍历 session pool | §8-13/15 |
| `permission::tests::builtin_effective_names_match_original_name_policy`（新增） | 判定等价 + 模型可见文本用 effective name（F9） | §8-11/17 |
| `subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names`（新增） | mutation 等价 | §8-11 |
| `kit::tool_display::tests`、`truncate::tests`（既有 + 回归） | cron/lsp 条目回归（F5） | §8-11/12 |
| `peri-middlewares/tests/mcp_isolation_contract.rs::instances_have_independent_transport_task_and_state` | transport/task/state 独立；off 零注入 | §8-15 |

命令纪律（F3/F4）：

- 单元测试：`cargo test -p <crate> --lib -- <精确模块/函数过滤器>`；过滤器必须命中**真实存在或明确标注「新增」**的测试函数，不得用未实现的名字凑数。
- 集成测试：`cargo test -p peri-middlewares --test mcp_isolation_contract -- mcp_isolation_contract::instances_have_independent_transport_task_and_state`（`--test <target>` 指定目标，目标名不作过滤前缀）。
- `0 tests` 与「只命中旧用例」都判失败；每条验收行须在 acceptance 记录命令、exit、`test result:`、passed 计数与新/改函数名。

## 11. 施工顺序 + 提交切分

1. **契约/台账预审（先于 C/L）**：核对主计划 IF-P3-03/04/07/10/11/12、W0 三面基线、owner 与本轮 A32/A33；S-04 出逐文件清单。验证：`git diff --check -- spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-h-host-assembly.md`。提交：本 sub-plan 单独一提交。
2. **H-01 dispatch 搬迁（先于 H-05）**：搬 enum/factory/impl、收敛 `web.rs`、迁测试。验证：`cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::all_registered_instances_have_handler`。提交：H-01 单独提交。
3. **A32 supervisor 先行（与 C-02 协同）**：在 `peri-middlewares/src/mcp/builtin/runtime.rs` 落 `TickGuard` / `BuiltinInstanceSupervisor` / `BuiltinCloseOutcome` / `BuiltinTransport.tick`+`into_parts`，同步 `peri-middlewares/src/mcp/client/lifecycle.rs` 的 `close_builtin_task`/`close_builtin_tasks` 与 §3.2 机械清单。验证：`cargo test -p peri-middlewares --lib -- mcp::builtin::runtime::tests::supervisor_close_orders_tick_before_server_task`。提交：A32 类型 + 收敛路径单独提交（C-02 消费其 API）。
4. **H-02 上下文与 pool 方法**：context 类型、一次性 setter、`McpClientPool::spawn_builtin_transport` 承接 spawn、initialize/reconnect 改调用；F1 缺口（builtin helper 的 `ToolContext::new(&[], cwd)`）随 C-01/H-05 接线修正。验证：`cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances`。提交：H-02 单独提交。
5. **H-03 装配与两表**：删 Cron 槽位、LSP 槽位只装 sync、context 类型经 `assembly` 暴露（**注入点不在 `assembly/mcp.rs`**）；S-02 只改 `peri-acp-types/src/meta_harness.rs`，H-03 单写 `peri-middlewares/src/assembly_test.rs`。验证：`cargo test -p peri-middlewares --lib -- assembly::tests::production_chain_has_only_lsp_sync_slot`；`cargo test -p peri-acp-types --lib -- meta_harness::tests::builtin_instance_policy_keys_match_declaration_table`。提交：H-03 与 S-02 分提交。
6. **H-05 + H-04 宿主接线**：H-05 接 cron/lsp 变体；H-04 做 §5.1/§5.2（A33 注入早于 `run_initialize`、删 CronTick、host pool、session/prompt/shutdown）。验证：`cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_handler_constructed_after_config_merge`；`cargo test -p peri-acp --lib -- host::mcp_v4_wave2::reconnect_has_single_tick_driver`。提交：H-05、H-04 分别提交。
7. **S-01/S-03 归一与回归**：permission（含 F9 模型文本）→ subagent/hooks/event/search → TUI 回归（F5，仅测试）。验证：`cargo test -p peri-middlewares --lib -- permission::tests::builtin_effective_names_match_original_name_policy`；`cargo test -p peri-tui --lib -- kit::tool_display::tests`。提交：S-01 与 S-03 分提交。
8. **S-05 文档落地与收口**：索引/参考/design/skill/`CLAUDE.md` + acceptance 证据；执行主计划 §8 全量矩阵与 `scripts/check-layer-imports.sh`。提交：S-05 单独提交。

## 与代码冲突 / 未闭合项

1. **基线 HEAD**：目标 worktree `git rev-parse HEAD` = `a81e0ba66c014f8a05ae05298f9eee9f94e6ffaf`，其父为 `b1651aee`；正文所述 `b1651aee` 与当前 HEAD 不一致，实施前须确认（本文不改动仓库状态）。
2. **K1 关闭集承载**：A33 把「关闭集」列入实例上下文，而主计划 IF-P3-04 冻结字段清单只有 `cwd` / `cron` / `lsp`。需主计划补字段或指定承载位置，否则 §3.1 与 IF-P3-04 存在形状冲突。
3. **K2 模块挂载原子性**：`peri-middlewares/src/mcp/builtin/mod.rs` 挂载 owner 是 H-02，但 C-02/L-01 的模块测试需要先挂载才能跑；C-04 删 `peri-middlewares/src/cron/middleware.rs` 与 H-03 删 import/槽位也须原子。需主计划 owner 确认原子提交边界，禁止用 `0 tests` 或 dead-code 豁免绕过。
4. **K3 A32 计数不符**：A32 称 `close_builtin_task` 有「2 处消费点」，现场核实为生产 5 处 + 测试 4 处（§3.2 第 6 条逐点列出）；以主 agent 复核为准，本文按现场清单施工。
5. **K4 旧 re-export owner**：`peri-middlewares/src/lib.rs:73,78` 与 `peri-middlewares/src/lsp/mod.rs:5` 需随 C-04/L-03 删除 `CronMiddleware` / `LspMiddleware` re-export，但主计划 §4 未把这两个文件列入 owner 行；需补登记。
6. **K5 host 字段 owner**：host LSP pool / builtin context 落在 `peri-acp/src/host/mod.rs` 的 `AcpServerConfig` 侧；`peri-acp/src/host/requests_test.rs` 与 `peri-acp/src/host/stdio/run_server_integration_test.rs` 需同步 mock/断言，但主计划 §4 未列这两个测试文件的测试挂载；需补登记。
7. **K6 timeout 不对称（F2）**：MCP bridge 外层 `TOOL_CALL_TIMEOUT = 120s`（`peri-middlewares/src/mcp/tool_bridge.rs:52,274`）包裹调用，而 `LspTool::timeout()` 返回 `None`（`peri-middlewares/src/lsp/tool.rs:266-268`）；因此「复用 LspTool」**不等价**于端到端 timeout 语义等价。须在 acceptance §3 语义变更登记并在 §8-8/§8-23 记录该差异，不得宣称等价。
8. **K7 tick 验收范围（F10）**：TUI 仍有私有 scheduler + tick（`peri-tui/src/app/cron_state.rs:26`、`peri-tui/src/app/mod.rs:97-99`）。「宿主无 tick task」只指驱动 MCP 所用共享 scheduler 的 ACP Startup/`HostTaskKind::CronTick` 任务；**禁止**用「全仓 grep 零 tick」作验收。
9. **K8 字段级 context 清单**：`lsp.pool` 的具体类型（`Arc<LspServerPool>` vs `Arc<dyn LspPoolPort>`）在 IF-P3-04 冻结为前者、宿主投影为后者；若实施需要在 context 内落端口对象，须先追加裁决。
10. **既有工作树**：`git status --porcelain` 显示主计划已修改、C/L/V 三份同波 sub-plan 未跟踪；实施/提交只暂存本 sub-plan 或明确授权的 owner 文件，不覆盖既有改动。
