# sub-plan E：Builtin MCP 运行时

> 属于 `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-plan.md`（下称「主计划」）的 wave 1。owner task 前缀：**E-**（E-01 / E-02 / E-03）。
>
> 主计划 §5 对本文件有覆盖登记（R1/R2/R3/R6/R10/R11/R12/R13/R14/R16/R18 **+ 本轮新增 R20/R22/R30/R31**）；与主计划冲突处以主计划为准。本文件是 builtin 运行形态的**唯一实现细节权威**，但不得放宽主计划的冻结接口。
>
> 设计前提来自工作区未提交的 spike：`peri-middlewares/src/mcp/builtin_spike_test.rs`（691 行，已挂在 `peri-middlewares/src/mcp/mod.rs` 的 `builtin_spike_tests` 模块下）。**不得**在本批次删除或改写该 spike；它同时是 W0 闸门与回归证据。

## 0. 裁决记录 A1–A19 落点（复核用索引）

| 裁决 | 本文件落点 |
| --- | --- |
| **A1** 注入点唯一（step 6.5） | 头部引语；§4.7「注入点」（含「不存在第二个注入点」的显式声明）；§4.4 明确 `initialize_config` 只消费 loader 结果；§5 S4；§7 `config_test.rs` 行 |
| **A2** `PERI_MCP_BUILTIN` 语义 | §4.7「策略来源」+ 覆盖规则的策略类型；§8 第 5 条（不做的事）；§4.7 末条（off 的能力面后果）；§6 失败路径表的 off 行 |
| **A3** 保留实例名 | §4.1（`BUILTIN_RESERVED_INSTANCE_NAMES` + `is_reserved_instance_name`）；§4.7 覆盖规则 3；§6 失败路径新增行；§5 S1/S2 断言 |
| **A4** 生效名归一原则 | §4.1（helper 归 `peri-acp-types`，本文件只提供 `effective_tool_name` 与字面量对齐）；§9 仲裁段；消费者（S-01/S-02/S-08）不在本文件 |
| **A5** 直连性声明（IF-D13） | §4.1（`BuiltinMcpTool::direct`、`declared_direct_tools`、`is_declared_direct`）；§4.7 覆盖规则 1/2（`system_mcp_tools` == 声明 direct 集合）；§5 S2 断言 |
| **A6** 三个工具面 | §4.1（关闭集下游三个面 + workflow 面）；§4.3 第 3 条；§6 表；§9（本文件不持有 `assembly/*`，落点归 I-03） |
| **A7** 两张名单 | §4.1（`closed_instances` 的 `policy_key` 合法性来自 `BUILTIN_INSTANCE_POLICY_KEYS`）；§4.7 规则注；§5 S2 断言 |
| **A8** TUI | 不涉及（归 S-08；本文件不改 `peri-tui`） |
| **A9** 声明段保留 | §4.1（`BuiltinMcpTool::prompt_declaration` + `builtin_prompt_declaration`）；§8 第 6 条；§9（断言归 S-02） |
| **A10** V-03 夹具 | 不涉及（归 V-02/V-06） |
| **A11** 隔离夹具 env | §7 `mcp_isolation_contract.rs` 行（只复跑既有断言；夹具 env 改动由 V-03 承担） |
| **A12** 基线证据 | 不涉及（归 V-06；本文件不含 host 侧断言） |
| **A13** 隔离断言 | §6 表（可观察项）；§8 第 7 条（capability root / 凭据不可证伪 → UNVERIFIED） |
| **A14** 任务号与过滤器 | §4.8（模块名与精确过滤器）；§5 全部命令；旧装配编号统一为 `I-03` |
| **A15** `call_tool` 映射（IF-D14） | §4.4（builtin 分支须与 stdio 同构、失败形态归 I-01）；§9（IF-D14 owner I-01，本文件只消费） |
| **A16** duplex 容量语义 | §3 第 6 行（重写）；§4.3 `BUILTIN_DUPLEX_BUF` 注释要求；§5 S3 断言⑨（大 payload） |
| **A17** `{"web": {}}` 语义 | §4.7 覆盖规则 2 |
| **A18** 关闭片段形状 | §4.7 覆盖规则 5（唯一合法片段 + 反例） |
| **A19** 主计划唯一裁决源 | 头部引语；§8 各条（S-01/S-02/S-08/V-05 的归属不在本文件）；§9 仲裁段 |

## 1. 目标与范围

**目标**：让「Builtin MCP 实例」成为与 stdio / HTTP 平级、走**同一条**连接链路的传输形态，从而 `system_mcp` / readiness / `system_mcp_tools` / direct 提升 / 状态面板全部复用既有语义，不需要任何旁路。

**范围（本文件负责）**：

1. `TransportConfig::Builtin` 与三分类连接超时（IF-D1）。
2. `ConfigSource::Builtin { instance }` 的传播面（IF-D2）。
3. builtin 注册表的**行为侧**：实例解析、effective name 计算、注入策略、关闭集（IF-D4 / IF-D5 / IF-D10 的判定部分）。
4. 同进程 server 的 spawn / 生命周期 / 关闭 / 重连归属（IF-D12）。
5. `initialize_config` / `reconnect` 的 builtin 分支与失败收口。
6. 默认配置层的注入点、优先级算法与写回隔离（IF-D3）。
7. `transport_type` 三分类（IF-D11）。
8. 上述行为的 crate 内证据（`mcp::builtin::tests` / `mcp::builtin::runtime` / `mcp::builtin_apply`）。**注意**：`mcp::builtin` 是**禁用过滤器**（会命中 `builtin_spike_tests`，主计划 §9 规则 4）。

**范围（其他 owner，本文件只描述接口）**：两个 builtin server 的工具实现归 `I-01`（sub-plan F）；策略一致性归 `S-01`；投影同步归 `S-02`；链与挂载点删除归 `I-03`；启动路径 / 审批链 / 关闭矩阵的端到端证据归 `V-01`（sub-plan H）。

## 2. 事实基线（已核实，逐条可复核）

