# MCP adaptation v4-part-1 — 主实施计划（master plan）

> 日期：2026-09-25。状态：**v2 已修订**（v1 经独立 check 后修订）。代码未实施。
>
> 目标事实源：`docs/design/mcp-adaptation-v4-part-1.md`（下称「设计文档」）。本文件是**实施批次、任务编排与接口冻结**的唯一事实源；设计文档是**契约语义**的唯一事实源。二者冲突时以设计文档为准，并回到本文件修订任务。
>
> 本文件不保存某一次执行的勾选状态、耗时或提交号；现场证据见 `2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`（task D-05 产出）。

## 0. v2 修订说明

v1 经三个独立 subagent 校验（事实核对 / 对抗式评审 / 冲突与依赖分析）。以下为 v2 的实质性变更及其触发原因：

| 变更 | 原因 |
| --- | --- |
| **推翻 v1 的 IF-M4**：不再把首批 `before_agent` 的 Err 从「warn 后继续」改为全局传播；改为**新增专用启动闸门 hook** | 全仓库有 6 个既有中间件的 `before_agent` 可返回 Err，其中 `AgentsMdMiddleware` 文件竞态、`AtMentionMiddleware` join 失败、`PluginMiddleware` 坏插件等属于**允许的软失败**。全局传播会把软失败升级为 turn fatal，属计划外回归 |
| **拆出 A-02a / A-02b** | A-01 给 `McpServerConfig` 加字段后，`peri-middlewares` 中大量 struct literal 与 `expand_server_config_with_context` 会立刻编译失败。A-01 与「机械收口」必须紧邻，否则同一 crate 内所有其它 task 都无法运行测试 |
| **插件严格化收窄为 MCP 专用路径** | 当前 `load_enabled_plugins_aggregated` 的宽容是**有意的产品行为**（坏插件不阻止宿主启动）。整体改 `Result` 会造成产品回归 |
| **明确 `tools/list` 的 discovery evidence** | `initialize.rs` 有 4 处 `unwrap_or_default()`（221/225/494/498 行）。`Connected + tools=[]` 会被误判为 ready，直接违反验收契约 2 与 4 |
| **统一接口可见性为 `pub(crate)`** | `peri-agent/src/session/exec/stage_builder/tools.rs:22` 的 `build_session_tool_view` 是 `pub(super)`；sub-plan 声称在 `peri-middlewares/tests/` 外部集成测试里断言真实工具视图**不可行**。冻结 `pub(crate)` 并把跨层断言上移到 `peri-acp` host seam |
| **补 `stage_builder.rs` 的所有权** | C 的接线需求指向该文件，但 v1 矩阵无人认领 —— 会导致「所有 task 绿色但 required 工具进不了首个 Reason」 |
| **补依赖边 B-05→B-04、B-04→B-03、A-02→B-01、D-05→D-06** | v1 把 B-04 排在 B-05 之前，方向反了 |
| **新增全局施工规则**：测试模块 wiring、禁止「0 tests 绿色」、禁止修改非自己拥有的文件 | 新增 `*_test.rs` 若不挂 `#[path] mod tests;`，`cargo test` 会以 0 tests 退出 0，产生假绿 |
| **验收矩阵降级契约 1/2/3/4/6 的证据强度声明** | v1 多处声称「强」，但外部集成测试无法触达真实 seam；必须写明实际断言层次 |

**对 sub-plan 的覆盖登记见 §5。** sub-plan 保留其领域内的细节权威；与 §5 冲突处以 §5 为准。

## 1. 范围

### 1.1 本次实施（契约 1–4、7）

| 契约 | 内容 | 主责 |
| --- | --- | --- |
| 1 | 配置解析拒绝「无 `system_mcp = true` 却声明 `system_mcp_tools`」 | A |
| 2 | System MCP 未完成 transport / initialize / 能力协商 / 必需工具检查前不得进入可启动 react loop；失败或 timeout 返回错误，不发布 ready | B |
| 3 | `system_mcp_tools` 每项经所属 MCP namespace 解析，schema 可构造 bridge，直接出现在 RCRA 工具列表；普通 deferred 工具仍走 `ToolSearchMiddleware` | C（解析与桥接）+ B（接线与目录发布） |
| 4 | 必需工具为空数组时只验证 ready，不注入额外工具 | C + B |
| 5 | 五个目标 MCP 的 transport / 状态 / 凭据 / capability root / client pool 不共享 | D（**只做已落地连接的隔离契约测试，PARTIAL**，见 §8） |
| 6 | 宿主保留能力（Permission / HITL / Hook / SubAgent / Workflow / Goal / PTC）不得被绕过 | D（**按能力分级**，见 §8） |
| 7 | 未完成迁移前必须区分「目标归属」与「已落地能力」 | D + 全部 sub-plan |

