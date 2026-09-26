# MCP adaptation v4-part-2 — sub-plan F：wave 1 实例迁移（Web MCP / Artifact MCP）

> **主 plan 覆盖（优先于本文）**：主计划 [`2026-09-26-mcp-adaptation-v4-part-2-plan.md`](2026-09-26-mcp-adaptation-v4-part-2-plan.md) 的 §5 覆盖登记优先于本文任何表述。涉及本文的覆盖如下（**实施前必读，本文对应小节已作废**）：
>
> | 覆盖 | 内容 | 本文受影响处 |
> | --- | --- | --- |
> | **R13** | **不新增 `McpServerConfig.builtin` 字段**、不做 struct literal 收口；builtin 身份由既有的 `source`（`#[serde(skip)]`）承载 | **IF-F2 作废**；IF-F3 的判定来源改为 `config.source == Some(ConfigSource::Builtin { .. })`（主 plan IF-D1），且 `TryFrom` 需新增 `TransportError::UnknownBuiltinInstance` |
> | **R3 / R14** | 覆盖规则 = 主 plan IF-D3 的**六条**：缺失→插入完整条目；无 `command`/`url` 且 `disabled != Some(true)` → 填 `source` **+ `system_mcp` + `system_mcp_tools`**（A17）；`disabled == Some(true)` → 只填 `source`；**保留名声明 `command`/`url` → 加载期 typed error**（A3，不再是「整条不动」）；非法关闭片段 → 加载期拒绝（A18）；`direct` 集合 == `system_mcp_tools` 集合（A5） | **IF-F1 被 IF-D3 替代**：本文「`builtin == Some("web")` 不变式」全部改判为「`source == Some(ConfigSource::Builtin { instance })`」；注入点为 `load_merged_config_full_with_paths` 的 **step 6.5**（`:419-430` 之后、`:433` 之前） |
> | **R5 / R15 / R24** | **两表形态（A7）**：`MIDDLEWARE_NAMES` **只留链槽位名**（删 `WebMiddleware`/`ArtifactMiddleware`）；新增 `BUILTIN_INSTANCE_POLICY_KEYS` 承载这两个关闭键；`provider/config.rs` 的 known 集合与 `all_middleware_disabled` 改两表并集（owner **S-02**，本文 I-08 改判为该改造） | **IF-F6 作废**；**IF-F4 的「从 `MIDDLEWARE_NAMES` 删除两键」成立但必须同时新增策略表**；`assembly_test.rs:1207-1217` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == 声明表 `policy_key` 集合」∧「交集为空」 |
> | **R16** | `transport_type` **必须**新增 `"builtin"`（主 plan IF-D11 的三分类 helper），不是另立 issue | **§6.7 作废**；差异处置改由 E-03 实现，`mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言仍不得修改 |
> | **R9（重写，A6）** | workflow agent **必须**补 builtin bridge 提供面（最小改法见 §6.6），旧的「本批次不接入」结论**作废** | **§6.6 的 workflow 行改判**：删两个挂载点后 workflow agent 仍须拿到 `mcp__web__*`（direct），否则是**净能力丢失** |
> | **R18 / A14** | **任务编号仲裁**：本文使用 `I-01…I-09`，与主 plan §6 的 `I-01…I-03` 语义不同；执行以主 plan §6 为准；**本文的旧装配编号（数值 4）一律读作 `I-03`** | 见下表「任务映射」 |
> | **R20 / R22 / R23 / R26** | A3 保留名（本文 §6.3 的关闭面需加「用户不得接管保留名」一条）、A5/IF-D13 直连性声明、A6 三个工具面、A9 声明段必须仍产出 | **IF-F0 的 handler 相关节**、§6.5（重写）、§6.6（重写）、§9 验收矩阵、§11 非目标 |
> | **R27 / R28** | A10（V-03 需新建 host wire 夹具，`PtcScriptedModel` 私有 ⇒ 需复刻）、A11（隔离夹具设 `PERI_MCP_BUILTIN=off`，既有断言不改） | §7 与 §9 中把「host 侧证据」引用的位置补齐；本文不持 host 文件 |
> | **R29 / A15** | `call_tool` 结果映射冻结为 **IF-D14**（owner I-01）；指向已作废 IF-E2 的引用全部改指 IF-D14 | **§6.1 的「二选一由 E 决定」作废**；§10「待核实项」第 3 条关闭 |
>
> **任务映射（F → 主 plan §6）**：
>
> | 本文 task | 主 plan task | 说明 |
> | --- | --- | --- |
> | I-01（声明表 + effective name 规则） | **E-01（数据）+ E-02（行为）** | 规则实现落 `peri-middlewares/src/mcp/builtin/mod.rs`，**必须**调用 `tool_bridge::sanitize_name_component`，不得复制规则（IF-D4）；数据落 `peri-acp-types/src/builtin_mcp.rs`（E-01，含逐工具 `direct`（IF-D13）与 `prompt_declaration`（A9）） |
> | I-02（`ConfigSource` + `TransportConfig`） | **E-01**（**去掉新增字段部分**） | `peri-acp-types/src/plugin.rs` 只加 `ConfigSource::Builtin { instance }`；`transport.rs` 加变体 + 三分类 + typed 错误；新增 `discover_tool.rs` 的 `config_source_str` 臂 |
> | I-03（默认层 overlay） | **I-02** | 含 12 处 `_with_paths` 调用点补参、六条覆盖规则（A3/A17/A18）、`mcp/builtin_apply_test.rs` + `mcp/mod.rs` 测试挂载 |
> | 第 4/5 号 task（两个 handler） | **I-01** | 落 `peri-middlewares/src/mcp/builtin/web.rs`、`artifact.rs`（**不是** `builtin/instances/**`，见 IF-E2 作废说明）；结果映射按 **IF-D14** |
> | I-06（删 middleware 与挂载点） | **I-01**（抽出可复用核心）+ **I-03**（删挂载点、再导出、A6 的三个工具面） | `middleware/web*.rs`、`artifact/*` 归 I-01；`assembly/*`、`mcp/middleware.rs`、`peri-acp/src/host/assemble.rs`、`lib.rs`、`middleware/mod.rs` 归 I-03 |
> | I-07（槽位 / 名单 / 锁定测试） | **I-03**（槽位与装配测试）+ **S-02**（两表 + `provider/config.rs` + `stage_builder` 谓词 + `builder_v2_test.rs`） | 本文「原子收口」的**理由仍然成立**，但跨 owner 拆分后必须按主 plan W3 的串行（I-02 → I-03 → S-02）执行 |
> | I-08（遗留键保留） | **取消原方案；改判为 S-02 的两表改造** | R5/R15/R24（A7）：不是「保留两键不动」，而是「删两键 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS` + 改 `provider/config.rs` 键集合」 |
> | I-09（声明段投影） | **S-02** | **A9 后不是「登记缺口」而是「必须仍产出声明」**（§6.5 已重写） |
>
> 事实源优先级：代码与契约测试 > `docs/standards/architecture-contracts.md` > `docs/design/mcp-adaptation-v4-part-1.md` > 主 plan > 本文件。

## 0. 裁决记录 A1–A19 落点（复核用索引）

| 裁决 | 本文件落点 |
| --- | --- |
| **A1** 注入点唯一（step 6.5） | 头部 R3/R14 行；IF-F1 头部作废说明；§7 任务表 I-03 行；§12 结论表 |
| **A2** `PERI_MCP_BUILTIN` 语义 | §6.3 关闭面（与 `disabled: true` 的区别）；§10 R11（本文件不持该实现，归 I-02/E） |
| **A3** 保留实例名 | §6.3（用户不得接管保留名）；§7 I-03 行断言；§10 R11；§9 验收矩阵 |
| **A4** 生效名归一原则 | §6.5（声明渲染用 effective name）；§10（引用主 plan §3 IF-D6 的 7 个消费点；本文件不实现 helper） |
| **A5** 直连性声明（IF-D13） | §6.1（handler 的 direct 语义来源）、§6.2、§6.6（三面 direct）、§9 验收矩阵「首个请求 direct」行 |
| **A6** 三个工具面 | **§6.6（重写）**；§9 验收矩阵；§11 非目标（删除「workflow 不接入」） |
| **A7** 两张名单 | 头部 R5/R15/R24；§4 矩阵（`provider/config.rs` → S-02）；§7 I-07 行 |
| **A8** TUI | §4 矩阵（`peri-tui/**` → S-08）；§9 关闭面矩阵「TUI completion」行 |
| **A9** 声明段保留 | **§6.5（重写）**；§7 I-09 行；§9 验收矩阵；§11 非目标（删除「本波不提供声明模板」） |
| **A10** V-03 夹具 | §7/§9 中 host 证据的引用（本文件不持 host 文件）；§10 待核实项 |
| **A11** 隔离夹具 env | §10 待核实项；§9「隔离」行 |
| **A12** 基线证据 | §9 验收矩阵（可见性等价以 V-06 基线为准） |
| **A13** 隔离断言 | §9「两实例隔离」行（capability root / 凭据 UNVERIFIED + 可观察四项） |
| **A14** 任务号与过滤器 | 头部 R18 行；§4 矩阵（`factory.rs` 无测试模块）；§7 全部命令；§7 表头注意项 |
| **A15** `call_tool` 映射（IF-D14） | §6.1（改指 IF-D14）；§7 的两个 handler 行；§10 待核实项第 3 条 |
| **A16** duplex 容量 | §10 待核实项（本文件不含 duplex 实现，归 E-03） |
| **A17** `{"web": {}}` 语义 | §6.3；§6.6；§9 验收矩阵 |
| **A18** 关闭片段形状 | §6.3（唯一合法片段 + 反例）；§10 R11 |
| **A19** 主计划唯一裁决源 | 头部任务映射表；§4 矩阵（TUI / provider config / docs owner）；§12 结论表 |

## 1. 元信息

- 日期：2026-09-26。状态：实现规划，**未实施**。本轮只写本文，不写生产代码、不改设计文档、不提交。
- 范围：wave 1 = 两个实例（Web MCP、Artifact MCP）的真实迁移；不含 Cron / LSP / Workspace。
- 交付判据（本文件的可验证产物）：
  1. 三个冻结 effective name（`mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`）出现在**首个 LLM 请求**的工具列表且为 direct（IF-D9）；
  2. `WebMiddleware` / `ArtifactMiddleware` 及其**全部四处挂载点**消失，`ChainSlot::Web` / `ChainSlot::Artifact` 处置完成（IF-D8）；
  3. 单独关闭 Web 或 Artifact 的能力在 ARC-CAPABILITY-CLOSURE-001 要求的**全部关闭面**上可观察（§6）；
  4. 遗留 `"WebMiddleware": false` / `"ArtifactMiddleware": false` 配置**不静默失效**（§6.3）。
- 本轮**不**宣称：Cron / LSP / Workspace 已迁移；`system_mcp` 的 session 中途声明；builtin 实例的凭据隔离已由运行时证据证明。

## 2. 事实基线（已核实，可直接引用）

### 2.1 迁移对象

| 事实 | 位置 |
| --- | --- |
| `WebMiddleware` 只有 `collect_tools`，无 prompt / hook / state 贡献；`name() == "WebMiddleware"` | `peri-middlewares/src/middleware/web.rs:29-38` |
| `WebMiddleware::build_tools()` 是 `pub`，被三处生产代码 + 一处测试调用 | `peri-middlewares/src/assembly/preparation.rs:132`、`peri-middlewares/src/assembly/workflow.rs:105`、`peri-middlewares/src/tool_search/declaration_test.rs:240` |
| `WebFetchTool` / `WebSearchTool` 各自 `is_direct() == true`、`namespace() == Some("web")`、实现 `prompt_declaration()` | `peri-middlewares/src/middleware/web_fetch.rs:92-113`、`peri-middlewares/src/middleware/web_search.rs:73-93` |
| Web 工具无状态、无凭据：`TAVILY_BASE_URL` 硬编码；`middleware/web*.rs` 全目录无 `env::var` | `peri-middlewares/src/middleware/web_search.rs:9`、`:148`（核实命令：`grep -rn "env::var" peri-middlewares/src/middleware/web*.rs` 无命中） |
| `WebFetchTool::invoke` 完全忽略 `ToolContext`（`_ctx`），自带 `reqwest` 30s timeout；`timeout()` 返回 `None` | `peri-middlewares/src/middleware/web_fetch.rs:141-165`、`:139` |
| `ArtifactMiddleware` 只有 `collect_tools`，`name() == "ArtifactMiddleware"`；文档注释自述「使 MetaHarness 可单独关闭公开上传能力」 | `peri-middlewares/src/artifact/mod.rs:10-34` |
| `ArtifactTool` 有 `cwd` 字段 + `ArtifactClient`；`is_direct() == true`、`namespace() == Some("meta")`、实现 `prompt_declaration()` | `peri-middlewares/src/artifact/tool.rs:15-26`、`:104-123` |
| Artifact 凭据：`PERI_ARTIFACTS_URL` / `PERI_ARTIFACTS_TOKEN`，缺省回退常量；构造期（`ArtifactTool::new`）即读环境 | `peri-middlewares/src/artifact/client.rs:31-38` |
| 两个 middleware 都在 `midware` 禁用名单机制里（`disabled.contains("WebMiddleware")` / `disabled.contains("ArtifactMiddleware")`） | `peri-middlewares/src/assembly.rs:272`、`:362` |
| `WebMiddleware` / `ArtifactMiddleware` 再导出 | `peri-middlewares/src/middleware/mod.rs:14`、`peri-middlewares/src/lib.rs:114`、`peri-middlewares/src/assembly.rs:25-30` |
| `web_test.rs` 挂载在 `web.rs` 内（16 个用例，全部测 `WebFetchTool`/`WebSearchTool`/`web_search` helper，**没有**测 `WebMiddleware` 本身） | `peri-middlewares/src/middleware/web.rs:40-42`、`peri-middlewares/src/middleware/web_test.rs` |
| `web_fetch.rs` 已有自己的测试挂载（`web_fetch_test.rs`，4 个用例，只测 `truncate_content`）；`web_search.rs` **没有**测试挂载 | `peri-middlewares/src/middleware/web_fetch.rs:216-218` |
| `artifact/mod.rs` 是 `mod client; mod tool; pub use tool::ArtifactTool;`——删除它会让 `crate::artifact::ArtifactTool` 整体消失 | `peri-middlewares/src/artifact/mod.rs:1-8` |
| `artifact/tool_test.rs` 挂载在 `artifact/tool.rs` 内（12 个用例） | `peri-middlewares/src/artifact/tool.rs:185-187` |

### 2.2 装配面与槽位

| 事实 | 位置 |
| --- | --- |
| `ChainSlot` 枚举含 `Web`（第二组）与 `Artifact`（第六组） | `peri-agent/src/session/factory.rs:27-89`（`Web` :56-57、`Artifact` :82-83） |
| `production_blueprint()` 的 `Web` 位于 `Terminal` 与 `Todo` 之间，`Artifact` 位于 `ToolSearch` 与 `Lsp` 之间 | `peri-agent/src/session/factory.rs:107-131`（`Web` :112、`Artifact` :127） |
| `assembly.rs` 的 `ChainSlot::Web` / `ChainSlot::Artifact` 分支是「禁用即空」模式 | `peri-middlewares/src/assembly.rs:272-275`、`:361-365` |
| 槽位 → middleware 名映射表 `slot_middleware_name` | `peri-middlewares/src/assembly_test.rs:1220-1250`（`Web` :1235、`Artifact` :1246） |
| 链序锁定测试 `default_config_produces_canonical_chain`（逐项断言完整序列，含 `"WebMiddleware"` :490、`"ArtifactMiddleware"` :498） | `peri-middlewares/src/assembly_test.rs:470-501` |
| `artifact_middleware_can_be_disabled_independently` 断言「`ArtifactMiddleware` 关闭后 `artifact` 工具消失、`SearchExtraTools`/`ExecuteExtraTools` 不受影响」 | `peri-middlewares/src/assembly_test.rs:503-524` |
| `MIDDLEWARE_NAMES`（27 项，含 `"WebMiddleware"` :107、`"ArtifactMiddleware"` :118）与 `production_blueprint` 槽位名**集合相等**由测试锁定 | `peri-acp-types/src/meta_harness.rs:93-121`；`peri-middlewares/src/assembly_test.rs:1205-1217` |
| `MIDDLEWARE_TOOL_NAMES`（23 项，含 `"WebFetch"`/`"WebSearch"`/`"artifact"`）与各 middleware 静态工具名并集相等由测试锁定 | `peri-acp-types/src/meta_harness.rs:163-201`（`:173-175`、`:192-193`）；`peri-middlewares/src/assembly_test.rs:1252-1295` |
| 剔除谓词按 `MIDDLEWARE_TOOL_NAMES` 名匹配，数据驱动，本身无需改代码 | `peri-agent/src/session/exec/stage_builder/tools.rs:28-52` |
| `builder_v2_test.rs` 用 `"WebFetch"`/`"WebSearch"` 作 fixture 并断言其在 `MIDDLEWARE_TOOL_NAMES` 内 | `peri-agent/src/session/exec/stage_builder/builder_v2_test.rs:38-45`（断言 :43-44） |
| `peri-acp` 侧 `validate_meta_harness` 会 **warn 后丢弃**不在 `SECTION_IDS ∪ MIDDLEWARE_NAMES ∪ {built-in-subagents}` 的 key | `peri-acp/src/provider/config.rs:396-413` |
| `example/minimal/.peri/settings.json:26`（`"WebMiddleware": false`）与 `:37`（`"ArtifactMiddleware": false`）是**仓库内现存的真实使用**；`example/minimal/README.md:53`、`:63` 有对应说明 | 已核实 |

### 2.3 builtin 运行时的可行性（spike 结论，可直接引用）

`peri-middlewares/src/mcp/builtin_spike_test.rs`（模块名 `mcp::builtin_spike_tests`，挂载于 `peri-middlewares/src/mcp/mod.rs:65-69`）用真实 `rmcp::serve_server` + 生产 `serve_client_auto` 在 `tokio::io::duplex(8 KiB)` 上确立了以下事实：

| 结论 | 见证用例 |
| --- | --- |
| 对端用 rmcp **默认** `discover` 时，Auto lifecycle 走 modern（线路只出现 `server/discover`），`peer.peer_info()` 可读且 `server_info.version` / `capabilities` 可读 | `builtin_spike_q1_default_discover_uses_modern_and_keeps_peer_info` |
| 对端 `discover` 返回 `-32601` 时 Auto 会**在同一连接上**回退 legacy `initialize`，且**此后 `tools/list` 恒被 `-32602` 拒绝**（缺 per-request `_meta`）；失败不在 client 侧，`tools/list` 根本没落到 handler | `builtin_spike_q3_legacy_fallback_breaks_tool_listing` |
| 对照：client 从不发 `server/discover`、直接 legacy `initialize` 时，工具发现与调用**可用** | `builtin_spike_legacy_only_handshake_lists_tools` |
| client `close_with_timeout` 后 server task 靠 duplex EOF 自然收敛为 `Ok(Ok(QuitReason::Closed))`，无需 abort | `builtin_spike_q2_server_task_converges_after_client_close` |
| modern 路径下真实 `tools/list` + `tools/call` 往返可用（server 侧各恰好 1 次） | `builtin_spike_q3_tools_round_trip_modern` |

**对本计划的直接约束（IF-F0）**：builtin 的 `ServerHandler` **不得**覆写 `discover` 使其失败；否则该实例的 `tools/list` 会在生产链路上被 `-32602` 拒绝，表现为 `ClientStatus::Failed`（v4-part-1 已消除 `unwrap_or_default()`，不会伪装成 ready）。默认采用「不覆写 `discover`」，见 §3 IF-F0。

### 2.4 消费侧（v4-part-1 已落地，wave 1 直接复用）

| 能力 | 位置 |
| --- | --- |
| `system_mcp` / `system_mcp_tools` / `system_mcp_timeout` 三字段与解析期校验 | `peri-acp-types/src/plugin.rs:77-107`、`:183-205` |
| `TransportConfig::try_from(&McpServerConfig)` 入口（含 `validate()` 前置） | `peri-middlewares/src/mcp/transport.rs:35-55` |
| 唯一的接线链路 `run_initialize → initialize_config`；disabled 分支注册 `ClientStatus::Disabled` 并跳过连接 | `peri-middlewares/src/mcp/initialize.rs:117-144`、`:146-160`、`:227-249` |
| System MCP 优先排序 + 严格 `tools/list`（失败即 `Failed`，不发布 ready） | `peri-middlewares/src/mcp/initialize.rs:214-224`、`:353-365` |
| 必需工具解析 / schema 校验 / direct 提升（all-or-nothing，按**原始工具名**在所属 server 内精确匹配） | `peri-middlewares/src/mcp/system_tools.rs:62-88` |
| effective name 规则与 sanitize（`^[a-zA-Z0-9_-]+$`） | `peri-middlewares/src/mcp/tool_bridge.rs:55-66`、`:95-101` |
| `visible_to_model()` 对无 `_meta` 的普通 `Tool` 返回 true（因此 builtin 工具不会被 `NotModelVisible` 拒绝） | `peri-middlewares/src/mcp/apps.rs:276-286`、`:351-354` |
| 启动闸门 hook `before_react_start` + `StartupState` + catalog 原子提交 | `peri-agent/src/middleware/trait.rs:87`、`peri-agent/src/session/tool_catalog.rs:260` |
| `transport_type` 由 `h.url.is_some()` 二值推导（builtin 的 `url` 为 `None` → 报告为 `"stdio"`） | `peri-middlewares/src/mcp/client/status.rs:172`、`:203`、`:224` |

## 3. 接口冻结

### IF-F0：builtin 实例的 lifecycle 选择（新增，落地方式二选一，F 冻结为 A）

- **A（默认，选它）**：builtin 默认层的 `McpServerConfig.protocol_version` 保持 `None`，`ServerHandler` **不覆写** `discover`。链路 = `ClientLifecycleMode::Auto { preferred_versions: [V_2026_07_28] }`，由 §2.3 的 modern 路径覆盖。
- **B（备选，仅当 A 的实测线路序列不稳定时启用）**：builtin 默认层显式声明 `protocolVersion: "2026-07-28"`，链路 = `ClientLifecycleMode::Discover`，无 legacy 回退。

两种都**必须**由测试钉住「builtin 实例的 `tools/list` 成功」这一可观察结果（见 §7 与 sub-plan H 的 V-* 任务）。选 A 的成本是依赖「不覆写 `discover`」这一约定，因此 A 必须附带一条回归用例断言该实例的**线路序列不含 `initialize`**，防止将来有人给 builtin handler 加上 `discover` 覆写而静默破坏工具发现。

### IF-F1：builtin 默认层与用户配置的 overlay 规则（⚠ **已被主 plan IF-D3 替代，见头部 R14**）

> **本节作废，保留仅作推理记录（说明「为什么 IF-D3 的规则 2 是必要的」）。** 实施以主 plan IF-D3 为准：注入点 = `load_merged_config_full_with_paths` step 6.5；`BuiltinInjectionPolicy` 为显式参数；三条覆盖规则见主 plan；注入后仍过 step 7 的 `validate_config`。本文下面「不变式」中凡出现 `builtin == Some(...)` 字样，一律读作 `source == Some(ConfigSource::Builtin { instance })`。

**问题**：`McpServerConfig.builtin`（IF-D1 的标记，见 IF-F2）是 `#[serde(skip)]`，用户无法从 wire 声明。若把 builtin 默认层当作普通一层「先合并」，则用户写 `{ "web": { "disabled": true } }` 会在合并时**整体替换**默认层条目 → `builtin` 标记丢失 → `TransportConfig::try_from` 走 `(None, None) => Err(InvalidConfig)` → 实例变成 `Failed` 而不是 `Disabled`，IF-D3 的「`disabled: true` 可关闭 builtin 实例」不成立。

**冻结规则（按优先级从低到高）**：

1. 三层用户配置（global → plugin → project）按 `load_merged_config_full_with_paths` 既有语义合并，**不插入 builtin**（`peri-middlewares/src/mcp/config.rs:328-435` 不动其顺序）。
2. 合并结果落定后，对 `peri_acp_types::builtin_mcp`（IF-D4）声明的每个实例名做 overlay：
   - 该名字**不在**合并结果中 → 插入 builtin 默认层条目（最低优先级，纯 gap-fill）。
   - 该名字在合并结果中 → 保留用户条目；**仅当**用户条目既无 `command` 也无 `url` 时，把 `builtin: Some(instance)` 与 `source: Some(ConfigSource::Builtin)` 填进用户条目。若用户声明了 `command` 或 `url`，则**清空** `builtin`、`source` 归该用户来源——用户完整接管该名字的传输，builtin 实例不再存在。
   - 其余字段（`disabled`、`system_mcp`、`system_mcp_tools`、`system_mcp_timeout`、`env`、`headers`…）一律以用户值为准，不做字段级深合并。
3. 结果必须过 `validate_config(&merged)`（既有第 7 步），非法组合仍是 `Err`，不得降级为空成功。

**不变式（必须由测试锁定）**：
- `{ "web": {} }`（空对象覆盖）→ 仍为 builtin 实例（`builtin == Some("web")`），且 `disabled`/`system_mcp*` 沿用 builtin 默认值。
- `{ "web": { "disabled": true } }` → `builtin == Some("web")` 且 `disabled == Some(true)`；`initialize_config` 的既有 disabled 分支（`initialize.rs:229-249`）把它注册为 `ClientStatus::Disabled` **且不建立任何 transport**。
- `{ "web": { "command": "my-web-server" } }` → `builtin == None`，`source` 为用户来源；不存在 builtin Web 实例。
- 未声明 `web` 的项目/全局/插件配置 → builtin Web 实例存在且 `system_mcp == Some(true)`。

### IF-F2：`McpServerConfig` 的 builtin 标记字段（⚠ **作废，见头部 R13**）

> **本节作废。** 主 plan IF-D1/IF-D2 冻结：**不**新增 `McpServerConfig` 字段、**不**做 struct literal 收口；builtin 身份由既有的 `source: Option<ConfigSource>`（已是 `#[serde(skip)]`）承载，`ConfigSource` 新增 `Builtin { instance: String }` 变体。因此本文下面的字段定义**不得实施**；保留仅用于说明「为什么需要一个不可从 wire 声明的身份载体」——该需求已由 `source` 满足。

```rust
// peri-acp-types/src/plugin.rs::McpServerConfig，紧随 pub source 之前
/// builtin 实例标记（实例名，如 "web" / "artifact"）。运行时标记，不序列化；
/// 只能由 builtin 默认层（IF-F1）写入，wire 不可声明。
#[serde(skip)]
pub builtin: Option<String>,
```

- 与 `source` 同性质（运行时标记、wire 不可见），因此 `McpServerConfigWire` **不加**对应字段，手写 `Deserialize` 里置 `None`。
- `Debug` / `Clone` 派生不变；不得加 `PartialEq` 语义（既有 `#[derive(Debug, Clone, Serialize)]` 保持）。
- 加字段会使仓库内全部 struct literal 立刻编译失败（与 v4-part-1 的 A-01 → A-02a 同类风险）。**必做**：本任务必须在同一变更新增后立即修完所有 literal，见 §7 的 I-02/I-03 串行约束。

### IF-F3：`TransportConfig::Builtin` 的判定顺序（新增，落地 IF-D1）

```rust
// peri-middlewares/src/mcp/transport.rs::TransportConfig
Builtin { instance: String },
```

> ⚠ 判定来源已由主 plan IF-D1 冻结：**唯一**来源是 `config.source == Some(ConfigSource::Builtin { .. })`（**不是**本文原先设想的新字段）。未解析到的实例名必须返回新变体 `TransportError::UnknownBuiltinInstance { instance }`。

`TryFrom<&McpServerConfig>` 的分支顺序冻结为：

```rust
config.validate()?;
if let Some(ConfigSource::Builtin { instance }) = config.source.as_ref() {
    return Ok(TransportConfig::Builtin { instance: instance.clone() });
}
match (&config.command, &config.url) { /* 既有 Stdio / StreamableHttp / InvalidConfig */ }
```

- `builtin` **优先于** `command`/`url`；R14 的三条覆盖规则已保证「用户声明了 command/url」时 `source` 不是 `Builtin`，因此该优先级不会遮蔽用户传输。
- 超时分类改**三分类**（`Builtin → BUILTIN_CONNECT_TIMEOUT`，冻结 5 s），且 `initialize.rs` / `reconnect.rs` 中禁止保留 `matches!(…, StreamableHttp { .. })` 形式的二元判定（主 plan IF-D1）。
- `build_http_transport` / `spawn_stdio_transport` 的既有分支**不改语义**；builtin 的 transport 装配分支归 sub-plan E（IF-E1）。

### IF-E1（消费 sub-plan E）：builtin transport 的装配接口

F 只消费、不实现。F 需要 E 提供且冻结以下可观察契约（签名以 E 的实际冻结为准，语义不得偏离）：

1. `initialize_config`（`peri-middlewares/src/mcp/initialize.rs`）新增 `TransportConfig::Builtin { instance }` 分支，且**不旁路**该函数既有的任何阶段：`validate_config` → `publish_system_manifest(Loaded)` → 排序（System 优先）→ `clear_discovery_evidence` → `serve_client_auto` → `retain_service` → `list_discovered_tools` → `commit_discovery_*`。
2. server 半边是**真实** `rmcp::serve_server(handler, (read, write))`，跑在由 `McpTaskOwner`（`peri-middlewares/src/mcp/task_scope.rs:180`、`:272`）纳管的 task 中；client 半边是生产 `serve_client_auto`，`transport` 为 `tokio::io::duplex` 的分半（与 `builtin_spike_test.rs::connect` 同形）。
3. `timeout` 取值：builtin 复用 `STDIO_CONNECT_TIMEOUT` 或由 E 新增常量；**不得**因为「同进程」而跳过超时。
4. 关闭语义：`McpServiceWrapper::close_with_timeout` 释放 duplex 写半边 → server task 收敛（IF-Q2 事实）；未收敛时的有界 abort 归 E。
5. 实例名的未知性：`instance` 不在 builtin 注册表中时，必须走 `insert_failed` + `commit_discovery_failure`（与 stdio 启动失败同路径），**不得**静默成功。

**IF-E2（消费，已由主 plan 冻结，本文原先的开放问题作废）**：builtin 实例 handler 的注册点。**目录布局已冻结**：`peri-middlewares/src/mcp/builtin/mod.rs`（E-01 骨架 → E-02 行为 → I-01 追加子模块声明，三 Wave 串行移交），两个 handler 落 `mcp/builtin/web.rs` 与 `mcp/builtin/artifact.rs`（**I-01**）。F **不**使用 `instances/` 子目录。**依赖边**：I-01 必须在 E-02 之后（handler 需要 `effective_tool_name` 与注册表数据）。

### IF-F4：`ChainSlot::Web` / `ChainSlot::Artifact` 处置 = **删除槽位**（落地 IF-D8；⚠ 见头部 R15）

**决策**：从 `ChainSlot` 枚举与 `production_blueprint()` 中**删除**两个变体。

> ⚠ **与主 plan 的差异（已裁决，以主 plan 为准）**：本文原先要求「同时从 `MIDDLEWARE_NAMES` 删除两键」，**该半句作废**（R15）。主 plan IF-D7 保留两键并把它们改注释为 builtin 实例关闭键，由 `BUILTIN_MCP_INSTANCES[].policy_key` 承载映射。因此实施时的动作是：**删槽位、保名单**——这也意味着 `assembly_test.rs:1208 middleware_names_match_production_blueprint` 必须改判（见 IF-F6 的冲突说明）。

**槽位删除的理由（仍然成立）**：
- `slot_middleware_name`（`assembly_test.rs:1220-1250`）的契约是「槽位 → `Middleware::name()` 返回值」；保留一个永不注册实例的空槽会让该契约失真。
- 删除**不改变其余槽位的相对顺序**：`Web` 在 `Terminal` 与 `Todo` 之间、`Artifact` 在 `ToolSearch` 与 `Lsp` 之间，删除后剩余序列相对顺序不变 → ARC-MIDDLEWARE-001 的链序契约不受影响。**必须**有断言「过滤掉被删两项后，本批次 blueprint 与上一批次 blueprint 逐项相等」（主 plan IF-D8）。

连带必须同步（否则锁定测试红）——以**主 plan IF-D8 给出的清单为准**（`assembly_test.rs` 的 `:394` / `:438` / `:472` / `:505` / `:624` / `:765` / `:1094` / `:1164` / `:1208` / `:1220` / `:1340` / `:1387`，以及 `builder_v2_test.rs`、`stage_builder/tools.rs` 的谓词注释与反向断言）；本文 §7 的 I-07 行只是该清单的子集视角。

### IF-F5：`MIDDLEWARE_TOOL_NAMES` 的删除是**安全要求**，不是可选项

`MIDDLEWARE_TOOL_NAMES` 的剔除谓词按**名字**匹配（`peri-agent/src/session/exec/stage_builder/tools.rs:36-43`）：共享表里名字落在清单内、且当前链未注册同名工具 → 该条目被剔除。迁移后不可能有任何 chain 注册出名为 `WebFetch` 的 middleware 工具，因此这三项若保留，会让**任何**同名工具（第三方插件、用户自建 MCP server 的裸名工具、P7 之后的其他来源）在**每个** session 的本地视图里被永久剔除。

因此：`"WebFetch"` / `"WebSearch"` / `"artifact"` **必须**从 `MIDDLEWARE_TOOL_NAMES` 移除，并由 sub-plan H 的 V-* 用例断言「同名非 middleware 工具不被误剔」。

### IF-F6：遗留关闭键的兼容（⚠ **作废，见头部 R15**）

> **本节作废。** 主 plan IF-D7 冻结：`MIDDLEWARE_NAMES` **保留** `"WebMiddleware"` / `"ArtifactMiddleware"` 两键，并在 `BUILTIN_MCP_INSTANCES` 的 `policy_key` 字段上承载「键 → 实例」映射（IF-D4 + IF-D10 的 `closed_instances`）。因此：
> - 遗留键**天然继续有效**，`peri-acp/src/provider/config.rs:395-413` 的键合法性校验**无需改动**（本文的 I-08 随之取消）；
> - **不需要** `LEGACY_DISABLE_ALIASES` 表，也**不得**把两键从 `MIDDLEWARE_NAMES` 删除（那正是会静默丢失关闭语义的做法）；
> - ⚠ **但**：主 plan IF-D7（保留两键）与 IF-D8（删除 `ChainSlot::Web` / `ChainSlot::Artifact`）之间存在一条**未收口的锁定测试冲突**——`assembly_test.rs:1208 middleware_names_match_production_blueprint` 断言 `MIDDLEWARE_NAMES` 与 blueprint 槽位名**集合相等**；两键保留而槽位删除后该断言必然失败（`slot_middleware_name` 也无处安放）。该缺口已登记在本文 §10 的 R9 与报告里，**实施前必须由主 plan 裁决**（候选处置：把该断言从集合相等改判为「blueprint 槽位名 ⊆ `MIDDLEWARE_NAMES`」+ 新增「policy-only 键恰为注册表 `policy_key` 集合」的独立断言）。

`"WebMiddleware": false` / `"ArtifactMiddleware": false` 在迁移前是**有效**的关闭面（`assembly.rs:272`、`:362`），且仓库内 `example/minimal/.peri/settings.json` 正在使用。IF-F4 删除 `MIDDLEWARE_NAMES` 里的两个名字后，`peri-acp/src/provider/config.rs:396-413` 的 `validate_meta_harness` 会对它们 **warn 后丢弃** → 用户「关闭 Web 以防外网访问」的配置**静默失效**。

**冻结规则**：两个遗留键必须继续可用，语义映射到 IF-D3 的实例关闭：

```rust
// peri-acp-types/src/builtin_mcp.rs（IF-D4 的声明表所在文件）
/// 迁移前的 MetaHarness 关闭键 → builtin 实例名。
pub const LEGACY_DISABLE_ALIASES: &[(&str, &str)] = &[
    ("WebMiddleware", "web"),
    ("ArtifactMiddleware", "artifact"),
];
```

- `validate_meta_harness` 的 `known` 集合必须把 `LEGACY_DISABLE_ALIASES` 的**键**一并纳入（否则键在到达任何消费者之前就被丢弃）。
- builtin 默认层（IF-F1 落点）必须读该别名表：别名键值为 `Some(false)` 且该实例无用户显式 `disabled` 声明时，把实例条目的 `disabled` 置为 `Some(true)`（然后走 `initialize_config` 的既有 disabled 分支）。
- 别名表**不得**回流进 `MIDDLEWARE_NAMES`（那会让 IF-F4 的删除白做）。
- 允许但不要求：后续批次把 `example/minimal/**` 迁到新写法。本波**故意不改**该 example，让它充当遗留键路径的活体回归样本。

### IF-F7：实例的 capability root 与凭据归属

| 实例 | capability root | 凭据 | `system_mcp_tools` |
| --- | --- | --- | --- |
| `web` | **无**（工具无状态；handler 不持有任何根目录）。`ToolContext` 的 `cwd` 传空串 `""`，因为两个 Web 工具都不读它 | **无**（`TAVILY_BASE_URL` 是公开常量，`web*.rs` 无 `env::var`） | `["WebSearch", "WebFetch"]`（顺序按声明表） |
| `artifact` | 构造期注入的 `cwd: String`（仅用于相对路径解析，`ArtifactTool::resolve_path`），由默认层从执行目录取 | `ArtifactClient` **在 Artifact MCP 内**构造（`ArtifactTool::new` 内部 `from_env_or_default()`），读 `PERI_ARTIFACTS_URL` / `PERI_ARTIFACTS_TOKEN`。凭据归 Artifact MCP 实例，不跨实例共享；**错误与日志路径不得打印 url/token**（沿用 `client.rs:31-38` 的既有注释纪律） | `["artifact"]` |

两个实例的 `system_mcp: true` + `protocol_version: None`（IF-F0 A）+ `source: Some(ConfigSource::Builtin)`。

## 4. 文件所有权矩阵（wave 1）

> ⚠ **以主 plan §4 为准**（见头部「任务映射」）。本表是 F 视角的细化（新增了一些主 plan 未逐项列出的文件归属判断），但与主 plan §4 冲突处一律以主 plan 为准；特别地：`middleware/web*.rs`、`artifact/*` 归 **I-01**，`assembly/*`、`lib.rs`、`middleware/mod.rs` 归 **I-03**，`meta_harness.rs` 与 `stage_builder/*` 归 **S-02**。

**同一文件在同一时刻只能有一个 owner。** 违反所有权即为计划外改动，必须回退。下表已把本文原先的 `I-0x` 编号替换为主 plan 的 task 编号。

| 文件 | owner | 备注 |
| --- | --- | --- |
| `peri-acp-types/src/builtin_mcp.rs`（新增）+ `builtin_mcp_test.rs`（新增） | **E-01** | 纯数据：`BuiltinMcpInstance { name, instance, policy_key, tools }` + `BuiltinMcpTool { original_name, effective_name, direct, prompt_declaration }` + `BUILTIN_MCP_INSTANCES` + `BUILTIN_RESERVED_INSTANCE_NAMES` + `find` + `is_reserved_instance_name` + IF-D15 helper（IF-D4 / IF-D13 / A3 / A9）。**不含**行为、不含 sanitize 复刻、不含遗留别名表（R5/R15/R24 已把该改造归 S-02） |
| `peri-acp-types/src/lib.rs` | **E-01** | 仅一行 `pub mod builtin_mcp;` |
| `peri-middlewares/src/mcp/tool_bridge.rs`、`tool_bridge_test.rs` | **E-03** | IF-D13 生效点：`build_typed_tool_bridges` 应用声明 direct；新增 `build_deferred_tool_bridges` 供 public `build_tool_bridges`（行为逐位不变，锁定测试 `:503-528` 不得改断言） |
| `peri-acp-types/src/plugin.rs` | **E-01** | 仅新增 `ConfigSource::Builtin { instance: String }`（IF-D2）；**不新增 `McpServerConfig` 字段**、不做 literal 收口（R13） |
| `peri-middlewares/src/mcp/transport.rs`、`transport_test.rs`、`mcp/discover_tool.rs`、`discover_tool_test.rs` | **E-01** | `TransportConfig::Builtin` 变体 + 以 `config.source` 为唯一判定 + `TransportError::UnknownBuiltinInstance` + 三分类超时（IF-D1） |
| `peri-middlewares/src/mcp/config.rs`、`config_test.rs`、`mcp/builtin_apply_test.rs`（新） | **I-02** | `load_merged_config_full_with_paths` step 6.5 overlay + `BuiltinInjectionPolicy` + 12 处 `_with_paths` 补参 + `mcp/mod.rs` 测试挂载（IF-D3；本文 IF-F1 作废） |
| `peri-middlewares/src/mcp/builtin/**`（运行时：transport 装配、task owner、注册表、`initialize.rs`/`reconnect.rs` 分支） | **sub-plan E** | F 不写；F 只消费 IF-E1/IF-E2 |
| `peri-middlewares/src/mcp/builtin/web.rs`（新增，含其 `#[cfg(test)]` 测试模块） | **I-01** | Web MCP handler（两个真实 `ServerHandler` 之一）；路径与模块名以主 plan §4/§6 冻结（IF-E2 作废） |
| `peri-middlewares/src/mcp/builtin/artifact.rs`（新增，含其测试模块） | **I-01** | Artifact MCP handler；同上 |
| `peri-middlewares/src/middleware/web.rs`、`web_fetch.rs`、`web_search.rs`、`web_test.rs` | **I-01** | 抽出可复用核心操作供 builtin 复用；删除 `is_direct` 提供面；`web_test.rs` 的 16 个用例**必须重新挂载**（§6.4） |
| `peri-middlewares/src/artifact/mod.rs`、`tool.rs`、`client.rs` | **I-01** | 同上；`ArtifactClient` 需可注入 base url / token 以支持无网络测试（主 plan §4） |
| `peri-middlewares/src/assembly.rs`、`assembly/preparation.rs`、`assembly/workflow.rs`、`middleware/mod.rs`、`lib.rs`、`assembly_test.rs` | **I-03** | 4 个挂载点 + 两处再导出 + 链序与名单类装配测试的期望值 |
| `peri-agent/src/session/factory.rs` | **I-03** | `ChainSlot` 枚举 + `production_blueprint`（IF-D8）；**该文件没有任何测试模块**（A14）⇒ 对应断言挂 `cargo test -p peri-middlewares --lib -- assembly::tests`（blueprint/链序）与 `cargo test -p peri-agent --lib -- session::exec::stage_builder`；**禁止**写 `-- session::factory`（会 0 tests 判失败） |
| `peri-middlewares/src/mcp/middleware.rs`、`middleware_test.rs` | **I-03** | `static_tool_bridges`（`:481-489`，含 `:487` fallback）改类型化构造 + 关闭集过滤（A6 面①） |
| `peri-acp/src/host/assemble.rs` | **I-03** | A6③ 的池注入点：`:510-511` 改 `default_workflow_middleware_factory_with_pool(mcp_pool_concrete.clone())`（`:332` 定义） |
| `peri-acp-types/src/meta_harness.rs` | **S-02** | `MIDDLEWARE_TOOL_NAMES` 删三裸名；`MIDDLEWARE_NAMES` **删两键**并新增 `BUILTIN_INSTANCE_POLICY_KEYS`（IF-D7 A 节 / A7） |
| `peri-acp/src/provider/config.rs`、`config_test.rs` | **S-02** | `validate_meta_harness` 的 known 集合（`:400-405`）与 `all_middleware_disabled`（`:419-426`）改为两表并集——否则 `"WebMiddleware": false` 会变成 warn+忽略的未知键（A7/R24） |
| `peri-agent/src/session/exec/stage_builder/tools.rs`、`tools_test.rs`、`builder_v2_test.rs`、`peri-agent/src/tools/invocation.rs`、`invocation_test.rs`、`peri-agent/src/session/tool_catalog.rs`、`peri-acp/src/event/tool_projection.rs`、`mapper_test.rs` | **S-02** | 谓词再论证、fixture 换名、参数别名（A4 ⑤）、`ToolFilterPolicy::canonical`（A4 ⑦）、`ToolKind`（A4，经归一而非硬编码） |
| `peri-middlewares/src/tool_search/declaration.rs`、`declaration_test.rs` | **S-02** | **A9**：声明段必须仍产出；`declaration_test.rs:239-242`/`:269` 不再调用 `WebMiddleware::build_tools()` / `ArtifactTool::new()`，改为从声明表构造 |
| `peri-middlewares/src/hooks/matcher.rs` | **S-01** | A4 ⑥ 匹配型归一 |
| `peri-tui/src/kit/tool_display.rs`、`truncate.rs`（+ 各自测试模块） | **S-08** | **A8：TUI 属于本批次**；`tool_display.rs:53/58` 与 `truncate.rs:186/190/275` 走 A4 归一（**不得**硬编码 `mcp__web__*`）；`kit/tool_semantics.rs`、`kit/acp_types/current_turn.rs` 经核查无按名分支 |
| `docs/reference/mcp-ecosystem.md` | **主 plan V-05**（W5） | 用户面的名称 / 关闭语义 / hook / `--disallowed-tools` / 保留名 / `PERI_MCP_BUILTIN` 说明；doc 检查项 = `git diff --check` + 链接检查 + 人工核对清单 |
| `docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` | **主 plan V-05** | F 不持有 |

**跨 sub-plan 交叠（已收敛）**：`peri-middlewares/src/lib.rs` 的唯一 owner 是 **I-03**（删 `WebMiddleware` 再导出）。G 的 S-01 **不**新增 crate 级模块声明（反查函数声明在 `permission/mod.rs`），因此**不存在** `lib.rs` 的交叠——本文原先的「F-I-06 先 → G-S-01 后」串行约束**作废**。

## 5. 覆盖登记

| 编号 | 覆盖内容 | 以何为准 |
| --- | --- | --- |
| R-F1 | IF-D8 的处置由本文冻结为「**删除** `ChainSlot::Web`/`ChainSlot::Artifact` 两个槽位」 | 本文 IF-F4（**已由主 plan 裁决**：槽位删除与本文一致；两键按 IF-D7/R5/R15/R24 **从 `MIDDLEWARE_NAMES` 删除并迁到 `BUILTIN_INSTANCE_POLICY_KEYS`**，改由 **S-02** 落地） |
| R-F2 | ~~IF-D3 的「`disabled: true` 可关闭」需要 overlay 规则~~ | **已由主 plan R3/R14 解决**：IF-D3 的六条覆盖规则已涵盖（含 `disabled != Some(true)` 时补 `system_mcp`/`system_mcp_tools` 的 A17 规则）；本文 IF-F1 作废 |
| R-F3 | ~~遗留 meta_harness 键会静默失效~~ | **已由主 plan R5/R15/R24（A7）解决**：两表形态 + `provider/config.rs` known 集合改并集（owner S-02）；本文 IF-F6 作废、I-08 改判为该改造。本文对该风险的**发现**成立，处置由主 plan 承担 |
| R-F4 | `MIDDLEWARE_TOOL_NAMES` 三项的删除是**误剔风险**的修复，不是文档同步 | 本文 IF-F5 |
| R-F5 | ~~`effective_mcp_tool_name` 的规则单一事实源归 `peri-acp-types::builtin_mcp`，`tool_bridge.rs` 改为委托~~ | **作废（主 plan IF-D4 / R13）**：依赖方向冻结为「`mcp::builtin::effective_tool_name` 调用 `tool_bridge::sanitize_name_component`」；`peri-acp-types` **只**持有纯数据（含冻结的 effective name 字面量）与 IF-D15 的**查表** helper，不复刻 sanitize。两处对齐由 E-02 的字面量测试保证 |
| R-F6 | `web_test.rs` 的 16 个用例在删除 `web.rs` 后会**静默不再运行**（0 tests 假绿的一种），必须重挂载 | 本文 §6.4；owner 归主 plan **I-01**（`middleware/web*.rs` 与其测试） |
| R-F7 | `TOOL_PARAM_ALIASES` 的处置归主 plan **S-02**（A4 ⑤ 匹配型归一）；TUI 按名分支归 **S-08**（A8 已在范围内，不再是「待裁决」） | 本文 §4 矩阵 |

## 6. 两个实例的详细设计

### 6.1 Web MCP（实例名 `web`）

**handler 形态**（归属主 plan **I-01**，落在 `peri-middlewares/src/mcp/builtin/web.rs`）：

```rust
/// 复用既有实现，不重写：持有两个 `Arc<dyn BaseTool>`。
/// Web 无状态、无凭据、无 capability root，因此 handler 是纯函数式的。
struct WebMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,   // WebSearchTool / WebFetchTool
}
```

- **复用 vs 重写**：**复用** `WebFetchTool` / `WebSearchTool` 的实现（`peri-middlewares/src/middleware/web_fetch.rs`、`web_search.rs`，模块声明 `pub(crate) mod` 于 `middleware/mod.rs:7-8`）。理由：两者的 `description` / `parameters` / `prompt_declaration` / 内部 `reqwest` 语义是既有行为的唯一事实源；重写会立刻产生 schema 漂移。主 plan I-01 删除的只是 `WebMiddleware` 包装，**不删**这两个工具模块（并按 I-01 的要求抽出可复用核心操作）。
- **`ServerHandler` 内持有并调用 `BaseTool`**：
  - `get_info()` → `ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new("peri-web-mcp", <crate version>))`；
  - `list_tools()` → `ListToolsResult::with_all_items(self.tools.iter().map(rmcp_tool_from_base).collect())`；
  - `call_tool(request, _ctx)` → 按 `request.name` 在 `self.tools` 中按 `name()` 精确匹配，命中后 `await tool.invoke(args, ToolContext::new(&[], ""))`；**结果映射按主 plan IF-D14（A15，owner I-01）冻结，不得二选一**：未命中工具名 → `Err(McpError::invalid_params("unknown tool: {name}"))`；`Ok(text)` → `Ok(CallToolResponse::Complete(CallToolResult::success(vec![ContentBlock::text(text)])))`；`Err(e)` → `Ok(CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(<分类化错误文本，不含路径/凭据>)])))`（**不用** `McpError::internal_error`）；取消 / 超时同 error 结果，不 panic、不 abort。两种形态各要一条断言。
  - **不覆写 `discover`**（IF-F0 A）。
  - `ToolContext::new(messages: &[], cwd: &str)` 是既有构造器（`peri-acp-types/src/tools.rs:477-488`）；Web 工具忽略 `cwd`，传 `""`。
- **schema 映射（`rmcp_tool_from_base`）**：

```rust
fn rmcp_tool_from_base(tool: &dyn BaseTool) -> rmcp::model::Tool {
    let def = tool.definition();                     // name/description/parameters
    let schema = def.parameters.as_object().cloned().unwrap_or_default();
    Tool::new(def.name, def.description, schema)     // 与 builtin_spike_test.rs:197-201 同形
}
```

  该 helper 必须**复用**（不要另写一份）——它的结构约束与 `prepare_system_tools::validate_input_schema`（`mcp/system_tools.rs:78-84`）同源：`parameters` 必须是 JSON object，否则 builtin 实例会在启动期被 `SystemToolError::InvalidSchema` 拒绝。因此 §7 的两个 handler task 必须加一条用例断言「两个 Web 工具的 `parameters()` 均能映射为 rmcp `Tool` 且 `input_schema` 非空 object」，把失败从启动期提前到单测期。

- **`system_mcp_tools` = `["WebSearch", "WebFetch"]`**：`prepare_system_tools` 按**所属 server 的原始工具名**精确匹配（`mcp/system_tools.rs` 模块文档），因此这里写裸名，不写 effective name。
- **配置来源**：`ConfigSource::Builtin`（IF-D2），由 builtin 默认层写入。

### 6.2 Artifact MCP（实例名 `artifact`）

**handler 形态**（归属 I-05 → 主 plan **I-01**，落在 `peri-middlewares/src/mcp/builtin/artifact.rs`）：

```rust
struct ArtifactMcpServer {
    tool: Arc<ArtifactTool>,   // 复用 crate::artifact::ArtifactTool（pub use 于 artifact/mod.rs）
}
```

- **复用 vs 重写**：**复用**。`ArtifactTool` 已承担 md→html 转换、扩展名/大小校验、`hash` 续传语义、OSC 8 链接输出（`artifact/tool.rs:86-100`、`:103-187`），重写等于复制一份 CSS 模板与上传协议。
- **capability root**：handler 构造时接收 `cwd: String`（来自 builtin 默认层的执行目录），构造 `ArtifactTool::new(cwd)`（`tool.rs:21-26`）。该 `cwd` 只用于相对路径解析，**不是安全沙箱**（沿用 `local-mcp-server` 的既有口径）；`ToolContext::new(&[], <该 cwd>)` 与工具自身 `cwd` 保持一致。
- **凭据归属**：`ArtifactClient::from_env_or_default()` 在 `ArtifactTool::new` 内执行 → 环境读取发生在 Artifact MCP 的构造点，凭据生命周期与该实例绑定。**不得**把 url/token 放进 `ServerInfo` / `Tool::description` / 错误文本；沿用 `redact_mcp_error` 与 `client.rs` 的既有「不写日志」注释。
- **schema 映射**：同 §6.1 的 `rmcp_tool_from_base`；`artifact` 的 `parameters()` 是 object（`required: ["file_path"]`），满足结构约束。
- **`system_mcp_tools` = `["artifact"]`**。
- **关闭语义必须保留（A7 修正后的机制）**：`ArtifactMiddleware` 的文档注释（`artifact/mod.rs:10`）承诺「MetaHarness 可单独关闭公开上传能力」。迁移后该承诺由三条同时成立承担：① **`BUILTIN_INSTANCE_POLICY_KEYS` 中的 `"ArtifactMiddleware": false`** → 该实例进入关闭集（`closed_instances`，主 plan IF-D10；`provider/config.rs` 的 known 集合按 A7 改为两表并集）② 或用户/项目配置写 `{ "artifact": { "disabled": true } }`（实例注册为 `Disabled`）③ `MIDDLEWARE_TOOL_NAMES` 中的 `"artifact"` 移除后，关闭时本地视图天然不含该工具（不再依赖剔除面）。**不允许**只删除 middleware 而不提供任一路径。

### 6.3 关闭面清单（ARC-CAPABILITY-CLOSURE-001 要求的 presence/absence 矩阵）

关闭一个 builtin 实例后（**三条合法路径**：`BUILTIN_INSTANCE_POLICY_KEYS` 的对应策略键置 false（A7）/ 用户显式配置 `{ "web": { "disabled": true } }`（**唯一合法关闭片段形状，A18**）/ 全局 `PERI_MCP_BUILTIN=off`（A2：两个实例都不注入，能力面为零，**不是**回退到 middleware 实现）），下列全部面必须同时消失；任一面仍然可见即为关闭不完整：

| # | 关闭面 | 迁移后的承担机制 | 必须由谁断言 |
| ---: | --- | --- | --- |
| 1 | direct tools（首个 LLM 请求的 `tools`） | 实例 `Disabled` / 在关闭集 → 无 bridge → `prepare_system_tools` 的必需工具缺失或该 server 无候选 → 不注入 | V-01（crate 内）+ V-02（host seam） |
| 2 | deferred index / `SearchExtraTools` 描述 | `static_tool_bridges`（类型化构造 + 关闭集过滤，A6 面①）不为关闭实例产出 bridge → 不进 deferred 索引 | V-01 |
| 3 | ACP updates（工具卡片 / `ToolKind`） | 无调用即无事件；`ToolKind` 属 S-02 的投影面（按 A4 归一） | S-02 + V-01 |
| 4 | TUI completion（按名参数摘要 / 输出折叠） | 属 **S-08** 的投影面（A8：在范围内，走 A4 归一，不得硬编码 effective name） | S-08 |
| 5 | 静态 prompt / examples / 声明段 | prompt 段落由 **S-06/S-04** 同步；**声明段不再是缺口**（A9：`tool_search/declaration.rs` 仍产出声明，owner S-02） | S-02 + S-06 |
| 6 | subagent 继承（`parent_tools`） | §6.6② 的等价路径（类型化构造 ⇒ 声明 direct）；关闭时 bridge 不产出 → 子 agent 的 `tools`/`disallowedTools` 名单对应条目落空 | V-01（crate 内断言 `is_direct()`）+ assembly 测试 |
| 7 | workflow agent 工具 | §6.6③（补 builtin bridge 提供面 + 关闭集过滤） | V-01 + assembly 测试 |
| 8 | 策略键不静默失效 | A7 的 `BUILTIN_INSTANCE_POLICY_KEYS` + `provider/config.rs` known 集合改两表并集（owner S-02） | S-02 + V-01 |
| 9 | `MIDDLEWARE_TOOL_NAMES` 不再误剔同名非 middleware 工具 | IF-F5 | I-03 + S-02 |
| 10 | 保留名不可被外部 server 接管 | **A3**：加载期 typed error（`web`/`artifact`/`cron`/`lsp`/`workspace`） | I-02（`mcp::builtin_apply`）+ S-01（反例测试） |
| 11 | 非法关闭片段不致命 | **A18**：`{"web": {"disabled": true, "system_mcp": true}}` 被加载期拒绝 | I-02（`mcp::builtin_apply`） |

**不在本表范围**：`/artifacts` slash 命令。`use-artifacts/SKILL.md:114` 声称存在该命令，但**核实结论**：`peri-tui/src` 与 `peri-acp/src` 中不存在 `"artifacts"` 命令注册（`grep -rn "\"artifacts\"" --include="*.rs" peri-tui/src peri-acp/src` 无命中）。因此该 skill 文本要么是过期描述、要么命令在别处。**待核实**：核实方法 = 全仓搜索 `CommandRegistry` 的注册点（`peri-acp-types/src/command_registry.rs` 及其调用者）确认是否存在 `artifacts` 命令；结论落地前，本计划**不**把 slash route 列为关闭面，也不据此改 skill 文本。

### 6.4 删除 middleware 的机械动作与测试重挂载（owner = 主 plan I-01 + I-03）

1. 删 `peri-middlewares/src/middleware/web.rs` 整个文件（含其 `#[cfg(test)] #[path = "web_test.rs"] mod tests;`）。
2. **重挂载 `web_test.rs`（16 个用例）**：在 `peri-middlewares/src/middleware/web_search.rs` 末尾加

```rust
#[cfg(test)]
#[path = "web_test.rs"]
mod tests;
```

   并把 `web_test.rs` 头部改为显式导入（去掉 `use super::*;`）：

```rust
use serde_json::Value;
use crate::middleware::web_fetch::WebFetchTool;
use crate::middleware::web_search::{format_search_results, SearchResult, WebSearchTool};
```

   验证命令 `cargo test -p peri-middlewares --lib -- middleware::web_search::tests` **必须报 16 passed**；报 0 tests 视为失败（plan §9 规则 3）。
3. `peri-middlewares/src/middleware/mod.rs`：删 `pub mod web;` 与 `pub use web::WebMiddleware;`。
4. `peri-middlewares/src/artifact/mod.rs`：**只删** `ArtifactMiddleware` 与其 `impl Middleware` / `Default`，保留 `mod client; mod tool; pub use tool::ArtifactTool;`。`artifact/tool.rs` 的测试挂载（12 个用例）不动。
5. `peri-middlewares/src/lib.rs:114`：从 `middleware::{...}` 再导出列表删 `WebMiddleware`；删 `ArtifactMiddleware` 的再导出（若在同处的 re-export 块内一并处理）。
6. `peri-middlewares/src/assembly.rs:25`、`:30`：删两个 `use`。
7. `assembly/preparation.rs:131-133`：删 `if !disabled.contains("WebMiddleware") { parent_tools.extend(WebMiddleware::build_tools()); }`；**同时**把 `:136` 的 `build_tool_bridges(pool)` 改为**类型化构造**（`build_typed_tool_bridges` + 关闭集过滤），否则子 agent 侧的 builtin 工具会从 direct 静默降级为 deferred 且其链无 ToolSearch（A6 面②）。
8. `assembly/workflow.rs:104-106`、`:205-207`：删 workflow agent 工具集与链中的对应两处（`WebMiddleware::build_tools()` / `WebMiddleware::new()`），并**按 §6.6③ 的最小改法**在 `build_tools` 追加 builtin direct bridge 提供面（否则 Web 能力净丢失）。
9. `mcp/middleware.rs:481-489`（`static_tool_bridges`，含 `:487` fallback）：改**类型化构造** + 关闭集过滤（A6 面①）。
10. `peri-acp/src/host/assemble.rs:510-511`：改 `default_workflow_middleware_factory_with_pool(mcp_pool_concrete.clone())`。
11. **不**改 `example/minimal/**`（IF-F6：作为遗留键路径的活体样本）。

### 6.5 声明段投影（I-09）

`peri-middlewares/src/tool_search/declaration_test.rs` 的 `build_real_direct_tools()` 直接调用 `WebMiddleware::build_tools()` 与 `ArtifactTool::new("/tmp")`（`:239-242`、`:269`），并在 `:290-313` 断言 14 Core + 3 Meta 中每个工具都实现 `prompt_declaration()`。

迁移后这三个工具的 `prompt_declaration()` 由谁提供，取决于 ACP 桥是否透传声明模板：
- `McpToolBridge` **未**实现 `prompt_declaration()`（回退 trait 默认 `None`），因此迁移后 `collect_declarations`（`tool_search/declaration.rs:16-40`）对这三个工具**无输出** → `05_using_tools.md` 只保留通用纪律，Web/Artifact 的逐工具指引从系统提示词中**整体消失**。

**I-09 的处置（冻结，已按 A9 重写；owner = 主 plan **S-02**）**：

1. `build_real_direct_tools()`（`:230-271`）**不再**调用 `WebMiddleware::build_tools()`（`:239-242`）与 `ArtifactTool::new("/tmp")`（`:269`）——删 middleware 后前者无法编译；改为**从声明表构造**这三个工具的 direct 表示（`mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`；crate 内可用合成 `McpClientHandle` 构造 `McpToolBridge`，见 `tool_bridge.rs` 的 `direct_flag_tests` 用法），期望名单相应改为「14 Core − 2 web + 2 web effective + 3 Meta − 1 artifact + 1 artifact effective」的等价新名单。
2. **冻结结论：声明不得丢失（A9）**——机制 = 声明文本进声明表（`BuiltinMcpTool::prompt_declaration`，纯数据）+ builtin 工具的 direct 桥实现 `prompt_declaration()`（`McpToolBridge` 对 builtin 来源返回 `builtin_prompt_declaration()`，其余仍为 `None`）。
3. **新增断言**（取代原「缺口声明用例」，两者不可并存）：`builtin_direct_tools_still_emit_prompt_declarations`
   - `collect_declarations` 对三个 builtin direct 工具**产出非 None** 文本；
   - 渲染后的 `{{name}}` 为 effective name（`mcp__web__WebFetch` / `mcp__web__WebSearch` / `mcp__artifact__artifact`）；
   - 文本中**不含**裸名 `WebFetch` / `WebSearch` / `artifact`；
   - 原 `:290-313` 的「14 Core + 3 Meta 全部实现 `prompt_declaration()`」计数断言强度不得降低。
4. 渲染规则**不变**：`{{name}}` → `desc.name`（`tool_search/declaration.rs:47-75` 的单遍扫描替换），因此渲染出 effective name 是**预期结果**，与「模型必须用 effective name 调用」一致；不得为此新增渲染分支或硬编码名字（A4）。
5. 命令：`cargo test -p peri-middlewares --lib -- tool_search::declaration`。

### 6.6 三条容易漏的能力等价路径（A6：必须全部保留 Web 能力）

`ChainSlot` 只覆盖顶层链。以下三条路径**不经过 ChainSlot**（或只部分经过），是本波最容易漏掉的等价点——**裁决是三条都必须拿到 `mcp__web__*`（direct）**：

| 路径 | 迁移前 | 迁移后（冻结） | 归谁 |
| --- | --- | --- | --- |
| ① 主链 deferred 收集 | `mcp/middleware.rs:487` 的 `build_tool_bridges`（全 deferred）+ `prepare_system_tools` 提升 | `static_tool_bridges`（`:481-489`）改**类型化构造**（`build_typed_tool_bridges` + 声明 direct）并加关闭集过滤 | I-03（落点）+ E-03（类型化构造） |
| ② subagent 父工具继承 `parent_tools` | `assembly/preparation.rs:131-133` 给裸名 `WebMiddleware::build_tools()`（**direct**）；`:136` 又给 `build_tool_bridges(pool)`（**全 deferred**）。子 agent 链**没有 ToolSearch**（唯一实例化点 `assembly.rs:356`） | `:131-133` 删除；`:136` 改走**类型化构造** ⇒ builtin 声明的 direct 在 parent_tools 上生效，子 agent 仍能用 `mcp__web__*`。**不成立**的旧论证已删除：「父链装配会自动带上 `mcp__web__*`」是错的——`build_parent_tools` 是自己按分支构造的 | I-03 + E-03 |
| ③ workflow agent 工具与链 | `assembly/workflow.rs:104-106`（工具集，`WebMiddleware::build_tools()`）、`:205-207`（链，`WebMiddleware::new()`）。workflow 链**没有 `McpMiddleware`** | 两处删除，并在 `build_tools`（`:87`）追加 **builtin bridge 提供面**（最小改法见下），否则 workflow agent 的 Web 能力**净丢失** | I-03 |

**③ 的最小改法（冻结）**：
```rust
// peri-middlewares/src/assembly/workflow.rs
pub struct WorkflowAgentMiddlewareFactory { builtin: Option<Arc<McpClientPool>> } // 由 ZST 改为持有池
pub fn default_workflow_middleware_factory() -> Arc<dyn WorkflowMiddlewareFactory>            // 签名/行为不变：builtin = None
pub fn default_workflow_middleware_factory_with_pool(pool: Option<Arc<McpClientPool>>)
    -> Arc<dyn WorkflowMiddlewareFactory>                                                     // 生产入口
```
- `build_tools`：在 `disabled` 语义下，用 `closed_instances(disabled)` 过滤后，把 `is_declared_direct` 命中的 builtin 工具以 **direct** bridge 追加（`mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`）。
- `build_middlewares`：只删除 `:205-207`，**不**新增 MCP middleware（避免引入 deferred/ToolSearch 语义与跨层改动）。
- 生产调用点：`peri-acp/src/host/assemble.rs:510-511` 改传 `mcp_pool_concrete.clone()`（该变量在 `:332` 定义；`bare` 时是 `None` ⇒ 与今天行为一致）。
- 既有测试 `assembly_test.rs:820/845/860/1328/1341/1388` 仍用无池构造器，不受影响。

**必须做的最小验证**（断言必须落在**可观察能力面**，不得只断言中间量）：`cargo test -p peri-middlewares --lib -- assembly::tests` 必须包含
1. 「三个面**同时**含 `mcp__web__WebSearch` / `mcp__web__WebFetch`」（未关闭时）——① 顶层链工具集合、② `parent_tools` 的 `is_direct()`、③ workflow agent 的工具列表；
2. 「关闭 `WebMiddleware` 后三个面**同时**不含 `mcp__web__*`」；
3. 关闭矩阵还必须覆盖 `ArtifactMiddleware=false` 与两者都 false（主 plan IF-D10 第 5 条）。

**归属声明（已按主 plan §4 更正）**：`assembly_test.rs`、`assembly/workflow.rs`、`assembly/preparation.rs`、`mcp/middleware.rs`、`peri-acp/src/host/assemble.rs` 全部归 **I-03**；`parent_tools` 的关闭过滤落点 = IF-D10 的第 3 处，workflow 面 = 第 4 处（A6 后**必须显式过滤**）。若需新建独立测试文件（如 `peri-middlewares/src/assembly/builtin_instances_test.rs`），必须由 I-03 在 `assembly.rs` 内挂载 `#[cfg(test)] mod`（主 plan §9 规则 12）。

### 6.7 `McpClientPool::all_server_infos()` 的 `transport_type`（⚠ **已被主 plan IF-D11 覆盖，见头部 R16**）

> **本节结论作废**：主 plan IF-D11 冻结 `transport_type` **必须**新增第三分类 `"builtin"`（单一 helper `transport_type_of(source, url)`，三处调用点共用），并由 **E-03** 实现。本文原先「本波不改、另立 issue」的结论**不成立**。
> 仍然成立的约束：`peri-middlewares/tests/mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言**不得修改**（其夹具是 stdio 实例），只复跑；新增断言为「builtin 实例在 `all_server_infos()` 与 `snapshot()` 中 `transport_type == "builtin"`」。据此，sub-plan H 的隔离用例**可以**用 `transport_type` 区分 builtin 与 stdio（本文原先禁止该用法的一半作废，但仍禁止改动 stdio 夹具的既有断言）。

`transport_type` 由 `h.url.is_some()` 二值推导（`client/status.rs:172`、`:203`、`:224`），builtin 实例的 `url` 为 `None` → 报告为 `"stdio"`。这不是错误，但会让 sub-plan H 的隔离断言在 builtin 上无法区分 builtin/stdio。

**决策（作废，见头部 R16）**：~~本波不改该推导~~ → **必须**按主 plan IF-D11 新增 `"builtin"` 三分类（helper `transport_type_of`，三处调用点共用），由 E-03 实现。

## 7. 任务表

> ⚠ **执行以主 plan §6 为准**（见头部「任务映射」）。下表保留本文的实现细节权威（每个 task 的断言、文件清单、注意点），但 **task ID 不直接派工**：派工时按映射表落到主 plan 的 `E-01/E-02/E-03/I-01/I-02/I-03/S-01/S-02/S-08`，并遵守主 plan §7 的 Wave 顺序与 §9 的全局施工规则（特别注意 §9 规则 4：**禁用过宽过滤器**——`mcp::builtin` 会命中 `builtin_spike`；`-- session::factory` 恒 0 tests 判失败）。**本文的旧装配编号（数值 4）一律读作 `I-03`**（A14）。

依赖用 `→` 表示「必须完成后才能开始」。**所有命令在仓库根目录运行，仅供后续实施，本轮未执行。** 未列出的文件不得修改（主 plan §9 规则 1）。

| 批次 | Task | 标题 | owner 产出文件 | 依赖 | 验证命令（精确过滤器） |
| --- | --- | --- | --- | --- | --- |
| W0 | **V-06（主 plan）** | 迁移前基线 + host 侧 wire 夹具（A10/A12） | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（新）、acceptance「迁移前基线」小节 | — | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture` |
| W1 | **I-01** → **E-01/E-02** | builtin 声明表（数据：含 `direct` / `prompt_declaration` / 保留名 / 关键字面量）+ effective name 规则（行为） | `peri-acp-types/src/builtin_mcp.rs`（新，E-01）、`builtin_mcp_test.rs`（新，E-01）、`peri-acp-types/src/lib.rs`（E-01）、`peri-middlewares/src/mcp/builtin/mod.rs` + `builtin_test.rs`（E-02） | — | `cargo test -p peri-acp-types --lib -- builtin_mcp`；`cargo test -p peri-middlewares --lib -- mcp::builtin::tests`（**禁止**裸 `mcp::builtin`） |
| W1 | **I-02** → **E-01**（**去掉新增字段部分**，R13） | `ConfigSource::Builtin { instance }` + `TransportConfig::Builtin` + 三分类超时 + typed 未知实例错误 | `peri-acp-types/src/plugin.rs`、`peri-middlewares/src/mcp/transport.rs`、`mcp/transport_test.rs`、`mcp/discover_tool.rs`、`discover_tool_test.rs`、`mcp/mod.rs`（1 行 `mod builtin;`） | — | `cargo check --workspace --all-targets`；`cargo test -p peri-middlewares --lib -- mcp::transport` |
| W3 | **I-03** → **I-02**（R3/R14） | step 6.5 overlay + 六条覆盖规则（含 A3/A17/A18）+ `BuiltinInjectionPolicy` + 12 处 `_with_paths` 补参 + 写回隔离 | `peri-middlewares/src/mcp/config.rs`、`config_test.rs`、`mcp/builtin_apply_test.rs`（新）、`mcp/mod.rs`（测试挂载） | E-02 → | `cargo test -p peri-middlewares --lib -- mcp::config::tests`；`cargo test -p peri-middlewares --lib -- mcp::builtin_apply` |
| W2 | **两个 handler（本文第 4/5 号 task）** → **I-01** | Web / Artifact MCP handler（路径：`mcp/builtin/web.rs`、`mcp/builtin/artifact.rs`；**不是** `builtin/instances/**`）+ **IF-D14 结果映射** | `peri-middlewares/src/mcp/builtin/web.rs`、`artifact.rs`、`mcp/builtin/mod.rs`（子模块声明）、`middleware/web*.rs`、`artifact/*`（抽出可复用核心）、`mcp/tool_bridge.rs`（A9 的 `prompt_declaration` 透传，与 E-03 分时移交同一文件——先 E-03 后 I-01，或由同一 agent 顺序执行） | E-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin::web`；`cargo test -p peri-middlewares --lib -- mcp::builtin::artifact` |
| W2/W3 | **I-06** → **I-01 + I-03**（A6/A14） | 抽出可复用核心（I-01）+ 删除两个 middleware 与全部挂载点、再导出（I-03）+ **补三个工具面的 builtin 提供面**（§6.6） | I-01：`middleware/web*.rs`、`artifact/*`；I-03：`middleware/mod.rs`、`lib.rs`、`assembly.rs`、`assembly/preparation.rs`、`assembly/workflow.rs`、`mcp/middleware.rs`、`peri-acp/src/host/assemble.rs` | I-01 → I-03 | `cargo test -p peri-middlewares --lib -- middleware::web_search::tests`（**须 16 passed**）；`cargo test -p peri-middlewares --lib -- artifact::tool`（**须 12 passed**）；`cargo test -p peri-middlewares --lib -- assembly::tests` |
| W3 | **I-07** → **I-03 + S-02**（A7/A14） | 槽位与装配测试期望值（I-03）+ 两表 / `provider/config.rs` / 谓词 / fixture（S-02） | I-03：`peri-agent/src/session/factory.rs`、`assembly_test.rs`；S-02：`peri-acp-types/src/meta_harness.rs`、`peri-acp/src/provider/config.rs`、`config_test.rs`、`stage_builder/tools.rs`、`tools_test.rs`、`builder_v2_test.rs` | I-02（主 plan）→ | `cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-acp-types --lib -- meta_harness`；`cargo test -p peri-agent --lib -- session::exec::stage_builder`（**禁止** `-- session::factory`） |
| W3 | **I-08**（**改判**） | ~~遗留 MetaHarness 关闭键保留~~ → **两表改造**：`MIDDLEWARE_NAMES` 删两键 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS` + `provider/config.rs` known 集合与 `all_middleware_disabled` 改并集 | `peri-acp-types/src/meta_harness.rs`、`peri-acp/src/provider/config.rs`、`config_test.rs`（**owner = S-02**） | I-03 → | `cargo test -p peri-acp-types --lib -- meta_harness`；`cargo test -p peri-acp --lib -- provider::config` |
| W3 | **I-09** → **S-02** | 声明段**保留**收口（A9）：`build_real_direct_tools()` 改从声明表构造 + 新增「声明仍产出」用例 | `peri-middlewares/src/tool_search/declaration.rs`、`declaration_test.rs` | I-03 → | `cargo test -p peri-middlewares --lib -- tool_search::declaration` |

### 7.1 每个 task 的断言内容与文件清单

**I-01 — builtin 声明表**
文件：`peri-acp-types/src/builtin_mcp.rs`（新）、`builtin_mcp_test.rs`（新，挂 `#[cfg(test)] #[path = "builtin_mcp_test.rs"] mod tests;`）、`peri-acp-types/src/lib.rs`（1 行）。

形状（IF-D4 / IF-D13 / A3 / A9 的落地，**取代本节旧形状**）：

```rust
pub struct BuiltinMcpTool {
    pub original_name: &'static str,     // "WebSearch"
    pub effective_name: &'static str,    // 冻结字面量："mcp__web__WebSearch"（IF-D5）
    pub direct: bool,                    // IF-D13：wave 1 三个工具全为 true
    pub prompt_declaration: Option<&'static str>, // A9：逐字搬运既有模板
}
pub struct BuiltinMcpInstance {
    pub name: &'static str,
    pub instance: &'static str,          // 与 name 同值
    pub policy_key: &'static str,        // "WebMiddleware" / "ArtifactMiddleware"
    pub tools: &'static [BuiltinMcpTool],
}
pub const BUILTIN_MCP_INSTANCES: &[BuiltinMcpInstance];              // 仅已实现实例
pub const BUILTIN_RESERVED_INSTANCE_NAMES: &[&str];                  // A3：web/artifact/cron/lsp/workspace
pub fn find(instance: &str) -> Option<&'static BuiltinMcpInstance>;
pub fn is_reserved_instance_name(name: &str) -> bool;
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>; // IF-D15（A4）
```

- `effective_tool_name` 的**行为实现**在 `peri-middlewares/src/mcp/builtin/mod.rs`（E-02），**必须**调用 `tool_bridge::sanitize_name_component`（`tool_bridge.rs:56-66`）；`peri-acp-types` **不**实现 sanitize（IF-D4 冻结的依赖方向），只持有冻结字面量并供 IF-D15 查表。
- **`tool_bridge.rs` 不再改为委托**（R-F5 作废）：依赖方向是 `builtin::effective_tool_name → tool_bridge::sanitize_name_component`；`tool_bridge.rs` 在本波只做 IF-D13（E-03）与 A9 的 `prompt_declaration` 透传（I-01）。
- 本节的**遗留别名表相关条目全部作废**（R5/R15/R24：关闭语义由 `BUILTIN_INSTANCE_POLICY_KEYS` + 注册表 `policy_key` 承担，不需要别名表）。

断言（`builtin_mcp_test.rs`，过滤 `builtin_mcp`）：
1. `builtin_instances_declare_expected_tools`：表内容 == 冻结值（已实现实例数 2；`web` 的工具为 `WebSearch`/`WebFetch`；`artifact` 为 `artifact`）。
2. `effective_names_match_frozen_wave1_names`：三个 `effective_name` 字面量逐字等于冻结值（**恒等 sanitize，无字符被替换**）；E-02 侧另有一条与 `effective_tool_name()` 输出的比对测试。
3. `reserved_names_cover_implemented_instances`：`BUILTIN_RESERVED_INSTANCE_NAMES` ⊇ 已实现实例名，且 wave 1 恰为 `{web, artifact, cron, lsp, workspace}`；`is_reserved_instance_name("cron") == true` 而 `find("cron").is_none()`（预留不等于已实现）。
4. `declared_direct_and_declarations_are_complete`：三个工具的 `direct == true`、`prompt_declaration.is_some()`；`policy_key` 集合 == `{"WebMiddleware","ArtifactMiddleware"}`（与 S-02 的 `BUILTIN_INSTANCE_POLICY_KEYS` 对齐）。
5. `effective_name_lookup_is_exact`：`original_tool_name_of_effective("mcp__web__Fetch") == None`（小写/近似名不命中）、`("mcp__web__WebFetch") == Some("WebFetch")`、`("mcp__some_tool") == None`。
6. `effective_name_rule_single_source`（放在 `mcp/builtin/builtin_test.rs`，过滤 `mcp::builtin::tests`，**不在** `builtin_mcp` 内重复）：对 `sanitize_name_component` 与 `effective_tool_name` 用同一组含特殊字符的探针（`"a.b c"`、`"x/y"`、`"a-b_c"`、空串）逐一比对输出相等。
7. `instance_names_and_tool_names_are_nonempty_and_unique`。

**I-02 — 类型与 transport**
文件：`peri-acp-types/src/plugin.rs`、`peri-middlewares/src/mcp/transport.rs`、`transport_test.rs`；**不新增 `McpServerConfig` 字段**（R13）。
断言：
1. `config_source_builtin_variant_exists`：`ConfigSource::Builtin` 可构造、`PartialEq` 可用（来源判断分支必须能匹配）。
2. `builtin_source_is_not_wire_visible`：序列化 `McpServerConfig` 的输出**不含** `builtin` / `source` 键；反序列化任意 JSON（含 `"source": "web"`、`"builtin": "web"`）后 `source == None`——wire 不可声明 builtin 身份（`plugin.rs:108-110` 的 `#[serde(skip)]`）。
3. `transport_prefers_builtin_source`：`source: Some(ConfigSource::Builtin { instance: "web" })` → `TransportConfig::Builtin { instance: "web" }`（**即使同时有 `command`**，因为只有代码能构造该 source）。
4. `transport_stdio_http_branches_unchanged`：`source: None` + `command` → `Stdio`；`source: None` + `url` → `StreamableHttp`；`source: None` + 两者皆无 → `Err(InvalidConfig)`（既有语义逐位不变）。
5. `invalid_system_keys_still_rejected_before_transport`：`system_mcp_tools: Some([])` 但 `system_mcp: None` → `Err(InvalidSystemConfig(...))`（既有 `validate()?` 前置不变）。
6. `unknown_builtin_instance_is_typed_error`：未注册实例名走 `TransportError::UnknownBuiltinInstance`，错误文本只含实例名。
7. 既有 `mcp::transport` 用例全部保持通过（不得删改断言）。

**I-03 — 默认层与覆盖规则（六条）**
文件：`peri-middlewares/src/mcp/config.rs`、`config_test.rs`。
断言（全部落在 `mcp::config::tests`）：
1. `builtin_instances_are_injected_when_absent`：空的 global/plugin/project → 合并结果含 `web`、`artifact` 两条，`system_mcp == Some(true)`、`system_mcp_tools` == 声明 direct 集合、`source == Some(ConfigSource::Builtin)`、`protocol_version == None`。
2. `empty_user_entry_stays_system_and_direct`（**A17**）：项目级写 `{"web": {}}` → `source` 为 Builtin、`system_mcp == Some(true)`、`system_mcp_tools == 声明 direct 集合`（**不得**只填 source 就收工）。
3. `disabled_user_entry_is_disabled_not_fatal`：`{"web": {"disabled": true}}` → `source` 为 Builtin、`disabled == Some(true)`、`system_mcp` 保持缺省 → 注册为 `ClientStatus::Disabled`、不触发 `Err(SystemReadinessError::Disabled)`。
4. `disabled_plus_system_mcp_is_rejected`（**A18**）：`{"web": {"disabled": true, "system_mcp": true}}` → **加载期 typed error**，且有反例断言（不得走到 readiness 的 fatal）。
5. `reserved_name_with_command_or_url_is_rejected`（**A3**）：`{"web": {"command": "node"}}`、`{"artifact": {"url": "http://127.0.0.1:1"}}`、`{"cron": {"command": "node"}}` → 加载期 typed error（`ReservedBuiltinInstanceName`）；非保留名（如 `{"myserver": {"command": "node"}}`）**不受影响**。
6. `unimplemented_reserved_name_without_transport_is_not_injected`：`{"cron": {}}` → 不注入任何条目（不产生解析不到的 `TransportConfig::Builtin`）。
7. `declared_direct_matches_system_mcp_tools`（**A5/A17**）：每个实例的 `system_mcp_tools` == `declared_direct_tools()`。
8. `builtin_injection_does_not_change_hash_dedup`：注入前后既有 server 的 `manual_hashes` 去重结果一致。
9. `builtin_default_layer_never_overrides_user_fields`：用户显式 `system_mcp_tools: ["X"]`（无 command/url 且未 disabled）→ 以用户值为准（**不**被默认层覆盖），并据此在文档中声明该写法会让 direct 集合与声明表不一致（属于用户显式覆盖，必须由 test 名体现）。
10. `merged_config_still_validated_after_builtin_injection`：注入后仍过 `validate_config`，非法组合仍 `Err`。
11. `policy_none_injects_nothing`（**A2**）：`BuiltinInjectionPolicy::none()`（= `PERI_MCP_BUILTIN=off`/`0`）→ `pool.configs` 中**没有**任何 builtin 实例。

**Web MCP handler（本文第 4 号 task → 主 plan I-01）**
文件：`peri-middlewares/src/mcp/builtin/web.rs`（含其 `#[cfg(test)]` 测试模块；模块名以 E 的 `builtin/mod.rs` 声明为准，过滤器用 `mcp::builtin::web`）。**不要**新建 `instances/` 子目录。
断言（模块 `mcp::builtin::web`）：
1. `list_tools_returns_exactly_two_declared_tools`：`tools/list` 结果的名字集合 == `{"WebSearch","WebFetch"}`（顺序按声明表）；每个 `input_schema` 是 object 且含 `properties`。
2. `tool_from_base_maps_definition_fields`：`rmcp_tool_from_base(WebSearchTool)` 的 `name` / `description` 与 `definition()` 逐字段相等，schema 与 `parameters()` 相等。
3. `call_tool_dispatches_by_exact_name`：`call_tool("WebSearch")` 命中 `WebSearchTool`；`call_tool("websearch")`（大小写不符）与 `call_tool("unknown")` 返回错误。
4. `call_tool_error_is_not_a_success_result`：工具返回 `Err` 时不伪装成成功 content。
5. `server_does_not_override_discover`：**行为级**断言——该 handler 的 `discover()` 结果不是 `method_not_found`（用 `ServerHandler::discover` 直接调用，或在线路级用例中断言；见 §10 风险 R4 的两条实现路径）。
6. **不覆盖**：真实 HTTP 调用（`WebSearchTool`/`WebFetchTool` 会向 `tavily.claude-code-best.win` 发请求）**不得**在本层发起；本层用「缺失必填参数」等**在发请求前返回**的路径覆盖 `call_tool` 分发，其余网络行为沿用既有工具测试。若 owner 发现 `call_tool` 的 happy path 无法在不发网络请求的前提下覆盖，必须在报告里显式写出「本层未覆盖网络路径」，不得用 mock server 顶替（那会引入新的测试基础设施）。

**I-05 — Artifact MCP handler**
文件：`peri-middlewares/src/mcp/builtin/artifact.rs`（含其测试模块；过滤器用 `mcp::builtin::artifact`）。
断言（模块 `mcp::builtin::artifact`）：
1. `list_tools_returns_exactly_one_tool`：名字 `"artifact"`，schema 为 object 且 `required == ["file_path"]`。
2. `capability_root_comes_from_constructor_argument`：用两个不同 `cwd` 构造两个 handler，分别对相对路径 `a.md` 断言解析结果落在各自 `cwd` 下（复用 `ArtifactTool::resolve_path` 的可观察行为；若不可直接观察则在 handler 内暴露 `#[cfg(test)]` 访问器——**仅测试可见**）。
3. `call_tool_rejects_missing_file_path_before_any_network_call`：缺 `file_path` → 错误；该路径不发任何 HTTP 请求。
4. `call_tool_rejects_non_artifact_extension_before_upload`：`.txt` → 错误，错误文本含 `allowed: html, htm, md`（沿用 `tool.rs:51-60` 的既有文本），且**不含** url/token。
5. `error_messages_never_contain_credentials`：用 `PERI_ARTIFACTS_TOKEN` 注入一个哨兵值（**测试内自造的假值**，不是真实 secret）后触发失败路径，断言输出不含该哨兵。
6. **不覆盖**：真实上传（需要凭据与公网）。显式声明。

**I-06 / I-07 / I-08 / I-09 的断言**见 §6.4 / IF-F4 / §6.6 / §6.5（对应主 plan 的 I-01 / I-03 / S-02；**I-08 已改判为 S-02 的两表改造**），此处只补槽位与声明段两类断言：

**I-07 追加断言（A7 后的形态）**：
- `middleware_names_match_production_blueprint`（既有，`:1207-1217`）**改写**为三条同时成立：`{blueprint 槽位名} == MIDDLEWARE_NAMES`；`BUILTIN_INSTANCE_POLICY_KEYS == 声明表 policy_key 集合`；`槽位名 ∩ 策略键 == ∅`（等价于「两表并集 == 已知键全集」，强度不降）。
- `slot_middleware_name`（`:1220-1249`）删除 `ChainSlot::Web` / `ChainSlot::Artifact` 两臂；`slot_name`（`:453`/`:464`）与 `blueprint_sequence_is_canonical`（`:394`）同步。
- `default_config_produces_canonical_chain`（既有，`:472`）的期望序列删除两项，且**其余项的相对顺序逐项不变**（把该断言写成显式列表，不写成「删两个元素」的推导式，防止误删第三项）。
- 新增 `builtin_instances_replace_removed_middleware_tool_names`：对每个 `BUILTIN_MCP_INSTANCES` 的 `(instance, tool)`，断言 `MIDDLEWARE_TOOL_NAMES` **不含**裸工具名，且 `effective_name` 也**不在** `MIDDLEWARE_TOOL_NAMES` 中（effective name 属于 MCP 动态 bridge，永不进共享 registry）。
- 重写 `artifact_middleware_can_be_disabled_independently` → `artifact_builtin_instance_can_be_disabled_independently`：以 `disabled` 集合含 `"ArtifactMiddleware"`（**策略键**）的输入断言 `closed_instances` 命中 `artifact`，且 `SearchExtraTools`/`ExecuteExtraTool` 不受影响。
- `builder_v2_test.rs` 的 `test_build_session_tool_view_isolates_disabled_sessions`（`:24-55`）：fixture 名字从 `"WebFetch"`/`"WebSearch"` 换成一个**仍在** `MIDDLEWARE_TOOL_NAMES` 内、且不是迁移对象的名字（例如 `"LspMiddleware"` 对应的工具名或 `"TodoWrite"`），并保留「共享表含 middleware 工具名 → disabled 链不得泄漏」的原意；新增一条断言：共享表里名为 `"WebFetch"` 的条目在 disabled 链下**不**被剔除（IF-F5 的误剔反证）。
- **A6 三面断言**（见 §6.6）：顶层链 / `parent_tools.is_direct()` / workflow agent 工具列表三处，未关闭时含 `mcp__web__*` 且为 direct，关闭 `WebMiddleware` 时三处同时不含。

**I-09 断言（A9 重写后）**：
- `builtin_direct_tools_still_emit_prompt_declarations`：`collect_declarations` 对三个 builtin direct 工具产出非 None 文本，渲染后的名字为 effective name，且文本不含裸名（详见 §6.5）。
- `test_all_real_tool_declarations_render_without_placeholder_residue`（既有）的期望名单按新形态更新后仍通过，且 `0 tests` 不成立。
- **删除**旧断言 `builtin_mcp_tools_have_no_prompt_declaration_yet`（与 A9 直接冲突，不得保留）。

## 8. 执行批次与并发

| Wave | 并发 task | 同 crate 冲突 | 串行原因 |
| --- | --- | --- | --- |
| **W0（闸门 + 基线）** | V-06（主 plan：host 夹具 + 迁移前基线） | `peri-acp` | 起始 `cargo check --workspace --all-targets` 必须绿；迁移前基线必须在任何生产改动前录下（A12） |
| **W1（主 plan）** | E-01、E-02（**同一 agent 连续执行**）、S-05/S-06（纯文本） | `peri-acp-types` + `peri-middlewares` | E-01 落 `TransportConfig::Builtin` 后 `initialize.rs:275` / `reconnect.rs:90` 的穷尽 match 立刻编译失败。~~「I-02 加 `builtin` 字段引发 literal 收口」作废~~（R13：不新增字段，因此**没有** struct literal 收口工作量） |
| **W2** | 两个 handler task 合并为 **I-01**（主 plan 把两个 handler 放在同一个 task）；本文件 I-03（overlay）实际落在主 plan **I-02（W3）** | `peri-middlewares/src/mcp/builtin/` | 主 plan §7 W2 与 E-03、S-01 三并发；文件互斥。`mcp/tool_bridge.rs` 由 E-03 与 I-01 分时移交（先 E-03 的 IF-D13，后 I-01 的声明透传），不得并发 |
| **W3（主 plan）** | I-02 → I-03（串行）、S-02、S-03/S-04、S-08 | `peri-middlewares`（I-02、I-03）+ `peri-agent`/`peri-acp`（S-02、S-04）+ `peri-tui`（S-08） | I-03 **必须**在 I-02 之后（先让 builtin 注入可用，再删除 middleware 提供面，否则出现能力真空窗口）；S-02 依赖 I-03（静态表与链终态一致）；**A6 的三个工具面在 I-03 内一次做完**（否则出现「只改两条路径」的半迁移） |
| **W3** | I-08（**改判为两表改造，owner S-02**）、I-09（**声明段保留，owner S-02**） | `peri-acp-types` / `peri-acp`（I-08）、`peri-middlewares`（I-09） | 文件互斥；两者都依赖 I-03 的链终态 |

**与 sub-plan E 的排序约束（已按主 plan §7 收敛）**：两个 handler task（→ I-01）依赖 E-02 的 `effective_tool_name` 与注册表数据；I-01 **不得**先删 middleware 提供面（那会让 Web/Artifact 能力在 E-03 落地前出现真空窗口）。主 plan §7 W2 的闸门保证该顺序。

## 9. 验收矩阵（本 sub-plan 的诚实分级）

| 交付判据 | 主责任务 | 断言层次 | 证据强度 |
| --- | --- | --- | --- |
| **迁移前基线对照（A12）** | V-06（主 plan，W0）+ V-02 | host seam 同一夹具的两个时点：迁移前 = 裸名 `WebSearch`/`WebFetch`/`artifact`（迁移前均 direct）；迁移后 = 三个 effective name | **强（两时点对照）**；缺基线则本行降为 PARTIAL |
| 三个 frozen effective name 出现在首个 LLM 请求且为 direct | I-02（配置）+ I-01（handler）+ E（transport） | host seam（`peri-acp`，counting model 断言首个请求 `tools`） | **强**（依赖 V-06 夹具与 V-02 断言） |
| 四个挂载点消失、`ChainSlot` 处置完成 | 主 plan **I-03** | 编译期（类型消失）+ 锁定测试 | **强**（类型删除无法静态绕过） |
| **三个工具面能力等价（A6）** | 主 plan **I-03**（+ E-03 的类型化构造） | assembly 测试三面断言（顶层链 / `parent_tools.is_direct()` / workflow agent 工具列表）+ V-02 的首个请求 | **强（crate 内 + host 复证）**；旧结论「workflow 净丢失 Web 能力」已作废 |
| 策略键不静默失效（A7） | S-02（两表 + `provider/config.rs`） | 配置层 + `validate_meta_harness` 单测 + `closed_instances` | **中强**：单测能证明键不被丢弃、实例进关闭集；「关闭面全消失」需 V-01/V-02 的运行时用例 |
| 两实例隔离（A13 口径） | E + V-01 | crate 内：`!Arc::ptr_eq` + `McpConnectionKey` 不同 / 重连 generation 只动 `web` / 关闭 `web` 后 `artifact` 仍能 `tools/call` / per-instance wire 观测面（无则 UNVERIFIED） | **PARTIAL**：capability root 与凭据在 builtin 形态下**不可证伪**（`capability_profile` pool 级 `apps.rs:229-268`、`bind_execution_cwd` pool 级 `initialize.rs:161`）⇒ 按 UNVERIFIED 记录，**不得**用本文件的绿色单测声称已验证 |
| ARC-CAPABILITY-CLOSURE-001 关闭面 | 主 plan **I-02/I-03 + S-01/S-02 + S-08 + V-01** | 逐面见 §6.3 与 G §6.10 | **中强**：11 个关闭面都有承担机制与断言落点；面 4（TUI）归 S-08（A8 已在范围）；面 5 的声明段由 S-02 断言**仍产出**（A9，不再是缺口） |
| 保留名与非法关闭片段（A3/A18） | I-02（`mcp::builtin_apply`）+ S-01（反例） | 配置层 typed error + 反例用例 | **强**：加载期失败可直接断言 |
| 声明段保留（A9） | S-02 | `tool_search/declaration` 的「仍产出 + 渲染名字正确」断言 | **中强** |
| cancel 传播到 builtin 工具体 | — | **未实施** | **UNVERIFIED**：`WebFetchTool` 完全忽略 `ToolContext`（`fetch.rs` 的 `_ctx`），`ArtifactTool::invoke` 亦然（`tool.rs:161` 的 `_ctx`）。可观察契约停在 MCP 请求/服务层（spike Q2 的 duplex EOF 收敛），**不是**工具体内部的 cancel 响应。不得声称已实现 |

## 10. 风险与未知

| # | 风险 | 影响 | 缓解 |
| --- | --- | --- | --- |
| R1 | builtin handler 覆写 `discover` | 工具发现在生产链路上被 `-32602` 拒绝（§2.3 实测） | IF-F0 A + 「线路序列不含 `initialize`」的回归断言；违反时立刻回退到 IF-F0 B |
| R2 | ~~IF-F1 与 IF-D3 冲突~~ | **已解除**：主 plan IF-D3 的规则 2（只填 `source`）已覆盖「`disabled: true` 可关闭」，R14 明确覆盖本文 IF-F1 | 无需行动；本文 IF-F1 保留为推理记录 |
| R3 | `builtin` 字段引发跨 crate 的 literal 收口工作量被低估 | W1 卡住，其它 task 无法验证 | 与 v4-part-1 的 A-01/A-02a 同法：同一 agent 串行；W0 基线闸门；收口清单以 `cargo check --workspace --all-targets` 为准 |
| R4 | 「`discover` 不被覆写」在单测层的可断言性 | 断言可能退化为「实现细节的镜像」 | 两条可选路径（owner 择一并写明）：① 直接调用 `ServerHandler::discover` 断言 `is_ok()`；② 在线路级夹具里断言 method 序列不含 `initialize`（复用 `builtin_spike_test.rs` 的 `MethodTap` 形态） |
| R5 | ~~迁移后 Web/Artifact 的逐工具 prompt 指引消失~~ | **已由 A9 裁决关闭**：声明**不得**静默丢失 | 机制 = 声明表 `BuiltinMcpTool::prompt_declaration` + builtin direct 桥实现 `prompt_declaration()`；断言见 §6.5；owner **S-02** |
| R6 | `web_test.rs` 16 用例因失挂载而静默停跑 | 假绿 | 主 plan I-01 的验证命令强制报 16 passed（`middleware::web_search::tests`） |
| R7 | 用户 hook / agent `tools:` / `--disallowed-tools` 写 `"WebFetch"` 的行为 | 迁移后若只按 effective name 匹配，用户既有配置会静默失效；反向若只归一，用户的 `mcp__web__*` 写法又会失效 | **A4 ⑥⑦ 的匹配型归一（原样优先，未命中再用原始名）**：两种写法都可用，owner **S-01**（`hooks/matcher.rs`）/**S-02**（`ToolFilterPolicy::canonical`）；`docs/reference/mcp-ecosystem.md`（owner **V-05**）说明两种写法与推荐形态 |
| R8 | `MIDDLEWARE_TOOL_NAMES` 删除后误剔面改变 | 同名非 middleware 工具**恢复**可见（预期行为） | I-03/S-02 的反证用例（IF-F5 + 主 plan IF-D7 B 节的 `tools_test.rs` 反向断言） |
| **R9** | ~~主 plan 内部未收口冲突（保名单 + 删槽位）~~ | **已由 A7/C10 裁决**（采用候选处置 ② 的强化版） | `MIDDLEWARE_NAMES` **删**两键 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS`；`assembly_test.rs:1207-1217` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == `policy_key` 集合」∧「交集为空」；`provider/config.rs` 的 known 集合与 `all_middleware_disabled` 改两表并集（owner **S-02**） |
| **R10** | 本文原先禁止在隔离用例中使用 `transport_type` 区分 builtin/stdio | 已被 R16 推翻（`"builtin"` 成为第三分类） | 无语义风险；但 `mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言仍**不得改动**，夹具 env 改动按 A11/R28 登记 |
| **R11** | 保留名被外部 server 接管 / 非法关闭片段导致全局 fatal（A3/A18） | 审批门被静默移除；或所有 session 被 `Err(Disabled)` 阻断 | 加载期 typed error + 反例用例（owner **I-02** + **S-01**）；`PERI_MCP_BUILTIN=off` 的语义（A2）必须写进 acceptance 与用户文档（V-05） |
| **R12** | 只改一到两个能力路径导致半迁移（A6） | 子 agent / workflow agent 净失去 Web 能力，或被静默降级为 deferred 而链无 ToolSearch | §6.6 的三面断言 + §8 W3「I-03 内一次做完」+ 主 plan §10 R3/R4 |

**待核实项**（本文件不据此下结论）：
1. `/artifacts` slash 命令是否存在（§6.3 末）。核实方法见该节。
2. `mcp/builtin/` 的最终目录布局：已由主 plan §4/E 冻结为 `mcp/builtin/{mod.rs, runtime.rs, web.rs, artifact.rs}` + 各自 `*_test.rs`（IF-E2 的旧「开放问题」表述作废）。
3. ~~`CallToolResponse` 的错误映射二选一~~ **已关闭**：按 **IF-D14**（A15，owner I-01）冻结为「工具 `Err` → `Ok(CallToolResponse::Complete(CallToolResult::error(...)))`；未命中工具名 → `Err(McpError::invalid_params(...))`」。
4. ~~是否需要 `"builtin"` 的 `transport_type`~~ **已关闭**：IF-D11（R16）要求三分类，由 E-03 实现。
5. host 侧证据（首个 LLM 请求 / 审批 + wire）依赖 **V-06** 新建的 `peri-acp/src/host/mcp_v4_wire_fixture.rs`；`mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT`（无 wire 日志、无 `tools/call` 分支）**不得**复用（A10）。

## 11. 非目标

- **不**迁移 Cron / LSP / Workspace，不新增其余实例；三者的名字只作为**保留名**登记（A3）。
- ~~**不**为本波的三个工具提供 prompt declaration 模板~~ **作废（A9）**：本波**必须**保留声明（§6.5）。
- **不**改 `McpToolBridge::aliases()`（保持空）——即**不**让裸名 `WebFetch` / `artifact` 在迁移后仍可解析为可调用名字。理由：别名是「旧名 → 本工具」的解析机制，会给模型两个可调用的名字并放大审批面；且 `resolve_target` 在多候选时返回 `ambiguous tool invocation`（`peri-agent/src/tools/invocation.rs:79-104`）。**注意区分**：A4 的归一只影响**判定与匹配**（审批、过滤、hook、TUI 展示、参数别名），不产生第二个可调用名字。
- `docs/reference/mcp-ecosystem.md` 由 **V-05** 持有（A19）；`docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 同归 V-05。`docs/design/**` **不**回填（非目标不变）。
- **不**改 `example/minimal/**`（IF-F6 的遗留键路径样本仅作阅读参考；两键已迁到 `BUILTIN_INSTANCE_POLICY_KEYS`）。
- **要改** `client/status.rs` 的 `transport_type` 推导（IF-D11 / R16：新增 `"builtin"` 三分类，owner E-03）；`mcp_isolation_contract.rs` 的 `"stdio"` 断言不改。
- **不**做 `McpServerConfig` 的字段级深合并（IF-D3 规则 2：无 command/url 时**只填**声明字段，其余以用户值为准）。
- **不**在本波实现 builtin 工具体内的 cancel 响应（§9 末行）。
- **不**在消费点硬编码 `mcp__web__*` 或新增第二张反查表（A4/A8）。

## 12. 对 IF-D1…IF-D9 与新增冻结接口的落地结论

> 本节覆盖 `IF-D1…IF-D9`，并补 `IF-D13/IF-D14/IF-D15`（本轮新增）。本文的旧装配编号（数值 4）一律读作 **`I-03`**（A14）。

| 冻结接口 | 结论 | 落地位置 / 偏差 |
| --- | --- | --- |
| **IF-D1** `TransportConfig::Builtin { instance }` + 共用 `initialize_config` | **成立**。`TransportConfig` 是 `peri-middlewares/src/mcp/transport.rs:10-22` 的枚举，`initialize_config` 是唯一接线链路。主 plan 已把判定来源冻结为 `config.source == Some(ConfigSource::Builtin { .. })`，并要求新增 `TransportError::UnknownBuiltinInstance` 与三分类超时（`BUILTIN_CONNECT_TIMEOUT = 5s`） | 落地 **E-01**（变体 + 判定 + 错误 + 超时）；transport 装配分支与 **sub-plan E** 的 IF-E1 对接。~~IF-F2 的新字段方案作废~~（R13） |
| **IF-D2** `ConfigSource::Builtin { instance }` | **成立**，但**是带载荷变体**（`Builtin { instance: String }`），本文原先假设「无载荷 unit 变体」**不成立**。`ConfigSource` 在 `peri-acp-types/src/plugin.rs:24-31`，无 serde derive，因此新变体不进任何序列化路径 | **E-01**。传播面（穷尽）：`mcp/discover_tool.rs` 的 `config_source_str` 新臂、`client/status.rs` 的 `transport_type`、`McpClientHandle.source` |
| **IF-D3** builtin 默认层最低优先级 / 可覆盖 / `disabled: true` 可关闭 | **主 plan 已给出可行且更简洁的规则；本文原先的「部分不成立」判断被 R3/R14 取代** | 主 plan IF-D3 的**六条**：注入点 step 6.5（`:419-430` 之后、`:433` 之前）、`BuiltinInjectionPolicy` 显式参数、缺失→插入、无 command/url 且未 disabled→填 `source`+`system_mcp`+`system_mcp_tools`（A17）、`disabled: true`→只填 `source`、保留名声明 command/url→typed error（A3）、非法关闭片段→拒绝（A18）。落地 **I-02**。本文 IF-F1 作废为推理记录 |
| **IF-D4** builtin 注册表（纯数据 + 行为分离） | **成立且已核实落点**。数据 = `peri-acp-types/src/builtin_mcp.rs`（`name` / `instance` / `policy_key` / `tools` + `find`）；行为 = `peri-middlewares/src/mcp/builtin/mod.rs`（`effective_tool_name` / `with_builtin_defaults` / `closed_instances` / `is_closed`） | **E-01（数据）/ E-02（行为）**。本文原先主张「把 `effective_tool_name` 放进 `peri-acp-types` 并让 `tool_bridge.rs` 委托」**不采用**：主 plan IF-D4 冻结的是反向依赖——`mcp::builtin::effective_tool_name` **必须调用** `tool_bridge::sanitize_name_component`，同样只有一份规则实现 |
| **IF-D5** `mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact` | **成立（与主 plan 独立核算结果一致）**。已用 `sanitize_name_component`（`tool_bridge.rs:56-66`）逐字符核实：`web` / `WebSearch` / `WebFetch` / `artifact` 全部只含 `[A-Za-z0-9_-]`，**无字符被替换** | **E-02** 的冻结字面量测试（`mcp::builtin`） |
| **IF-D6** 策略一致性 | **成立，但有一条必须显式记录的后果** | 细则归 **sub-plan G / S-01**。F 侧只需知道：`mcp__artifact__artifact` 的判定将等于对 `artifact` 的判定，而 `artifact` **当前不在** `default_requires_approval` 清单中（`peri-middlewares/src/permission/mod.rs:44-59`）→ 迁移后 artifact 的审批结论为**不需审批**。这与「今天 `artifact` 也不需审批」等价（不是回归），但**不等于**「`mcp__*` 前缀的保守语义」——必须由 G 的显式断言与 acceptance 记录，不能靠推断。G 必须同时保留「未知 `mcp__*` 仍为 true」的反证 |
| **IF-D7** 静态表与投影同步 | **成立，但主 plan 的取向与本文相反且已裁决**：`MIDDLEWARE_NAMES` **保留**两键（改注释为 builtin 关闭键）、`MIDDLEWARE_TOOL_NAMES` **删除**三个裸名、`stage_builder/tools.rs` 谓词重新论证、`builder_v2_test.rs` fixture 换名、`tool_projection.rs` 新增 Fetch 分支、`TOOL_PARAM_ALIASES` 归一 | **S-02**（`meta_harness.rs` + `stage_builder/*` + `invocation.rs` + `tool_projection.rs`）。本文 IF-F5（三个裸名必须删除且需反证）**仍然成立**，是 IF-D7 的直接推论 |
| **IF-D8** 槽位与挂载点删除 | **已冻结且与本文一致**（删除 `ChainSlot::Web` / `ChainSlot::Artifact` 与 blueprint 两项、4 个挂载点、`lib.rs`/`middleware/mod.rs` 再导出） | **I-03**。**新增义务**：必须断言「过滤掉被删两项后，本批次 blueprint 与上一批次 blueprint 逐项相等」。⚠ **未收口冲突**：IF-D7 保留 `MIDDLEWARE_NAMES` 两键与 IF-D8 删除槽位，会让 `assembly_test.rs:1208 middleware_names_match_production_blueprint`（断言集合相等）失败——见 §10 R9 |
| **IF-D9** `system_mcp: true` + `system_mcp_tools` 保持可见性等价 + 打通 direct 审批链缺口 | **成立，且是本波的最大收益点**。主 plan 追加 `PERI_MCP_BUILTIN` 紧急闸门（只作发布回退，不得成为按工具粒度的策略开关） | **I-02** 写默认层；direct 提升完全复用 v4-part-1 已落地的 `prepare_system_tools` + `replace_static_mcp_tools` + `before_react_start`，**F 不新增注入代码**。真实审批链的运行时证据归 **V-01（crate 内）+ V-02（host seam）**（v4-part-1 acceptance §4 的 BLOCKED 项） |