| 事实 | 位置 |
| --- | --- |
| `TransportConfig` 只有 `Stdio` / `StreamableHttp`，**无** in-process 分支；`TryFrom<&McpServerConfig>` 会先 `config.validate()?` 再按 `(command, url)` 二选一 | `mcp/transport.rs:10-22`、`:35-55` |
| 穷尽 `match` 会在新增变体时编译失败 | `mcp/initialize.rs:275`（臂 `:276` / `:299`）、`mcp/reconnect.rs:90`（臂 `:91` / `:120`） |
| 二元 `is_http` 判定（新变体会被当成 stdio，走 10 s 超时） | `mcp/initialize.rs:262`、`mcp/reconnect.rs:76`；常量 `mcp/client.rs:119-121` |
| `serve_client_auto` 与 transport 枚举**无耦合**，只要求 `T: IntoTransport<RoleClient, E, A>` | `mcp/client/transport.rs:18-57` |
| 连接建立链（builtin 必须逐段复用） | `initialize.rs:117 run_initialize` → `:127 load_merged_config_full` → `:134 initialize_config` → `:157 validate_config` → `:161 bind_execution_cwd` → `:173-177` 可连接计数 → `:178-185` 空集合短路 → `:197-201` 写 `pool.configs` → `:204` 发布 `SystemMcpManifest` → `:217-224` system 优先排序 → `:227` admin 循环 → `:253 TransportConfig::try_from` → `:275` 分派 → `:345 retain_service`（`client/lifecycle.rs:59`）→ `:357 list_discovered_tools`（`initialize.rs:32`）→ `:388-403` 构造 `McpClientHandle` → `:405 try_commit_connection`（`client/lifecycle.rs:214`）→ `:413 commit_discovery_success`（`:47`） |
| stdio 传输的 spawn 实现在 pool 上，且需要已绑定的执行目录 | `mcp/client/process.rs:108-115`（`spawn_stdio_transport`）；`initialize.rs:280`；reconnect 路径未绑定 cwd 时返回 `ConnectionFailed`（`reconnect.rs:92-98`） |
| `pool.configs` 是「System 依赖事实源」，`system_requirements()` 只认 `system_mcp == Some(true)` | `readiness.rs:349-363` |
| 配置层禁用的 system server 是**立即 fatal**，不是「等待」 | `readiness.rs:466-479`（`Err(SystemReadinessError::Disabled)`） |
| 状态面板的 `transport_type` 由 `url.is_some()` 二元推断（三处） | `client/status.rs:172`、`:203`、`:224` |
| 集成断言期望 `"stdio"`（其夹具是两个真实 stdio 子进程） | `peri-middlewares/tests/mcp_isolation_contract.rs:382-383` |
| `ConfigSource` 唯一穷尽 `match` 在 discover 工具的来源标签 | `discover_tool.rs:331-335` |
| `source` 字段是 `#[serde(skip)]`（用户配置无法构造 `Builtin` 变体） | `peri-acp-types/src/plugin.rs:108-110` |
| `server_config_hash` 覆盖 command/args/env/protocolVersion/system 三键；`manual_hashes` 由 global+project 构造，插件条目与之相当且**非 system** 时被删 | `mcp/config.rs:151-187`、`:388-407` |
| loader 的消费方只有生产路径与一个 `#[cfg(test)]` helper | `initialize.rs:127`、`initialize.rs:508-535`（`McpClientPool::initialize`）、`config.rs:443`（公开 `load_merged_config`） |
| 两个写回函数只改**磁盘上已存在**的条目，不会把内存条目写盘 | `mcp/config.rs:586-701`（`set_server_disabled_with_paths`）、`:478-…`（`remove_server_from_config`）；生产无调用方（仅 `mcp/mod.rs:44` 再导出） |
| spike 的夹具模式（真实 server + 生产 client + 线路级观测 + 有界收尾） | `builtin_spike_test.rs:131-176`（`MethodTap`）、`:313-326`（`spawn_builtin_server`）、`:329-377`（`connect`）、`:299-308`（`shutdown`） |
| spike 挂载点 | `mcp/mod.rs` 末 6 行（`#[cfg(test)] #[path = "builtin_spike_test.rs"] mod builtin_spike_tests;`） |

## 3. spike 结论 = 本文件的设计前提

| 结论 | 证据 | 对设计的强制含义 |
| --- | --- | --- |
| **Q1(a)** 对端是真实 `ServerHandler` 且 `discover` 用 rmcp 默认实现时，Auto lifecycle 走 **modern**（线路只有 `server/discover`，协议 `2026-07-28`），`peer_info()` 为 `Some` | `builtin_spike_test.rs:471-525` | builtin server **不得**覆写 `discover`；`initialize.rs:376-392`（改后 `:376-385`）读取的 `capabilities.experimental["claude/channel"]` 与 `peer_info().server_info.version` 在 modern 路径下仍可读 |
| **Q1(b)/Q3(b)** 若把 `discover` 覆写为 `-32601` 逼 Auto 回退 legacy，握手本身「成功」但同一连接上的 `tools/list` 会被 `-32602`（缺 per-request `_meta`）拒绝，且 `tools/list` **根本没到 handler** | `builtin_spike_test.rs:529-564`、`:634-680` | ① 硬约束：不覆写 `discover` ② 反面教材：`initialize_test.rs:3-25` 的 node 夹具（拒绝 `discover` 逼 legacy）**不得**用于 builtin 路径的验证，否则会掩盖此风险 ③ builtin 的 `tools/list` 必须有一条线路级断言（server 侧确实收到帧） |
| **Q2** client `close_with_timeout` 之后，同进程 server task 靠 duplex EOF **自然收敛**（`Ok(Ok(QuitReason::Closed))`），**无需 abort** | `builtin_spike_test.rs:572-616` | 关闭正常路径不 abort；仅在有界等待超时才 abort（沿用 `:299-308` 的模式） |
| 对照实验：从不发 `server/discover`、直接 legacy `initialize` 的连接工具发现**可用** | `builtin_spike_test.rs:689-691` | 失败根因是「探测被拒后同连接回退」，不是 legacy 协议本身；排障文档与错误分类必须据此区分（避免把 modern 失败一律归因于「MCP 版本不兼容」） |
| duplex 缓冲的语义（**A16 改写**）：capacity 只影响**背压**，不是单帧上限；单帧大小不受它约束 | `builtin_spike_test.rs:27-29`、`:65` | 冻结 `BUILTIN_DUPLEX_BUF = 8 * 1024` 并**禁止**再把注释写成「8 KiB 足够」；大结果（WebFetch 级正文，数十~数百 KB）必须有一条端到端用例（§5 S3 断言⑨），不得以「缓冲够用」推断其可用 |

## 4. 设计

### 4.1 术语与注册表

- **builtin 实例**：一个由 Peri 自己实现、跑在同一进程内的 `rmcp::ServerHandler`，配一个独立 duplex 通道、独立 `Arc<McpClientHandle>`、独立 pool entry。
- 实例的「身份」有三个等价值，必须始终一致：配置 key / server name（如 `web`）、`TransportConfig::Builtin.instance`、`ConfigSource::Builtin.instance`。
- **禁止**把 builtin 实例做成「同一 pool 内多个 server 共用一个 transport 或一个 handler 实例」：隔离契约要求每实例独立 transport 与独立状态（设计文档 §「MCP 运行形态」末段）。

注册表（数据，`peri-acp-types/src/builtin_mcp.rs`）：