### 1.2 非目标（本次不做，且不得在文档中写成已实现）

- 不把 `Filesystem` / `Terminal` / `Web` / `Cron` / `Lsp` middleware 真实迁移为独立 MCP server。
- 不新建 Artifact / Web / Cron / LSP MCP 实例；不改 MCP 实例划分。
- 不实现 `system_mcp` 的 session 中途声明语义（动态 MCP 路径继续拒绝该 key，只加回归保护）。
- 不新增宿主 CLI 暴露入口。
- 不改 `ToolSearchMiddleware` 的既有 deferral 契约（只验证不回归）。
- **不改写既有 `before_agent` 的全局错误语义**（见 §3 IF-M4）。

## 2. 事实基线（已核实）

| 事实 | 位置 |
| --- | --- |
| `McpServerConfig` 是唯一字段定义；`mcp/config.rs` 仅 re-export | `peri-acp-types/src/plugin.rs:41-77` |
| `McpServerConfig` **没有** `rename_all = "camelCase"` | `peri-acp-types/src/plugin.rs:41-77`（对照 `:84-98`、`:116-130`） |
| 首批 `before_agent` 的 Err 被 `tracing::warn!` 后继续 | `peri-agent/src/agent/stages/mod.rs:879-886` |
| 后续批次 `before_input` 的 Err 会传播，且有 `Interrupted` 专门分支 | `peri-agent/src/agent/stages/mod.rs:887-894` |
| `run_before_agent` 已返回 `AgentResult`，可传播；链内任一 Err 立即返回 | `peri-agent/src/agent/stages/middleware_runner.rs:62-82`；`peri-agent/src/middleware/chain.rs:68-77` |
| `BaseTool::is_direct()` 默认 `false`；`McpToolBridge` 未 override → MCP bridge 全部 deferred | `peri-acp-types/src/tools.rs:608-612`；`peri-middlewares/src/mcp/tool_bridge.rs:179+` |
| `ClientStatus` 无 `Connecting` / `Reconnecting` 变体 | `peri-middlewares/src/mcp/client/types.rs:11-20` |
| 静态 MCP 无启动超时配置；动态 MCP 默认 30 000 ms | `peri-acp-types/src/dynamic_mcp.rs:198` |
| `tools/list` 结果有 **4 处** `unwrap_or_default()`（错误表现为 `Connected + tools=[]`） | `peri-middlewares/src/mcp/initialize.rs:221`、`225`、`494`、`498` |
| `build_session_tool_view` 是 `pub(super)`，外部集成测试不可调用 | `peri-agent/src/session/exec/stage_builder/tools.rs:22` |
| `peri-acp` host 的测试模块名是 `executor_flow_tests`（文件 `executor_flow_test.rs`） | `peri-acp/src/host/mod.rs:48-49` |
| 配置错误存在被吞成空配置的路径 | `peri-middlewares/src/mcp/config.rs:84`、`238-245`、`296-303`；`plugin/loader.rs:402-433`、`563-647` |
| `McpClientHandle` 无 credential 字段；`capability_profile` / `McpConnectionKey` 非 public | `peri-middlewares/src/mcp/client.rs:96-99`；`client/types.rs:83-132` |

## 3. 冻结接口（Interface Freeze v2，唯一版本）

### IF-M1 配置字段（owner：A-01 / A-02a）

```rust
// peri-acp-types/src/plugin.rs::McpServerConfig
#[serde(default, rename = "system_mcp", alias = "systemMcp", skip_serializing_if = "is_false")]
pub system_mcp: Option<bool>,
#[serde(default, rename = "system_mcp_tools", alias = "systemMcpTools", skip_serializing_if = "Option::is_none")]
pub system_mcp_tools: Option<Vec<String>>,
#[serde(default, rename = "system_mcp_timeout", alias = "systemMcpTimeout", skip_serializing_if = "Option::is_none")]
pub system_mcp_timeout: Option<u64>,   // 毫秒；缺省 30_000；合法区间 1..=600_000
```

**唯一错误枚举（三个变体，A 与主 plan 的统一版本）**：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpServerConfigValidationError {
    #[error("system_mcp_tools requires system_mcp = true")]
    SystemMcpToolsRequiresSystemMcp,
    #[error("system_mcp_timeout requires system_mcp = true")]
    SystemMcpTimeoutRequiresSystemMcp,
    #[error("system_mcp_timeout must be within 1..=600000 milliseconds")]
    SystemMcpTimeoutOutOfRange,
}

impl McpServerConfig {
    pub fn validate(&self) -> Result<(), McpServerConfigValidationError>;
}
```

- canonical 输出严格 snake_case 三 key；输入额外接受 camelCase 别名；两种拼法同时出现报 duplicate field。
- `system_mcp` 缺省 `None`；消费判定固定为 `== Some(true)`。
- `SystemMcpToolsRequiresSystemMcp` 触发条件为 `system_mcp_tools.is_some() && system_mcp != Some(true)`，**含显式 `[]`**。
- 不 trim / 不排序 / 不去重 / 不展开 `${...}` / 不加前缀。`None` 与 `Some([])` 必须保持可区分并可无损写回。

### IF-M2 一等工具注入（owner：C-INJ-01 / C-INJ-02 / C-INJ-03）

```rust
// 全部 pub(crate)：见 §0 可见性决策
pub(crate) fn build_typed_tool_bridges(pool: &McpClientPool) -> Vec<McpToolBridge>;
pub(crate) fn prepare_system_tools(
    bridges: Vec<McpToolBridge>,
    required: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<McpToolBridge>, SystemToolError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SystemToolError {
    MissingTool { server: String, tool: String },
    AmbiguousTool { server: String, tool: String, matches: Vec<String> },
    InvalidSchema { server: String, tool: String, reason: String },
    NotModelVisible { server: String, tool: String },
    EffectiveNameCollision { effective_name: String },
}

impl McpToolBridge {
    pub(crate) fn with_direct(self) -> Self;
    pub(crate) fn original_tool_name(&self) -> &str;
}
```

- **保留既有 public API 不变**：`build_tool_bridges(pool) -> Vec<Box<dyn BaseTool>>` 签名与 deferred 默认行为不得改变。
- **防重复注册**：一次构造 typed bridges，在原对象上提升 direct；`collect_tools` **整体替换**初始 bridge 集合，禁止再 append 一份。
- 工具名匹配：对模型暴露名 `mcp__{sanitize(server)}__{sanitize(tool)}`；配置数组在**所属 server 的原始工具名**上精确匹配，不折叠大小写、不剥离前缀。
- **all-or-nothing**：先验证全部 required 项，再统一 `with_direct`；不得返回部分成功。
- schema 只做结构解析（对象/属性存在性），**不做完整 JSON Schema draft 编译**。

### IF-M3 启动准入（owner：B-01 / B-03）

```rust
pub(crate) async fn await_system_connections(
    self: &Arc<Self>,
    cancel: &peri_agent::agent::AgentCancellationToken,
    started_at: tokio::time::Instant,
) -> Result<Vec<NegotiatedSystemMcp>, SystemReadinessError>;

pub(crate) async fn await_system_ready(&self) -> Result<SystemReadySnapshot, SystemReadinessError>;
```

- 时间类型统一 `tokio::time::Instant`（不用 `std::time::Instant`）。
- 全部 `pub(crate)`：内部 seam，不对外暴露（避免 private-in-public）。
- `SystemReadinessError` 变体全集由 sub-plan B §4.4 冻结；主 plan 只追加两条硬约束：① `Cancelled → AgentError::Interrupted`；② timeout **不是** cancel，必须映射 fatal。
- **不可伪造的 discovery evidence（v2 新增，硬要求）**：

```rust
pub(crate) struct DiscoveryEvidence {
    pub generation: u64,
    pub initialize_ok: bool,
    pub tools_list_ok: bool,
}
```

只有**真实成功的 live `tools/list`** 路径才能提交 `tools_list_ok = true`。空数组是成功结果；`Err` 不得创建 ready evidence。`initialize.rs:221/225/494/498` 的 `unwrap_or_default()` 必须消除。

### IF-M4 启动闸门 hook（owner：B-05 / B-04）— **v2 推翻 v1**

**v1 方案（已废弃）**：把 `stages/mod.rs:881-886` 的 warn 降级改为全局传播 `before_agent` 的 Err。

**v2 方案**：新增**专用启动闸门 hook**，只让 system 启动依赖经由它阻止 loop 启动。

```rust
#[async_trait]
trait Middleware {
    /// 首次 Receive 输入准备完成后、Compact 前的启动闸门。
    /// 默认 no-op；只有声明启动依赖的 middleware 实现。
    async fn before_react_start(
        &self,
        _state: &mut dyn hook_state::StartupState,
    ) -> AgentResult<()> {
        Ok(())
    }
}
```

要求：

- 新增 `StartupState` capability 接口，遵守 ARC-MW-002（hook 只接收其阶段真实支持的能力组合，不得继承完整 `MiddlewareState`）。
- `StartupState` 必须提供 candidate 的暂存与取出（见 IF-M5），且**不暴露可写 transcript/queue**。
- 调用点在首批 `before_agent` 之后、Compact 之前；`Interrupted` → `LoopResult::Interrupted`，其它 Err → `LoopResult::Error`。
- **既有 `before_agent` 的 warn 降级保持不变**（不改 `stages/mod.rs:881-886` 对 `before_agent` 的处理）。
- 不得按 middleware 名称字符串特判 MCP，不得新增 fail-open 开关。
- 若实施中发现该 hook 不可行而必须回退到 v1 方案，**必须**：① 在 §5 登记覆盖；② 对 `AgentsMd` / `AtMention` / `Plugin` / `SkillPreload` 各补回归测试；③ 在 acceptance 记录中声明为全局契约变更。不得静默回退。

### IF-M5 目录发布时序（owner：B-05）

session tool catalog 早于首批 `before_agent` 构建（`stage_builder.rs` 构建点），因此仅改 `collect_tools` 不足以让 direct 工具进入**首个 Reason**。

- candidate 必须经**本次 hook 的 state** 传递，**不得**存在 `McpMiddleware` 内部字段（避免失败未清除、cancel 后复用到下一轮、多 session 串用）。

```rust
pub(crate) struct StartupRequiredTool {
    pub server_name: String,
    pub original_tool_name: String,
    pub effective_tool_name: String,
}

pub(crate) struct StartupToolUpdate {
    pub tools: Vec<Arc<dyn BaseTool>>,
    pub required: Vec<StartupRequiredTool>,
}
```

- 失败或取消时直接丢弃 state 内 candidate，不需要 middleware 内部的 commit/discard 状态协议。
- 提交后**仍必须**完整走 ARC-TOOLS-001 的 Reason 发布顺序（`catalog refresh → working map swap → before_reason_catalog → before_model → pin`），startup 提交只能更新 static base，**不得**替代 Reason boundary、不得混入 dynamic overlay。

## 4. 文件所有权矩阵（v2 补漏）

**同一文件在同一时刻只能有一个 owner。** 违反所有权即为计划外改动，必须回退。

| 文件 / 目录 | owner | 备注 |
| --- | --- | --- |
| `peri-acp-types/src/plugin.rs` | A-01 | 含 timeout 字段与三变体错误枚举 |
| `peri-middlewares/src/mcp/config.rs`、`config_test.rs` | A-02a（字段收口）→ A-02b（错误闭环） | 同一 owner 串行 |
| `peri-middlewares/src/mcp/transport.rs`、`transport_test.rs` | A-02a / A-02b | |
| `peri-middlewares/src/mcp/client_test.rs`、`resource_cache_test.rs` | A-02a | **v1 漏列，v2 补** |
| `peri-middlewares/src/mcp/initialize.rs`、`initialize_test.rs` | A-02b 先 → **B-02 后** | 见 §5 R1；`unwrap_or_default()` 的消除归 B-02 |
| `peri-middlewares/src/plugin/loader.rs`、`loader_test.rs` | A-02b | 只新增 MCP 专用严格路径 |
| `peri-middlewares/src/mcp/dynamic/tool_test.rs` | A-03 | 仅测试 |
| `docs/reference/mcp-ecosystem.md` | A-04 | |
| `peri-middlewares/src/mcp/tool_bridge.rs` | C-INJ-01 | |
| `peri-middlewares/src/mcp/system_tools.rs`、`system_tools_test.rs` | C-INJ-02 | 新增；测试模块挂在 `system_tools.rs` 内 |
| `peri-middlewares/src/mcp/mod.rs` | C-INJ-02（W2：`system_tools` 生产模块声明）→ **D-02（W5：自身测试模块挂载）** | 仓库约定为在**实现文件**内挂 `#[cfg(test)] #[path = "..."] mod tests;`；D-02 的测试模块挂载落在 `mcp/mod.rs`，故该文件在 W5 归 D-02，B-03 不得代加 |
| `peri-middlewares/src/mcp/middleware.rs`、`middleware_test.rs` | **B-03 唯一** | C 不拥有 |
| `peri-middlewares/src/mcp/client.rs`、`client/readiness.rs`、`client/readiness_test.rs`、`client/lifecycle.rs`、`client/status.rs` | B-01 | 模块声明放 `client.rs`，不碰 `mcp/mod.rs` |
| `peri-middlewares/src/mcp/reconnect.rs`、`client_oauth.rs` | B-02 | |
| `peri-middlewares/src/mcp/dynamic/registry.rs`、`registry_test.rs` | B-06 | |
| `peri-agent/src/middleware/trait.rs`、`capabilities.rs`、`chain.rs`、`agent/stages/middleware_runner.rs`、`middleware_runner_test.rs`、`session/tool_catalog.rs`、`tool_catalog_test.rs` | B-05 | 含新 hook 与 `StartupState` |
| `peri-agent/src/session/exec/stage_builder.rs`、`stage_builder/builder_v2_test.rs` | **B-05** | **v1 无人认领，v2 补** |
| `peri-agent/src/session/exec/stage_builder/tools.rs` | B-06 | |
| `peri-agent/src/agent/stages/mod.rs`、`stages_test.rs` | B-04 | 调用新 hook（非改 `before_agent`） |
| `peri-acp/src/host/mod.rs`、`host/executor_flow_test.rs`、`host/prompt_test.rs`、`host/mcp_v4_startup_test.rs`（新增） | **B-07 唯一** | `peri-acp` 侧测试与模块声明单一 owner，避免并发编辑 `mod.rs` |
| `peri-middlewares/tests/mcp_isolation_contract.rs` | D-03 | 新增 |
| `peri-middlewares/tests/mcp_host_policy_contract.rs` | D-04 | 新增 |
| `peri-middlewares/src/mcp/mcp_v4_seam_test.rs`（新增） | D-02 | 新增；crate 内可触达 `pub(crate)` seam；挂载见 `mcp/mod.rs` 行 |
| `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md` | D-05 | 新增 |
| `docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 路由表 | **D-06 唯一** | 见 §5 R2 |

## 5. 对 sub-plan 的覆盖登记

| 编号 | 覆盖内容 | 以何为准 |
| --- | --- | --- |
| R1 | `initialize.rs` / `initialize_test.rs` 由 A-02b 先完成并移交 B-02 | 本文件 §4（串行，不并行编辑） |
| R2 | `initialize.rs:221/225/494/498` 的 `unwrap_or_default()` 修复归 **B-02**，不归 A | 本文件 IF-M3；覆盖 sub-plan A 的“只接线错误”表述 |
| R3 | `docs/code-index/**` 唯一 owner 是 D-06；**A-04 不再拥有任何 `docs/code-index/` 文件**（sub-plan A 任务表 A-04 行作废） | 本文件 §4；A-04 只写 `docs/reference/mcp-ecosystem.md` 并向 D-06 提交索引条目 |
| R4 | `system_mcp_timeout` 归 A（字段 + serde + 校验 + 透传 + hash + 默认值 + 区间）；覆盖 sub-plan A 的 IF-A5「B 决定 timeout，A 不新增字段」 | 本文件 IF-M1 |
| R5 | `McpServerConfigValidationError` 为**三变体**；覆盖 sub-plan A IF-A2 的单变体 | 本文件 IF-M1 |
| R6 | 新 hook `before_react_start` + `StartupState`；**覆盖 sub-plan B 的 B-04「改 `stages/mod.rs:881-886` 为传播」** | 本文件 IF-M4 |
| R7 | 插件严格化收窄为 MCP 专用路径（保留宽容 API）；覆盖 sub-plan A IF-A4 的整体严格化表述 | 本文件 §0 + §4 |
| R8 | 接口可见性统一 `pub(crate)`；覆盖 sub-plan C 的 `pub` 与主 plan v1 的 `pub` 混用 | 本文件 IF-M2/IF-M3 |
| R9 | 跨层「首个 LLM 请求 tools」断言上移到 `peri-acp` host seam（B-07 拥有），D-02 只断言 crate 内可观察层；覆盖 sub-plan C 的“通过 `run_reason` 验证”与 sub-plan D 的 D-02 外部集成测试方案 | 本文件 §4 + §8 |
| R10 | `stage_builder.rs` 归 B-05 所有；覆盖 v1 的未分配 | 本文件 §4 |
| R11 | `SystemReadinessError` / `SystemToolError` 变体全集以 B/C sub-plan 的冻结章节为准，本文件只追加 IF-M3 的两条硬约束与 `DiscoveryEvidence` | 本文件 IF-M3 + sub-plan B §4.4 + sub-plan C §3 |
| R12 | B-04 依赖 B-05（v1 方向反了） | 本文件 §7 |
| R13 | D-06 依赖 D-05（v1 同 Wave） | 本文件 §7 |
| R14 | 新增 A-02a / A-02b 拆分；覆盖 sub-plan A 的单一 A-02 | 本文件 §6 |
| R15 | sub-plan C 的 C-INJ-03 纳入任务表 | 本文件 §6 |
| R16 | B-03 排在 Wave 4（v1 §5 R5 误写「Wave 3 末尾」） | 本文件 §7 |

## 6. 任务表

任务 ID 沿用 sub-plan 原编号（新增项显式标注）。`→` 表示必须完成后才能开始。

| 批次 | Task | 标题 | owner 产出文件 | 依赖 | 验证命令 |
| --- | --- | --- | --- | --- | --- |
| W1 | **A-01** | 契约 DTO + 三变体校验 + timeout 字段 | `peri-acp-types/src/plugin.rs` | — | `cargo test -p peri-acp-types --lib -- system_mcp` |
| W1 | **A-02a** | 字段机械收口（恢复 workspace 编译） | `mcp/config.rs`、`config_test.rs`、`mcp/transport.rs`、`transport_test.rs`、`mcp/client_test.rs`、`resource_cache_test.rs`、`mcp/initialize.rs`、`initialize_test.rs`、`plugin/loader_test.rs` | A-01 → | `cargo check --workspace --all-targets` |
| W1 | **B-05** | 启动闸门 hook + `StartupState` + catalog 原子提交 | `peri-agent/src/middleware/trait.rs`、`capabilities.rs`、`chain.rs`、`agent/stages/middleware_runner.rs`、`middleware_runner_test.rs`、`session/tool_catalog.rs`、`tool_catalog_test.rs`、`session/exec/stage_builder.rs`、`builder_v2_test.rs` | — | `cargo test -p peri-agent --lib tool_catalog`；`cargo test -p peri-agent --doc` |
| W2 | **A-02b** | 配置错误闭环 + MCP 专用严格插件路径 | `mcp/config.rs`、`config_test.rs`、`mcp/initialize.rs`、`initialize_test.rs`、`mcp/transport.rs`、`transport_test.rs`、`plugin/loader.rs`、`loader_test.rs` | A-02a → | `cargo test -p peri-middlewares --lib -- mcp::config::tests` |
| W2 | **B-04** | 新 hook 的 Err 传播 + Interrupted 分类 | `peri-agent/src/agent/stages/mod.rs`、`stages_test.rs` | B-05 → | `cargo test -p peri-agent --lib agent::stages` |
| W2 | **C-INJ-01** | typed bridge 的 direct 提升 | `mcp/tool_bridge.rs` | A-02a → | `cargo test -p peri-middlewares --lib -- mcp::tool_bridge` |
| W2 | **C-INJ-02** | `system_tools` 解析 / 验证 / direct 提升（纯 crate 内测试） | `mcp/system_tools.rs`、`system_tools_test.rs`、`mcp/mod.rs`（一行） | A-02a、C-INJ-01 → | `cargo test -p peri-middlewares --lib -- mcp::system_tools` |
| W3 | **B-01** | 连接证据与等待（`DiscoveryEvidence`、watch、typed error） | `mcp/client.rs`、`client/readiness.rs`、`client/readiness_test.rs`、`client/lifecycle.rs`、`client/status.rs` | A-02b → | `cargo test -p peri-middlewares --lib -- mcp::client::tests` |
| W3 | **B-02** | 严格 discovery + 消除 4 处 `unwrap_or_default()` | `mcp/initialize.rs`、`initialize_test.rs`、`reconnect.rs`、`client_oauth.rs` | A-02b、R1 移交 → | `cargo test -p peri-middlewares --lib -- mcp::initialize` |
| W3 | **B-06** | 动态冲突目录配套 | `peri-agent/src/session/exec/stage_builder/tools.rs`、`mcp/dynamic/registry.rs`、`registry_test.rs` | B-05 → | `cargo test -p peri-middlewares --lib -- mcp::dynamic::registry` |
| W4 | **B-03** | `McpMiddleware` 闸门 + C 接线 | `mcp/middleware.rs`、`middleware_test.rs` | B-01、B-02、B-05、C-INJ-02 → | `cargo test -p peri-middlewares --lib -- mcp::middleware` |
| W5 | **C-INJ-03** | 验收接线回归（只读复核 + 复跑） | 无写入 | B-03 → | `cargo test -p peri-middlewares --lib -- mcp::system_tools`；`cargo test -p peri-middlewares --lib -- tool_search` |
| W5 | **A-03** | 动态配置拒绝 System key 边界回归 | `mcp/dynamic/tool_test.rs` | A-02b → | `cargo test -p peri-middlewares --lib -- test_dynamic_mcp_rejects_system_mcp_fields` |
| W5 | **A-04** | 配置参考文档（**不含 code-index**） | `docs/reference/mcp-ecosystem.md` | A-02b → | `git diff --check` + 人工核对 |
| W5 | **B-07** | host seam 验收（含首个 LLM 请求 tools） | `peri-acp/src/host/mod.rs`、`executor_flow_test.rs`、`prompt_test.rs`、`mcp_v4_startup_test.rs` | B-03、B-04 → | `cargo test -p peri-acp --lib -- host::executor_flow_tests` |
| W5 | **D-02** | crate 内 seam 测试 | `peri-middlewares/src/mcp/mcp_v4_seam_test.rs`（新增）、`mcp/mod.rs`（测试模块挂载） | B-03 → | `cargo test -p peri-middlewares --lib -- mcp::mcp_v4_seam` |
| W5 | **D-03** | MCP 实例隔离契约测试 | `peri-middlewares/tests/mcp_isolation_contract.rs` | B-01 → | `cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` |
| W5 | **D-04** | 宿主策略 / 生命周期契约测试 | `peri-middlewares/tests/mcp_host_policy_contract.rs` | B-03、C-INJ-02 → | `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` |
| W6 | **D-05** | 验收记录（契约矩阵 + 证据强度 + PARTIAL 标记） | `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md` | W1–W5 全部 → | 复跑 W1–W5 全部命令并记录终态 |
| W7 | **D-06** | code-index / 标准口径同步 | `docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` | D-05 → | `git diff --check` + 链接检查 |

**v1 任务表中已作废的行**：`D-01`（`peri-middlewares/tests/mcp_system_ready_e2e.rs`，外部集成测试触达不到 readiness 内部 seam）；其跨层职责由 **B-07**（host seam）与 **D-02**（crate 内 seam）承接。sub-plan D 的 D-01 行不再执行。

## 7. 执行批次（v2）

| Wave | 并发 task | 同 crate 冲突 | 串行原因 |
| --- | --- | --- | --- |
| **W0（闸门）** | — | — | 起始 `cargo check --workspace --all-targets` 必须绿，作为基线 |
| **W1** | A-01、A-02a（同一 agent 串行）、B-05 | `peri-acp-types`(A) + `peri-middlewares`(A) + `peri-agent`(B-05) | A-01 与 A-02a 必须由**同一个 agent 连续执行**：A-01 落字段后 crate 立刻编译失败，只有 A-02a 能恢复 |
| **W2** | A-02b、B-04、C-INJ-01、C-INJ-02 | `peri-middlewares`：A-02b、C-INJ-01、C-INJ-02（**3 个并发**） | 均已过 W1 闸门，文件互斥；中间态编译错误只允许在自己的文件内修 |
| **W3** | B-01、B-02、B-06 | `peri-middlewares`：B-01、B-02（2 个并发） | B-01/B-02 共享 readiness 证据语义，需在同一 Wave 内协同，由 B-01 先冻结类型 |
| **W4** | B-03（单） | `peri-middlewares`（单） | B-03 是本计划的关键路径汇聚点，独占 `middleware.rs`，必须独占执行 |
| **W5** | C-INJ-03、A-03、A-04、B-07、D-02、D-03、D-04（7 个） | `peri-middlewares`：C-INJ-03(只读)、A-03、D-02、D-03、D-04；`peri-acp`：B-07（单） | 文件互斥（见 §4）；`peri-acp/src/host/mod.rs` 由 B-07 独占 |
| **W6** | D-05（单） | — | 验收记录汇总 |
| **W7** | D-06（单） | — | 依赖 D-05 的终态 |

## 8. 验收矩阵（诚实分级）

| 契约 | 主责任务 | 断言层次 | 证据强度 |
| --- | --- | --- | --- |
| 1 | A-01、A-02b、A-03 | serde 类型层 + 四条加载入口（direct / global / project / plugin-MCP） | **PARTIAL→强**：仅在 MCP 专用严格路径落地后成立；宽容插件路径下的非法 MCP 配置必须仍被拒绝，需逐入口测试 |
| 2 | B-01、B-02、B-03、B-04、B-05、B-07 | 真实 transport seam + host prompt 路径（断言 fatal JSON-RPC error、模型调用计数为 0） | **强**（依赖 B-07 的 host seam 断言；仅有 crate 内单测不足以称强） |
| 3 | C-INJ-01、C-INJ-02、B-03、B-05、B-07 | bridge 层（C）+ catalog 层（B-05）+ 首个 LLM 请求 tools（B-07） | **强**：必须以 counting model 断言真实首个请求的工具入参 |
| 4 | C-INJ-02、B-03、B-07 | 空数组 → ready 且 direct 增量为 0；**且**须区分「tools/list 成功返回空」与「tools/list 失败」 | **强**：两条分支都要断言 |
| 5 | D-03 | pool entry / owner / transport wire / namespace / 无隐式跨 MCP 调用 | **PARTIAL**：凭据与 capability root 无 per-instance public observable；D-03 只覆盖「已落地连接的局部隔离」，**不覆盖契约 5 全文**。acceptance 必须标注 UNVERIFIED 项 |
| 6 | D-04 + B-07 | Permission / HITL / effective tool name / cancel 可在 middlewares 层断言；session / event / host assembly 需 host seam | **PARTIAL→中强**：按能力逐项标注 PASS / PARTIAL / BLOCKED，**不得**用一条测试覆盖七类能力 |
| 7 | D-05、D-06 | acceptance 记录使用三态（目标归属 / 当前实现 / 本次运行时证据） | **强**：契约 5、6 的 PARTIAL 必须显式写出 |

## 9. 全局施工规则（所有 agent 必须遵守）

1. **文件所有权**：只修改 §6 任务表中列为你产出的文件。编译错误出现在**非你拥有**的文件时，**忽略并继续**，不得顺手修复；完成自己的部分后如实报告。
2. **测试模块 wiring**：新增 `*_test.rs` 必须在对应生产模块中挂载 `#[cfg(test)] #[path = "<name>_test.rs"] mod tests;`。未挂载的测试文件不会被编译。
3. **禁止假绿**：验证命令若输出 `0 tests`，视为**失败**。每个 task 完成报告必须包含实际执行的测试数与 exit status。
4. **验证命令用精确过滤器**：不用过宽前缀（例：`host::prompt` 会匹配 `prompt_dispatch`）。模块名以真实模块为准（例：`host::executor_flow_tests`）。
5. **不并发跑 `cargo`**：同一 Wave 内多个 agent 会争抢 target 锁；cargo 会阻塞等待，不要因为等待而改用其它命令，也不要 kill 别人的构建。
6. **不新增 public API**：除非 §3 明确冻结为 `pub`；新增 seam 一律 `pub(crate)`。
7. **错误信息不含 secret**：错误只保留文件定位、server 标识与固定规则文本；不打印 env / headers / URL 认证信息 / OAuth 值。测试不得使用真实 secret。
8. **不写设计文档的进度**：设计文档不回填批次、提交号、勾选状态。

## 10. 风险登记

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| A-01 → A-02a 之间 crate 编译中断 | 同 Wave 其它 task 无法验证 | W1 由同一 agent 连续执行；W0 基线闸门；其余 middlewares task 全部排到 W2 之后 |
| 新 hook（IF-M4）不可行 | 需回退到全局传播方案 | 明确回退条件与三项补救义务（§3 IF-M4），禁止静默回退 |
| IF-M5 的 catalog 原子提交与 ARC-TOOLS-001 冲突 | 首个 Reason 拿不到 required 工具 | 提交只更新 static base；Reason boundary 顺序不变；B-07 以首个 LLM 请求入参为终审 |
| A-02b 触发插件产品行为回归 | 坏插件阻止启动 | 严格化只限 MCP 专用路径（R7）；既有宽容 API 不动 |
| 契约 5/6 证据不足被读成「已验收」 | 虚假完成 | §8 强制 PARTIAL 标注；D-05 必须写出 UNVERIFIED 项 |
| 同一 crate 多 agent 中间态编译错误 | agent 互相"修"对方文件 | §9 规则 1 明确禁止；D-05 复核 diff 是否越界 |
| 关键路径过长（W1→W7） | 中途失败留下半成品 | 每 Wave 后构建闸门；父 agent 终态独立复跑验证 |

## 11. 契约 7 的文档纪律

- 设计文档 `docs/design/mcp-adaptation-v4-part-1.md` **不回填**迁移批次、提交号和现场勾选状态。
- 所有描述必须保留「当前实现」与「v4 目标归属」的区分；§1.2 的非目标不得写成已实现。
- 引用「目标：完全下放 → Workspace MCP」时必须同时标注未迁移。
- acceptance 记录使用 `PARTIAL` / `BLOCKED` / `UNVERIFIED` 显式标记，不用绿色局部单测代替整体结论。

## 12. sub-plan 索引

- [`sub-plan A：配置契约`](2026-09-25-mcp-adaptation-v4-part-1-sub-plan-a-config.md)（受 §5 R3/R4/R5/R7/R14 覆盖）
- [`sub-plan B：1R 启动准入`](2026-09-25-mcp-adaptation-v4-part-1-sub-plan-b-readiness.md)（受 §5 R1/R2/R6/R12 覆盖）
- [`sub-plan C：一等工具注入`](2026-09-25-mcp-adaptation-v4-part-1-sub-plan-c-injection.md)（受 §5 R8/R9/R15 覆盖）
- [`sub-plan D：验证与文档一致性`](2026-09-25-mcp-adaptation-v4-part-1-sub-plan-d-verification.md)（受 §5 R3/R9/R10/R13 覆盖）