```rust
pub struct BuiltinMcpTool {
    pub original_name: &'static str,     // "WebSearch"（配置 / readiness / system_mcp_tools 侧精确匹配）
    pub effective_name: &'static str,    // 冻结字面量（IF-D5），必须与 effective_tool_name() 输出逐字相等
    pub direct: bool,                    // IF-D13（A5）：Web 两工具 / Artifact = true；Cron/LSP 后续波次 = false
    pub prompt_declaration: Option<&'static str>, // A9：逐字搬运既有模板（web_fetch.rs:106-111 等）
}
pub struct BuiltinMcpInstance {
    pub name: &'static str,             // "web" / "artifact"
    pub instance: &'static str,         // 与 name 同值；独立字段以便将来重命名 server 而不改身份
    pub policy_key: &'static str,       // "WebMiddleware" / "ArtifactMiddleware"（IF-D10 / BUILTIN_INSTANCE_POLICY_KEYS）
    pub tools: &'static [BuiltinMcpTool],
}
pub const BUILTIN_MCP_INSTANCES: &[BuiltinMcpInstance];              // 仅**已实现**实例
pub const BUILTIN_RESERVED_INSTANCE_NAMES: &[&str];                  // A3：web/artifact + 预留 cron/lsp/workspace
pub fn find(instance: &str) -> Option<&'static BuiltinMcpInstance>;  // 只命中已实现实例
pub fn is_reserved_instance_name(name: &str) -> bool;

/// IF-D15（A4）：唯一归一入口，按冻结字面量查表；禁止反拆 `mcp__` 名字、禁止复刻 sanitize。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>;
```

行为侧（`peri-middlewares/src/mcp/builtin/mod.rs`，全部 `pub(crate)`）：

```rust
pub(crate) fn effective_tool_name(instance: &str, original_tool: &str) -> Option<String>;
pub(crate) fn effective_tool_names(instance: &str) -> BTreeSet<String>;   // 关闭集与断言共用
pub(crate) fn declared_direct_tools(instance: &str) -> Option<&'static [&'static str]>; // IF-D13 / A17 一致性
pub(crate) fn is_declared_direct(server_name: &str, original_tool: &str) -> bool;       // 类型化 bridge 构造点
pub(crate) fn builtin_prompt_declaration(server_name: &str, original_tool: &str) -> Option<&'static str>; // A9
pub(crate) fn apply_builtin_overlay(servers: &mut HashMap<String, McpServerConfig>, policy: &BuiltinInjectionPolicy); // 见 §4.7
pub(crate) fn builtin_injection_policy_from_env() -> BuiltinInjectionPolicy; // 只在 load_merged_config_full 调用
pub(crate) fn closed_instances(disabled_middlewares: &HashSet<String>) -> BTreeSet<String>;
pub(crate) fn is_closed(server_name: &str, closed: &BTreeSet<String>) -> bool;
```

- `effective_tool_name` 必须调用 `tool_bridge::sanitize_name_component`（`tool_bridge.rs:56-66`）与 `effective_mcp_tool_name` 的模板（`:95-101`）；**禁止**复制规则；`BuiltinMcpTool::effective_name` 与它的输出必须由测试逐字锁定（规则一份、字面量一份）。
- `is_declared_direct` 只在 `(server_name, original_tool)` 命中**已实现**实例的声明表且 `direct == true` 时返回真；`workspace` 是预留但未实现的名字 ⇒ 恒为 false（`tool_bridge.rs:503-528` 的既有 fixture 用它，因此该锁定测试不受影响）。
- `closed_instances` 是 IF-D10 的**唯一判定入口**；下游过滤面共**四个**（direct 注入 / deferred bridge / parent_tools / **workflow agent 工具面**——A6 后 workflow 面必须显式过滤，不再「天然关闭」），都只调用它，不得各自硬编码前缀或实例名。
- A9 的声明文本只经 `builtin_prompt_declaration` 暴露给 bridge 层；本文件不渲染模板（渲染仍由 `tool_search/declaration.rs` 的既有规则负责）。

### 4.2 身份传播：`ConfigSource::Builtin` → `TransportConfig::Builtin`

```rust
// peri-acp-types/src/plugin.rs
pub enum ConfigSource { Project(PathBuf), Global(PathBuf), Plugin, Builtin { instance: String } }

// peri-middlewares/src/mcp/transport.rs
pub enum TransportConfig {
    Stdio { .. },
    StreamableHttp { .. },
    Builtin { instance: String },
}
impl TryFrom<&McpServerConfig> for TransportConfig { /* 签名不变 */ }
```

`TryFrom` 的判定顺序（冻结）：

1. `config.validate()?`（沿用 `:40`，非法 System 组合不得建立传输）。
2. 若 `matches!(config.source, Some(ConfigSource::Builtin { .. }))` → 返回 `Builtin { instance }`。该分支**优先于** command / url 判定（只有代码能构造该 source；`#[serde(skip)]` 已保证用户配置无法伪造）。
3. 其余沿用现有 `(command, url)` 二选一；`(None, None)` 仍为 `TransportError::InvalidConfig`。
4. 新增错误变体：

```rust
#[error("builtin MCP 实例未注册: {instance}")]
UnknownBuiltinInstance { instance: String },
```

由**消费侧**（`initialize.rs` / `reconnect.rs` 的 builtin 分支）在 spawn 前查注册表，未命中即用该变体构造 `McpPoolError::ConnectionFailed { server, reason }` 并走 `insert_failed` + `commit_discovery_failure(…, false)`。`TryFrom` 自己不查注册表（它没有 pool 上下文，也无需网络/IO）。

超时分类（冻结，替换两处 `is_http`）：

```rust
pub(crate) enum TransportKind { Stdio, Http, Builtin }
// 由 mcp/transport.rs 提供：impl TransportConfig { pub(crate) fn kind(&self) -> TransportKind }
pub(crate) const BUILTIN_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
```

- `initialize.rs:262-267` 与 `reconnect.rs:76-81` 的超时选择改成对该 `kind()` 的穷尽匹配。
- `initialize.rs:443` 附近失败日志的 `transport` 字段值域扩为 `"http" | "stdio" | "builtin"`，不得再用 `if is_http {"http"} else {"stdio"}`。

### 4.3 spawn / 生命周期 / 关闭

新增 `peri-middlewares/src/mcp/builtin/runtime.rs`（`pub(crate)`）：

```rust
pub(crate) const BUILTIN_DUPLEX_BUF: usize = 8 * 1024;
// 注释冻结（A16）：capacity 只影响**背压**，不是单帧上限；单帧大小不受它约束。
pub(crate) const BUILTIN_CONVERGE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

/// 构造一条同进程链路；返回 client 侧 service 包装与 server task 句柄。
pub(crate) fn spawn_builtin_transport(
    instance: &str,          // 已确认存在于注册表
    cwd: &Path,              // artifact 实例的文件解析根；web 忽略
) -> Result<BuiltinTransport, BuiltinSpawnError>;

pub(crate) struct BuiltinTransport {
    pub(crate) io: /* (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) */,
    pub(crate) server_task: BuiltinServerTask,
}
```

要求：

1. **server 端**：`rmcp::serve_server(handler, (read, write))` 在 `tokio::spawn` 中运行（spike `:313-326`）。handler 由 `I-01` 提供（`builtin/web.rs`、`builtin/artifact.rs`）；本文件只负责把 handler 交给 rmcp 并持有 task。
2. **禁止覆写 `discover`**（§3 Q1(b)）。
3. **task 归属**：pool 侧新增 builtin server task 表（与 `client.rs` 的 `services` 同期登记 / 移除，键 = server name）。冻结：wave 1 **不**新增 `McpTaskKey` 变体（主计划 IF-D12 / §5 R11）。
4. **关闭**：pool 关闭或实例被替换（重连 / 覆盖配置 / 取消）时，顺序为 ① client service `close_with_timeout(SHUTDOWN_TIMEOUT)`（`client.rs:121`）② 有界等待 server task（`BUILTIN_CONVERGE_TIMEOUT`）③ 未收敛才 `abort` + `await`。禁止把 `abort` 当正常路径（Q2 证据）。
5. **无 orphan**：pool 关闭后 builtin server task 表必须为空且任务已结束；该事实必须有断言（`is_finished()` 为真，或 abort 后 join 完成）。
6. **不引入进程 / 不读 env / 不写磁盘**：builtin 实例是纯进程内对象；artifact 的网络与凭据面完全由 `I-01` 的 handler 复用既有 `ArtifactClient`（注入式 base url / token），本文件不接触凭据。

### 4.4 `initialize_config` 的 builtin 分支语义

> **A1 前置声明**：`initialize_config`（`initialize.rs:146`）**只消费** loader（step 6.5）产出的有效配置，**不得**在函数体内注入 builtin 条目、**不得**读 `PERI_MCP_BUILTIN` 或任何 env。本节只描述「配置里已出现 `TransportConfig::Builtin` 时」的处理链。

在既有的逐 server 循环（`initialize.rs:227`）内，`TransportConfig::Builtin` 分支必须与 stdio 分支**同构**：

| 阶段 | 行为 |
| --- | --- |
| 实例解析 | 查注册表；未命中 → `insert_failed` + `commit_discovery_failure(…, false)` + `continue`（与 `:253-260` 的 `try_from` 失败同构） |
| cwd | 调 `pool.execution_cwd`；未绑定 → 与 stdio 路径一致的 `ConnectionFailed`（复用 `reconnect.rs:92-98` 的措辞），不 fallback 到进程 cwd |
| spawn | `spawn_builtin_transport(instance, cwd)`；失败 → `insert_failed("builtin 启动失败: …")` + `commit_discovery_failure(…, false)` + `continue` |
| 握手 | `serve_client_auto(io, channel_handler.as_ref(), protocol_version, &pool.capability_profile, BUILTIN_CONNECT_TIMEOUT)`，与 `:282-289` 完全同参（含 `channel_handler` 透传：builtin 与外部实例遵守同一 channel 语义） |
| 成功收口 | `retain_service` → （`subscriptions` 分支保留同构行为）→ `install_peer_cache_version` → `list_discovered_tools`（`:32`：`system_mcp == Some(true)` 走 live `tools/list`）→ 失败即 `fail_tool_discovery` + `continue`（`:357-365`） |
| 句柄 | 与 `:388-403` 同字段构造：`channel_capable` 走 `peer_info()` 读取（modern 路径已由 spike 证明可用）、`skills_capable`、`source = Some(ConfigSource::Builtin{..})`、`url = None`、`version` 取 `server_info.version` |
| 提交 | `try_commit_connection` → `commit_discovery_success`；提交被拒时 `clear_discovery_evidence` + `break`（`:405-410`） |

冻结约束：

- 分支必须写在**同一条循环**里，不得为 builtin 另起一条 spawn 流程（否则 readiness / `system_mcp_tools` / direct 提升 / 面板全部要写第二套语义）。
- `capability_profile` 是 pool 级（`client.rs:111`），builtin 实例与该 pool 内其它 server 共享 profile —— 这是既有语义（pool 级），不得为 builtin 单独协商 capability；accepted 的差异写入 acceptance。
- **禁止**在 builtin 分支走 OAuth 相关代码路径（builtin 无 URL / 无凭据）。

### 4.5 `reconnect` 的 builtin 分支语义

`reconnect`（`reconnect.rs:32`）必须支持 builtin，且语义为「重建一条全新的同进程链路」：

1. 沿用既有前置步骤：`clear_discovery_evidence` → `stop_background(Subscription)` → 取出并关闭旧 service（`SHUTDOWN_TIMEOUT`）→ 记录 `old_status` → 移除 client。
2. builtin 分支：关闭并**移除**旧 server task（§4.3 第 4 条），再 `spawn_builtin_transport` 新链路 → `serve_client_auto`（同 4.4 参数）。
3. 成功收口与 `initialize` 一致：`retain_service` → `install_peer_cache_version` → `list_discovered_tools` → 句柄 → `try_commit_connection` → `commit_discovery_success`（`:224-236` / `:250-272` 同构）。
4. 失败收口：`fail_tool_discovery` + `Err(McpPoolError::ToolDiscoveryFailed)`（与 `:227-235` 同构）；spawn 失败用 `ConnectionFailed`。
5. **禁止**复用旧的 duplex 或旧的 handler 实例（否则「隔离 / 独立状态」在重连后失真）。任务句柄必须换新。

### 4.6 `transport_type` 与面板

```rust
// mcp/client/status.rs
fn transport_type_of(source: Option<&ConfigSource>, url: Option<&str>) -> &'static str {
    match source {
        Some(ConfigSource::Builtin { .. }) => "builtin",
        _ => if url.is_some() { "http" } else { "stdio" },
    }
}
```

- 三个调用点（`:172` handle 行用 `h.source`；`:203` handle 行用 `h.source`；`:224` config-only 行用 `sc.source`）全部改走该 helper。
- 非 builtin 分支必须与今天**逐位一致**：`mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言不得修改，只复跑。
- 新增断言：builtin 实例在 `all_server_infos()` 与 `snapshot()` 中 `transport_type == "builtin"`；`discover_tool` 的来源标签为 `"builtin"`（`config_source_str` 新臂）。

### 4.7 默认配置层注入与优先级算法（冻结：loader step 6.5 overlay）

注入点（主计划 IF-D3，冻结；**A1 唯一表述**）：`peri-middlewares/src/mcp/config.rs::load_merged_config_full_with_paths`，在 step 6 变量展开循环（`:419-430`）结束之后、step 7 `validate_config(&merged)?`（`:433`）之前执行 `crate::mcp::builtin::apply_builtin_overlay(&mut merged.mcp_servers, policy)`。**不存在第二个注入点**：`initialize_config` 不注入、不读 env（§4.4 前置声明）。

选这里的理由（采纳 sub-plan F 的位置论证）：单一「有效配置」事实源——`run_initialize` 与公开 `load_merged_config`（`config.rs:442`）看到同一份结果，不出现「运行时有 builtin、配置 API 没有」的两套语义；同时注入晚于 step 4 的 hash 去重（`:388-407`），builtin 条目不进 `manual_hashes`。

策略来源（冻结）：`BuiltinInjectionPolicy` 为**显式参数**。`load_merged_config_full`（`:301`）读一次 `PERI_MCP_BUILTIN` 并传给 `_with_paths`；`_with_paths` 与 `apply_builtin_overlay` **禁止**读 env（并行测试下不确定）。`config_test.rs` 的 12 处 `_with_paths` 调用点由 I-02 统一补参：断言生产语义的传 `all()`，断言迁移前行为的传 `none()`。**禁止**把生产注入改成「只有 env=on 才注入」的旁路（会让默认路径与测试路径分叉）。

```rust
pub(crate) struct BuiltinInjectionPolicy { enabled: Vec<&'static str> }  // 注册表实例名子集
impl BuiltinInjectionPolicy {
    pub(crate) fn all() -> Self;
    pub(crate) fn none() -> Self;
    pub(crate) fn from_env() -> Self;   // "off" / "0" → none；缺省/其它 → all（未知值 warn + all）
}
pub(crate) fn apply_builtin_overlay(servers: &mut HashMap<String, McpServerConfig>, policy: &BuiltinInjectionPolicy);
```

覆盖规则（冻结，**六条**；**替代 sub-plan F 的 IF-F1 不变式**，见主计划 §5 R3/R14 + 本轮裁决 A3/A17/A18）：

1. 实例名**缺失** → 插入完整 builtin 条目：`command/url/headers/oauth/args/env/subscriptions = None`、`protocol_version = None`（**必须**，否则 Auto 不探测 `server/discover`，见 §3 Q1(a)）、`disabled = None`、`system_mcp = Some(true)`、`system_mcp_tools = Some(<该实例**声明为 direct**的工具原始名>)`、`system_mcp_timeout = None`（缺省 30 s）、`source = Some(ConfigSource::Builtin { instance })`。
2. 实例名**存在**且 `command.is_none() && url.is_none()`：填 `source = Some(ConfigSource::Builtin { instance })`；**`disabled != Some(true)` 时同时填 `system_mcp = Some(true)` 与 `system_mcp_tools = Some(<声明为 direct 的工具原始名>)`**（**A17**：否则 `{"web": {}}` 会从 direct 静默降级为 deferred，与「实例可用」不符）；`disabled == Some(true)` 时**只填** `source`（保持 `Disabled` 注册语义，不构成 system 依赖，不触发 `Err(SystemReadinessError::Disabled)` fatal），其余字段以用户值为准（**不做字段级深合并**）。本规则**仅对已实现实例（`web`/`artifact`）生效**；预留未实现名（`cron`/`lsp`/`workspace`）不写 `command`/`url` 时**不注入任何条目**（否则会构造出 `TransportConfig::Builtin` 解析不到的条目）。
3. 实例名**存在**且声明了 `command` 或 `url`：若是**保留实例名**（`is_reserved_instance_name`，A3）→ **加载期 typed error**（`McpConfigError::ReservedBuiltinInstanceName { name }`，错误文本只含实例名）；**不是**「整条不动 / 用户接管传输」。非保留名不受影响（overlay 不触碰）。
4. overlay 结果必须通过 step 7 的 `validate_config`（`config.rs:99-112` / 调用点 `:433`）；注册表自身非法（工具清单为空 / 名字含非法字符 / `effective_name` 与 `effective_tool_name()` 输出不一致）必须在加载期可见失败，不得降级为空成功。
5. **关闭片段形状（A18）**：唯一合法的用户关闭写法 = `{"web": {"disabled": true}}`（只写 `disabled`）。`{"web": {"disabled": true, "system_mcp": true}}` 组合必须被**加载期拒绝**（typed error）——它在今天会走到 `readiness.rs:466-479` 的 `Err(SystemReadinessError::Disabled)`，阻断**所有** session。必须有反例用例。
6. **direct 一致性（A5/A17）**：每个实例「声明为 direct 的工具原始名集合」== 其 `system_mcp_tools` 集合（`declared_direct_tools` 是唯一来源），必须有断言锁死。

其余要求：

- overlay 结果必须通过 step 7 的 `validate_config`（`config.rs:99-112`）；注册表自身非法（工具清单为空 / 名字含非法字符）必须在加载期可见失败，不得降级为空成功。
- **不新增 `McpServerConfig` 字段**（主计划 §5 R13 否决 sub-plan F 的 IF-F2）：身份由 `source` 承载，`#[serde(skip)]`（`plugin.rs:108-110`）已保证用户配置无法自行声明 builtin。
- 与 config hash：builtin 条目不参与 `server_config_hash`（`:151-187`）。必须有一条测试锁死「注入不改变既有 server 的 hash 去重结果」。
- 与写回：`set_server_disabled` / `remove_server_from_config`（`:586` / `:478`）只改磁盘上已存在的条目；用户条目会正常写盘，内存中的 builtin 默认条目永不被写盘。必须有一条测试断言：写入后文件里不出现 builtin 默认条目的字段组合。
- 空集合短路（`initialize.rs:178-185`）在默认机器上不再触发：`Ready { total: 0 }` 改为「两个 in-process 实例的连接结果」；行为变化写入 acceptance。
- `PERI_MCP_BUILTIN=off`（A2）：`BuiltinInjectionPolicy::none()` ⇒ 零 overlay，`initialize_config` 行为与迁移前**逐位一致**；**语义必须写明**——middleware 提供面已删除，因此 off 的退回态**没有** Web/Artifact 能力（模型面不存在这三个工具），这是**显式运维开关、不是静默降级**，也没有「回退到旧实现」这条路径。必须有测试（off ⇒ 两个实例都不出现在 `pool.configs`）。与 `disabled: true` 的区别：后者仍保留该实例的 builtin 配置（只把该实例登记为 `Disabled`），另一个实例与其它 capability 不受影响。

### 4.8 模块与文件布局（含测试模块名与精确过滤器）

| 路径 | 内容 | owner |
| --- | --- | --- |
| `peri-acp-types/src/builtin_mcp.rs`（新增）+ `builtin_mcp_test.rs`（新增） | 注册表数据（逐工具 `direct` / `prompt_declaration` / 冻结字面量）+ 保留名表 + `find` + IF-D15 helper；挂载：`#[cfg(test)] #[path = "builtin_mcp_test.rs"] mod tests;` | E-01 |
| `peri-acp-types/src/lib.rs` | `pub mod builtin_mcp;` | E-01 |
| `peri-acp-types/src/plugin.rs` | `ConfigSource::Builtin { instance }` | E-01 |
| `peri-middlewares/src/mcp/transport.rs` + `transport_test.rs` | 变体、`kind()`、`BUILTIN_CONNECT_TIMEOUT`、`UnknownBuiltinInstance` | E-01 |
| `peri-middlewares/src/mcp/discover_tool.rs` + `_test.rs` | `config_source_str` 新臂 | E-01 |
| `peri-middlewares/src/mcp/builtin/mod.rs`（新增）+ `builtin_test.rs`（新增） | 纯函数：effective name、`apply_builtin_overlay`、策略、关闭集、`declared_direct_tools`、`builtin_prompt_declaration`；挂载 `#[cfg(test)] #[path = "builtin_test.rs"] mod tests;`（过滤 `mcp::builtin::tests`） | E-02（W1）→ I-01（W2 只追加 `mod web; mod artifact;`） |
| `peri-middlewares/src/mcp/builtin/runtime.rs`（新增）+ `runtime_test.rs`（新增） | spawn / task 表 / 有界关闭；挂载 `#[cfg(test)] #[path = "runtime_test.rs"] mod tests;`（过滤 `mcp::builtin::runtime`） | E-03 |
| `peri-middlewares/src/mcp/tool_bridge.rs` + `tool_bridge_test.rs` | **IF-D13 生效点**：`build_typed_tool_bridges` 应用声明 direct；新增 `build_deferred_tool_bridges` 供 public `build_tool_bridges` 使用（行为逐位不变，锁定测试 `:503-528` 不得改断言） | E-03 |
| `peri-middlewares/src/mcp/initialize.rs` + `initialize_test.rs` | builtin 分支（**不**注入配置，A1） | E-03 |
| `peri-middlewares/src/mcp/reconnect.rs` | builtin 分支 | E-03 |
| `peri-middlewares/src/mcp/config.rs` + `config_test.rs` | step 6.5 overlay + 策略参数 + 覆盖规则（含 A3/A17/A18 派生规则）/写回/hash 测试 | I-02 |
| `peri-middlewares/src/mcp/client.rs`、`client/status.rs`、`client/lifecycle.rs` | task 表 + `transport_type_of` | E-03 |
| `peri-middlewares/src/mcp/mod.rs` | W1：`pub(crate) mod builtin;`；W3 归 I-02 挂测试模块（`builtin_apply_tests`、`builtin_runtime_tests`） | E-01 → I-02 |

测试模块名（决定 `cargo test` 过滤器，必须逐字照用）：

```rust
// peri-middlewares/src/mcp/mod.rs
#[cfg(test)]
#[path = "builtin_apply_test.rs"]
mod builtin_apply_tests;      // 过滤：cargo test -p peri-middlewares --lib -- mcp::builtin_apply
#[cfg(test)]
#[path = "builtin_runtime_test.rs"]
mod builtin_runtime_tests;    // 过滤：cargo test -p peri-middlewares --lib -- mcp::builtin_runtime
```

`mcp/builtin/mod.rs` 与 `mcp/builtin/runtime.rs` 内的 `mod tests` 走仓库默认约定（`#[cfg(test)] #[path = "..."] mod tests;`），过滤分别为 `mcp::builtin::tests` 与 `mcp::builtin::runtime`。`peri-acp-types` 侧的挂载为 `builtin_mcp.rs` 内 `#[cfg(test)] #[path = "builtin_mcp_test.rs"] mod tests;`（过滤 `builtin_mcp`；owner E-01）。

**禁用过滤器（主计划 §9 规则 4）**：`mcp::builtin`（会命中 `builtin_spike_tests`）。**另注意** `-- session::factory` 恒为 0 tests（`peri-agent/src/session/factory.rs` 无测试模块）⇒ 按主计划 §9 规则 3 判失败，相关断言挂 `assembly::tests` / `session::exec::stage_builder`。

## 5. 实施步骤（每步：改什么 → 断言 → 命令）

**S1（E-01）类型与身份**

- 改：三个文件（`builtin_mcp.rs`、`plugin.rs`、`transport.rs`）+ `discover_tool.rs` + `mcp/mod.rs` 的模块声明。
- 断言：`TryFrom` 对 `source: Some(Builtin{..})` 返回 `Builtin`；对 stdio/http 夹具逐位不变；`kind()` 三分类；`config_source_str(Builtin) == "builtin"`；`BuiltinMcpTool::effective_name` 三字面量与后续 `effective_tool_name()` 输出一致（W1 先以常量形式锁定，E-02 接上行为后转为函数比对）。
- 命令：`cargo check --workspace --all-targets`；`cargo test -p peri-acp-types --lib -- builtin_mcp`；`cargo test -p peri-middlewares --lib -- mcp::transport`。
- **注意**：`cargo check` 必须在同一步内把 `initialize.rs:275`、`reconnect.rs:90` 的穷尽 `match` 补成三臂（哪怕 builtin 臂先返回 `unimplemented` 语义的 typed `Err`），否则整个 crate 不可验证。E-03 再替换为真实实现。

**S2（E-02）注册表行为**

- 改：`mcp/builtin/mod.rs`。
- 断言：三个冻结字面量逐字相等（与 `effective_tool_name()` 输出比对）；`effective_tool_names` 与注册表一致；`closed_instances` 对 `{"WebMiddleware"}` / `{"ArtifactMiddleware"}` / 空集 / 未知键四种输入正确；`apply_builtin_overlay` 六条规则（缺失 / 无 command+url 且 `disabled != true`（**含 A17 的 `system_mcp`/`system_mcp_tools` 填充**）/ 无 command+url 且 `disabled == true` / 有 command|url 且**保留名 ⇒ typed error** / 非法关闭片段 ⇒ 拒绝 / 未实现的预留名不注入）；`declared_direct_tools` 集合 == `system_mcp_tools` 集合（A5/A17）；`is_declared_direct("workspace", "Read") == false`（预留未实现）；`BuiltinInjectionPolicy::from_env` 三态（`off` / `0` / 缺省与未知值）。
- 命令：`cargo test -p peri-middlewares --lib -- mcp::builtin::tests`（**禁止**裸 `mcp::builtin`）。

**S3（E-03）运行时与接线**

- 改：`builtin/runtime.rs`、`initialize.rs`、`reconnect.rs`、`client.rs`、`client/status.rs`、`client/lifecycle.rs`、`mcp/tool_bridge.rs`（IF-D13 生效点 + `build_deferred_tool_bridges`）。
- 断言（crate 内）：① 一条 builtin 链路能完成 modern 握手且 `peer_info()` 为 `Some` ② server 侧收到 `tools/list`（线路级）③ 关闭后 server task 在 `BUILTIN_CONVERGE_TIMEOUT` 内收敛，未收敛才 abort ④ 未注册实例名 → `Failed` 且无 ready 证据 ⑤ 握手超时 → 走 builtin 超时常量 ⑥ pool 关闭 → 无 ready、无 orphan ⑦ reconnect 换新链路（旧 task 结束、新 handle 代际前进）⑧ `transport_type == "builtin"` ⑨ **大 payload**（WebFetch 级正文，数十~数百 KB）经 builtin 链路往返成功且内容完整（A16：`BUILTIN_DUPLEX_BUF` 只影响背压，不设帧上限）⑩ 类型化构造对声明 direct 的 builtin 工具产出 `is_direct() == true`，而 public `build_tool_bridges` 对同一 pool 仍全为 deferred（A5/IF-D13）。
- 命令：`cargo test -p peri-middlewares --lib -- mcp::builtin::runtime`；`cargo test -p peri-middlewares --lib -- mcp::initialize`；`cargo test -p peri-middlewares --lib -- mcp::tool_bridge`。

**S4（I-02）默认层与覆盖语义验证**

- 改：`mcp/builtin_apply_test.rs`（新增）+ `mcp/mod.rs` 测试挂载。
- 断言：注入后 `configs` 含两实例、字段逐项相等（含 `protocol_version = None`、`system_mcp_tools == 声明 direct 集合`）；`{"web": {}}` → 仍为 system 依赖且工具为 direct（A17）；`{"web": {"disabled": true}}` → `source` 为 Builtin 且 `disabled == Some(true)`、`system_mcp` 保持缺省 → 注册为 `ClientStatus::Disabled`、不建立 transport、不触发 fatal；`{"web": {"disabled": true, "system_mcp": true}}` → **加载期拒绝**（A18）；保留名声明 `command`/`url` → **typed error**（A3）；预留未实现名不注入；hash 去重结果不受注入影响（对照注入前后同一份 global+project 输入）；两个写回函数不把 builtin 写盘（临时目录断言文件内容）；`policy = none()` 时逐位等于迁移前；`PERI_MCP_BUILTIN=off`（以及 `0`）解析为 `none()`。
- 命令：`cargo test -p peri-middlewares --lib -- mcp::builtin_apply`。

## 6. 失败 / 超时 / 取消路径（每条都要有断言）

| 路径 | 期望行为 | 断言落点 |
| --- | --- | --- |
| 实例名未注册 | `TransportError::UnknownBuiltinInstance` → `insert_failed` + `commit_discovery_failure(…, false)`；**不**发布 ready | `mcp::builtin_apply`（构造）+ `mcp::builtin::runtime` |
| 保留实例名被用户接管（A3） | 加载期 typed error `McpConfigError::ReservedBuiltinInstanceName`（`command`/`url` 声明保留名）；**不**注册该 server、不进连接链 | `mcp::builtin::tests`（overlay 层）+ `mcp::builtin_apply` |
| 非法关闭片段（A18） | `{"web": {"disabled": true, "system_mcp": true}}` → 加载期 typed error，**不**落到 `readiness.rs` 的 fatal | 同上 |
| `PERI_MCP_BUILTIN=off`（A2） | 零注入 ⇒ 两实例都不在 `pool.configs`；**能力面为零**（不是回退到 middleware 实现，middleware 提供面已删）；不产生任何 fatal | `mcp::builtin_apply`（策略解析 + 零注入） |
| cwd 未绑定 | `ConnectionFailed`（措辞与 stdio 路径一致），不 fallback 进程 cwd | `mcp::initialize`（沿用既有 cwd 断言模式） |
| spawn 失败（构造 handler / duplex 失败） | `insert_failed("builtin 启动失败: …")` + 失败证据，`continue` 处理其它 server | `mcp::builtin::runtime` |
| 握手失败（`serve_client_auto` 返回 `Err`） | 走 `Ok(Err(_))` 分支（`:424-437` 同构），不产生 ready 证据 | `mcp::builtin::runtime` |
| 握手超时（`Elapsed`） | 走 `Err(_)` 超时分支，日志 `transport = "builtin"`、`timeout_secs = 5` | `mcp::builtin::runtime` |
| `tools/list` 失败 | `fail_tool_discovery` → `Failed` + `tools_list_ok = false`，**不**留下 `Connected + tools=[]` | `mcp::initialize` / `mcp::builtin::runtime` |
| 取消 / pool 关闭 | 等待方 `Cancelled → AgentError::Interrupted`（闸门层）；提交被拒时 `clear_discovery_evidence` + `break`；timeout **不是**取消 | `mcp::initialize` + V-01/V-02 |
| 关闭时 server task 未收敛 | 有界等待后 abort + `await`，不留 orphan | `mcp::builtin::runtime` |
| 重连期间旧 server task | 新链路建立前旧 task 必须已结束（或已 abort 并 join） | `mcp::builtin::runtime` |

## 7. 与既有测试的冲突清单与处置

| 测试 / 断言 | 冲突原因 | 处置 |
| --- | --- | --- |
| `mcp/transport_test.rs`（`test_config` / `stdio_config` / `http_config` 结构体字面量） | 本批次**不新增** `McpServerConfig` 字段，故无需机械收口 | 只新增 builtin 用例；既有用例期望值不动 |
| `mcp_isolation_contract.rs:382-383`（`"stdio"`） | helper 引入第三取值 | **既有断言不改**；V-03 复跑。夹具侧**允许的唯一改动** = `EnvIsolation`（`:140-160`）增加 `PERI_MCP_BUILTIN=off` + 新增「off 时 pool 恰有两台 server」断言（主计划 §5 R28 / A11） |
| `peri-acp/src/host/mcp_v4_startup_test.rs`（11 用例，走真实 `run_initialize`） | 注入两个 builtin 后其精确集合断言（servers 列表、计数）会变红 | 同 A11 处置：`HomeRedirect`（`:107+`）增加 `PERI_MCP_BUILTIN=off` + 新增一条「off 时 servers 恰为夹具实例」断言；**既有断言不改**（主计划 §5 R28） |
| `client/status.rs`（无独立 `status_test.rs`） | 三处推断分散 | 在 `builtin_apply_test.rs` 内断言 `transport_type`（含 stdio/http 对照），不新建 `status_test.rs` |
| `mcp/initialize_test.rs`（含 `:3-25` 的「拒绝 discover 逼 legacy」node 夹具） | 该夹具模型**不适用于** builtin：真实 rmcp server 覆写 `discover` 会导致 `tools/list` 被拒（§3 Q1(b)） | builtin 用例**不得**复用该脚本；必须用真实 `ServerHandler`（spike 模式）。该夹具继续服务 stdio 用例，不改 |
| `mcp/config_test.rs`（12 处 `load_merged_config_full_with_paths` 调用） | 注入点就在 loader 的 step 6.5，`_with_paths` 新增策略参数 | 由 I-02 统一补参：生产语义用例传 `all()`，断言迁移前行为的用例传 `none()`。**禁止**为了让旧断言变绿而把 overlay 挪出 loader 或改成 env-gated 旁路 |
| 断言「空配置 → `McpClientPool::initialize` 得到 `Ready{total:0}`」的测试（若存在，走 `#[cfg(test)] initialize`） | 注入后多出两个实例 | 优先给该测试传 `BuiltinInjectionPolicy::none()`（保持其原意是「测 loader/空配置路径」）；**禁止**改生产注入为旁路。归属：该测试文件所在 task 的 owner |
| `mcp/middleware_test.rs` / `mcp_v4_seam_test.rs` 的启动快照类断言 | 启动提交多了 builtin 工具 | 若断言是「某个 server 的 required 工具集合」则不改；若断言是「集合整体相等」则更新期望值。改动归属：V-01（其测试文件不在 E-03 的产出清单内，须先登记） |
| `builtin_spike_test.rs` | 无冲突 | **只读**：W0 闸门与回归证据，不得改 |

## 8. 本文件明确**不做**的事

1. 不实现 Cron / LSP / Workspace 三个实例，不为它们预建抽象（任何「通用实例注册中心 + 插件式 handler」都不做）；其名字只作为**保留实例名**登记（A3：防止被外部 server 接管），不注入、不解析。
2. 不做 session 中途声明 builtin 实例的语义（`system_mcp` 的 session 中途声明在 part-1 已拒绝，本批次不放开）。
3. 不做外部进程拉活 / 常驻 supervisor / 跨进程 builtin（builtin 恒为同进程，且无子进程）。
4. 不实现 host CLI 入口（不新增命令）。**注意**：`peri-tui` 在本批次**属于**范围（A8），但按名分支的归一归 **S-08**，不是 E-03/本文件的对象。
5. 不做按工具粒度的开关（`PERI_MCP_BUILTIN` 只区分「注入全部 / 不注入」，禁止成为策略面）；**也不得**把它写成「回退开关」意义上的旧实现回退（A2：off 时能力不存在）。
6. 不改 `McpServerConfig` 字段集与 serde 行为（不新增配置 key）。
7. 不改 `serve_client_auto` 的签名与 lifecycle 选择逻辑（`protocolVersion` 缺省仍为 Auto）。
8. 不改 `system_mcp` / `system_mcp_tools` / `system_mcp_timeout` 的语义与校验（IF-M1 冻结）；只在 `system_mcp_tools` 的**取值来源**上改为「声明为 direct 的工具集合」（A5/A17）。
9. 不在本批次扩展 `McpTaskKey`（如需扩展先登记主计划 §5 R11）。
10. 不实现 builtin 实例的 `resources/list` / `subscriptions` 语义（handler 可以不声明能力；`downgrade_resource_listing` 路径保持既有降级行为）。
11. **不**声称 capability root / 凭据隔离已验证：`capability_profile` 是 pool 级（`apps.rs:229-268`）、`bind_execution_cwd` 是 pool 级（`initialize.rs:161`），builtin 形态下不可证伪 ⇒ 按 **UNVERIFIED** 记录（A13）。可用的替代断言见主计划 §8「契约 5 扩展」行。
12. 不做「按名硬编码 effective name」的任何实现（A4）：本文件只提供 `effective_tool_name` 与冻结字面量，归一由 `peri-acp-types` 的 helper 与各消费点负责。

## 9. 依赖与移交

> **与并行编写的 sub-plan F / G 的仲裁（已按主计划 v2 更新）**：本文件的 `apply_builtin_overlay` 方案与「不新增 `McpServerConfig` 字段」由主计划 §5 R13/R14 冻结，**覆盖** sub-plan F 的 IF-F2（新字段）与 IF-F1（不变式措辞）；**覆盖规则今为六条**（R3/R14 + A3/A17/A18：保留名 typed error、非法关闭片段拒绝、`system_mcp`/`system_mcp_tools` 填充）；`transport_type` 三分类由 R16 冻结，**覆盖** F §6.7 的「本波不改」；`MIDDLEWARE_NAMES` **改为两表**（`MIDDLEWARE_NAMES` 只留槽位名 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS`）由 R5/R15/R24 冻结；F/G/H 文本里的旧装配编号（数值 4）一律读作 **`I-03`**（A14）；IF-D13/IF-D14/IF-D15 三条新冻结接口见主计划 §3；任务编号映射见 R18。实施者遇到本文件与 F/G 冲突时，以主计划为唯一裁决。

| 依赖 | 说明 |
| --- | --- |
| E-01 → E-02 → E-03 | 同一 crate 内串行；E-01/E-02 由同一 agent 连续执行（新增变体后完全 crate 立刻需要收口） |
| E-02 → I-01 | I-01 的 handler 注册到同一注册表；`mcp/builtin/mod.rs` 在 W2 由 I-01 追加子模块声明 |
| E-02 → I-02 | overlay 的接线与验证依赖 E-02 的纯函数（`apply_builtin_overlay` / 策略） |
| I-02 → I-03 | 链删除必须在 builtin 注入可用之后（否则出现能力真空窗口） |
| E-02/E-03 → S-01 | 策略一致性需要 `effective_tool_name` 与注册表（归一 helper 落 `peri-acp-types`，由 E-01 提供） |
| E-03 → I-03 | 链删除必须在 builtin 实例可用之后（否则出现能力真空窗口）；A6 的三个工具面（`static_tool_bridges` / `parent_tools` / workflow `build_tools`）都依赖 E-03 的类型化构造与声明 direct |
| E-03 → V-01 | 端到端（审批链 / 关闭矩阵 / 无 orphan / 大 payload）依赖运行时 |
| **V-06（W0，主计划）→ V-01 / V-02** | 迁移前基线与 host 侧 wire 夹具必须先于任何生产改动落地（A12/A10）；V-02 的「首个 LLM 请求」断言与基线逐项对照 |
| V-01 → V-02 | host seam 断言依赖 crate 内已证明的事实 |
