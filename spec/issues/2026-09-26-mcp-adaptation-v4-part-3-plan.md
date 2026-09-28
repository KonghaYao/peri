# MCP adaptation v4-part-3 — 主实施计划（wave 2：Cron / LSP 实迁为 builtin MCP 实例）

> 2026-09-28 范围更新：`local-mcp-server` 已按用户裁决退役，代码与独立构建入口已删除。本文涉及旧项目的路径、命令、比较和后续复用建议仅作为历史记录，不再作为实施或验收要求；当前 Workspace MCP 入口与验证见 [主项目代码索引](../../docs/code-index/peri-middlewares.md)。

> 日期：2026-09-26。状态：**v3.1（事实源核查 A 台账已整合；A30–A33 收口已整合；采信 HEAD `a81e0ba6`）**（对抗核查 B「设计可行性」裁决 A20–A29、A30–A33 与事实源核查 A 已整合；裁决索引见 §0）。本文件是裁决唯一事实源；本状态只表示计划裁决已修订，不表示生产迁移或验收已完成。代码未实施，W0 基线已落地（`b1651aee`）。行号漂移时以**符号名 + repository-relative 完整路径**为唯一索引，行号仅作采信 HEAD `a81e0ba6` 的辅助标注。
> **实施回填（2026-09-28）**：本波（wave 2）代码已实施并验收；现场证据见 `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-acceptance.md`（状态「就绪」；未闭合项与未落地 gap 逐条登记在 §7）（交付提交 `48ea61cb`；分支 `feat/mcp-adaptation-v4-part-3`）。
>
> 目标事实源：`docs/design/mcp-adaptation-v4-part-1.md`（下称「设计文档」，目标契约事实源）；本波裁决、批次编排与接口冻结以本文件为准，显式偏离见 §5 / §10。现场证据只记入 acceptance，不回填为设计裁决。
>
> 上一批次：`spec/issues/2026-09-26-mcp-adaptation-v4-part-2-plan.md`（下称「part-2 plan」，wave 1 = Builtin MCP 运行时 + Web/Artifact 实迁），其现场证据在 `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`。
>
> 本波次范围（用户裁决，2026-09-26）：**只做 `cron` 与 `lsp` 两个实例**；`workspace` 单独成波（wave 3），本文件只登记其依赖与本波为其预留的接口缺口。
>
> 三态口径沿用 part-1/part-2：「目标归属」（设计文档）≠「当前实现」（代码事实）≠「本次运行时证据」（命令 + exit + 测试数）。绿色局部单测不得升级为整体迁移结论。
>
> 工作现场：worktree `/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3`，当前计划修订采信 HEAD `a81e0ba6`（W0 基线记录提交 `b1651aee`）；W0 现场证据采集于 acceptance 记录的生产基线 `226fa2bc`。

## 0. 裁决记录 A1–A33（复核用索引）

> 口径：本文件是裁决唯一事实源；本节「落点」索引 §3（冻结接口）、§5（覆盖登记）与验收责任。§5 中标注的 wave 1 A 号属于 part-2 plan，不是本节 A 号。

| 裁决 | 内容摘要 | 本文件落点 |
| --- | --- | --- |
| **A1** 状态归属与注入方向 | cron scheduler / lsp pool 由**组合根构造**并以 `Arc` 注入 builtin 实例（同一状态对象，非第二份）；实例是**唯一 MCP 工具适配器**。宿主仅经冻结端口接收事件、投影 UI、同步文件；关闭经显式生命周期操作，不依赖最后一个 `Arc` 释放。被否方案见 §0.2 IF-D20 | §3 IF-P3-04/06/09；§5 R1/R25；§8 第 6/15 行 |
| **A2** tick 归属 | cron 1s tick 移入每一代 builtin server 的**代监督者**（A32：`BuiltinInstanceSupervisor`，**不在 handler 内**），显式 cancel + 有界 join，`Drop` 仅兜底；宿主注入 `tick_enabled`，保留 print/stdio 无 tick 差异 | §3 IF-P3-05；§5 R18/R25；§6 H-02a/C-02/C-03；§8 第 7 行 |
| **A3** 事件出口路线 | 采用**路线 B**，复用 `CronSchedulerPort::subscribe`；本波不引入 MCP `notifications/*` 或 server→client 推送，路线 A 的代价（接线、生命周期与审批通路）单独登记 | §3 IF-P3-06；§5 R1；§8 第 6 行；§10 第 1 项 |
| **A4** `direct` 与声明段 | cron 3 工具与 lsp 1 工具一律 `direct: false`；`prompt_declaration: None`，声明段零变化 | §3 IF-P3-01/02；§5 R6/R9/R16；§8 第 3/4/12 行 |
| **A5** `system_mcp` 声明 | 两实例 `system_mcp: Some(true)` + `system_mcp_tools: Some([])`：保留 1R readiness，零工具提升 | §3 IF-P3-03；§5 R24/R26；§8 第 2/4 行 |
| **A6** LSP 工具面条件暴露 | **builtin enabled 且未被配置关闭**时，`lsp` 总是注入（overlay 不感知 LSP 配置域）；工具面按生效配置非空（`has_servers()`）构造快照，无配置返回空列表，不要求 language server 进程就绪 | §3 IF-P3-03 规则 5、IF-P3-08；§5 R24；§8 第 5 行 |
| **A7** 同步中间件归属 | 保留薄 `LspSyncMiddleware` 占用原 `ChainSlot::Lsp`；`after_tool` 经既有 `LspPoolPort` 同步（A23）。`LspMiddleware: false` 关工具面 + 同步目标；`LspSyncMiddleware: false` 只关同步；均非物理销毁 | §3 IF-P3-09；§5 R1/R15/R23；§8 第 8/10 行 |
| **A8** 两表迁移 | `MIDDLEWARE_NAMES` 删 `CronMiddleware` / `LspMiddleware`、增 `LspSyncMiddleware`；`BUILTIN_INSTANCE_POLICY_KEYS` 增前两者；`MIDDLEWARE_TOOL_NAMES` 删 `LSP`，cron 三工具不入表 | §3 IF-P3-10；§5 R8/R21；§8 第 9/24 行 |
| **A9** 分派文件重构 | 枚举与工厂由 `peri-middlewares/src/mcp/builtin/web.rs` 迁入 `peri-middlewares/src/mcp/builtin/dispatch.rs`；H-01 负责搬迁且单一持有文件，H-05 负责集成阶段（仍由 H-01 写入） | §3 IF-P3-07；§5 R11/R13/R25；§6 H-01/H-05；§8 第 15/20 行 |
| **A10** 实例上下文载体 | 工厂改为 `(instance, &BuiltinInstanceContext)`；`spawn_builtin_transport` 由 pool 方法承载，initialize/reconnect 不扩签名；可见性与一次性注入按 A26 | §3 IF-P3-04；§5 R25；§6 H-02/H-03/H-04；§8 第 2/15 行 |
| **A11** LSP pool 作用域 | per-session ⇒ per-host（root_uri = host cwd）；单 cwd 逐字等价，多 cwd 为已裁决的**功能退化**（A22），`session/delete` 不再关 pool | §5 R19；§8 第 13 行；§10 第 2 项 |
| **A12** LSP 同步内容传递 | 文件内容由**薄中间件**结合工具名与冻结的 cwd/内容来源读取后传入端口；端口只做协议工作，不新增文件系统 capability root；`after_tool` 现有 hook 没有 `ToolContext`，不得假定可直接持有 | §3 IF-P3-09；§5 R1/R23；§8 第 8/10 行 |
| **A13** 关闭集四面 | 沿用启动 required / deferred 目录 / subagent `parent_tools` / workflow agent 四个过滤面，不新增第五工具过滤面；五维生命周期矩阵另见 A24 | §3 IF-P3-03；§5 R23；§8 第 10 行 |
| **A14** 归一消费点 | 沿用 part-2 IF-D6 七处 + `tool_projection::infer_tool_kind`；list/remove/LSP 不审批、非 mutation，register 仍审批 + mutation | §3 IF-P3-11；§5 R5/R14；§8 第 11 行 |
| **A15** 既有缺陷登记（不修） | TUI 私有双 scheduler、print/stdio 无 tick、LSP 同步覆盖缺口三件保持现状，只登记；TUI 私有 scheduler/tick 不计入本波 MCP 共享 scheduler 的宿主 tick 验收 | §5 R22；§10 第 4 项；§8 第 7/14 行 |
| **A16** 不新增能力声明 | `server_info` 只声明 `tools`，不声明 `resources`、不启用订阅 | §3 IF-P3-12；§5 R24/R25；§8 第 2/15 行；§10 第 1 项 |
| **A17** 验证口径 | 三面对照按 A20 的重命名映射，不作字面集合相等；cron 触发与 LSP 同步端到端；`0 tests` 失败，非零旧测试也不等于闭合（A29） | §5 R7/R28；§6 V-01…V-06；§8 第 3/6/8/16/18 行；§9 规则 1/2/13 |
| **A18** 范围边界 | workspace、local-mcp-server、`peri-lsp` 行为重构、MCP 通知能力不在本波；A23 必需的端口适配不改变 `peri-lsp` 协议业务行为 | §1.2；§4；§5 R1/R19/R25；§11 |
| **A19** 文档同步 | 代码索引、MCP 用户参考、槽位表、`peri-middlewares/src/skills/builtin/skills/cron/SKILL.md` 及工具调用文本同步 effective name、关闭语义和退化现状；permission 敏感清单保留判定规则/14 项数量/顺序，但模型可见 Markdown 名称从声明表解析 effective name | §0.1 IF-P3-11；§5 R27；§6 S-05/V-05；§8 第 11/17 行；§9 规则 12 |
| **A20** 对照面改判 | 等价对照面 = 搜索面（`SearchExtraTools` / `ToolIndex`），判据为裸名 XOR effective name 的重命名映射；摘要因 `mcp__` 过滤只作裸名消失时点证据；直连参数、摘要、搜索/执行三面分列 | §3 IF-P3-02；§5 R7/R28；§8 第 3/16 行；§10 第 10 项 |
| **A21** LSP 门控谓词 | 生效配置非空（`has_servers()`），与进程就绪无关；handler 在配置合并之后构造并快照工具集（H-04）；配置热更新显式非目标 | §1.2；§3 IF-P3-03 规则 5、IF-P3-08；§5 R24；§8 第 5 行 |
| **A22** 多 cwd 退化登记 | 单 cwd 逐字等价，多 cwd 共享 host root_uri 为功能退化；acceptance「语义变更」与用户文档显式写明；host shutdown 有界关闭全部 language server 并观测资源；wave 3 统一经 `ToolContext` → MCP 调用上下文恢复 per-session | §5 R19/R27；§8 第 13/17 行；§10 第 2 项；§12 |
| **A23** 端口合流 | 不增第二 LSP 端口，同步并入 `LspPoolPort`；async、每文件 didChange→didSave、typed Err 由薄中间件 debug 降级且不影响工具结果；读文件只在薄中间件；形状不容须先追加裁决 | §3 IF-P3-09；§4；§5 R1/R23；§8 第 8/10 行；§11 |
| **A24** 关闭语义五维 | 四类关闭分开：MCP 配置 `disabled: true` 跳过连接；MetaHarness `policy_key = false` 只影响本 turn 投影，readiness 仍走 pool 配置；`PERI_MCP_BUILTIN=off` 不注入 builtin；物理 close 才停任务/释放资源。仅策略关闭保留 handler/pool/readiness；逐维矩阵须入用户文档及验收 | §3 IF-P3-03/04；§5 R23；§8 第 10/17 行 |
| **A25** tick 生命周期 | tick 归属代监督者（A32）：每代至多一个 `TickGuard{cancel, join}`，handler 不持 tick；close 显式 `shutdown().await`，Drop 仅兜底；同 scheduler 最多一个驱动，reconnect 先停旧再起新；关闭 >2×interval 无触发且 task 已 join，重连后每 interval 恰 1 tick | §3 IF-P3-05；§5 R18/R25；§6 C-03/H-02；§8 第 7/15 行；§10 第 7 项 |
| **A26** 上下文可见性与一次性注入 | `peri-acp` 经公开 assembly 工厂构造并注入上下文；必须在 initialize 前，重复返回 typed Err（首个生效），runtime 记录并拒绝；冻结 host shutdown / instance close / reconnect 三态保留与销毁对象 | §3 IF-P3-04；§5 R25；§6 H-02/H-03/H-04；§8 第 2/15 行 |
| **A27** owner 归并与任务重排 | `peri-acp-types/src/builtin_mcp.rs` 单 owner C-01；`peri-middlewares/src/assembly/lsp.rs`、`peri-middlewares/src/assembly_test.rs` 单 owner H-03；增 H-05（dispatch 集成，文件 owner H-01）、S-05（文档落地）、V-04 isolation 文件；任务连续 C-01…04/L-01…03/H-01…05/S-01…05/V-01…06 | §4；§5 R13/R15/R27/R28；§6/§7/§12 |
| **A28** 引用与措辞对齐 | 落点按 R 实际内容重建；三面定义、注入前提一致；被否方案改为本波不做、记录接线/生命周期/审批通路代价与复核点，不作协议不可行或必须改泛型 seam 的绝对断言 | §0/§0.1/§0.2；§3 IF-P3-02/03；§5 R7/R24/R26；§10 第 1 项 |
| **A29** 覆盖登记补齐 | 增 R23–R28；R10/R12/R15/R17/R20/R21 分别挂 §8 第 19–24 行；每行具名测试函数或 acceptance「新增/修改函数名 + 命令 + 计数」，非零旧测试不计闭合 | §5 R10/R12/R15/R17/R20/R21/R23–R29；§8 第 19–25 行；§9 规则 13 |
| **A30** LSP 同步契约冻结 | `LspPoolPort`（`peri-acp-types/src/ports.rs`）新增三个方法且**一律不给默认实现**（默认实现会让替身静默 no-op 制造假绿）：`fn ready_for(&self, path: &Path) -> bool`（读文件之前的唯一前置判定；`false` ⇒ 薄中间件不得读文件、不得发任何通知）、`async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspSyncError>`、`async fn did_save(&self, path: &Path) -> Result<(), LspSyncError>`；参数是文件系统 path 而非 URI（URI 转换留在实现内部）；`LspSyncError`（`NoServer` / `Protocol`）定义在 `peri-acp-types`，只进 debug 日志、不进模型面工具文本；同一调用内严格 didChange→didSave 且前者失败**仍尝试**后者；跨并发调用**不承诺**全局 FIFO（登记为已知限界，留 wave 3）；读文件只在薄中间件，端口不读文件、不新增 capability root、不解析工具输入；实现者矩阵四处必须显式实现并断言调用计数：`peri-lsp/src/pool.rs`、`peri-lsp/src/pool_test.rs`、`peri-acp/src/host/requests_test.rs`、`peri-acp/src/host/stdio/run_server_integration_test.rs` | §3 IF-P3-09；§4.1/§4.2/§4.5；§5 R1/R23；§8 第 8/10/21/23 行；§10 第 3 项 |
| **A31** 事实核查收口（F/K 项） | ① `BuiltinInstanceContext` 补「关闭集」字段（`closed_instances(&HashSet<String>) -> BTreeSet<String>`（`peri-middlewares/src/mcp/builtin/mod.rs`）的产物，随上下文由宿主装配传入）；② `peri-middlewares/src/mcp/builtin/mod.rs` 是模块挂载与上下文公开的唯一 owner（H-02）：`cron.rs`/`lsp.rs`/`dispatch.rs` 的挂载、`cron/middleware.rs` 删除与 `ChainSlot::Cron` 摘除必须在同一原子提交内完成，禁止用 `0 tests` 或 dead-code 豁免绕过；③ `peri-middlewares/src/lib.rs` 与 `peri-middlewares/src/lsp/mod.rs` 的旧 re-export（`CronMiddleware` / `LspMiddleware`）由 C-04 / L-03 删除并登记 §4；④ `peri-middlewares/src/mcp/client/lifecycle.rs` 由「待生命周期方案」升为 H-02 改动面（A32）；⑤ `McpToolBridge::invoke` 已使用 `ToolContext`（F1），缺口只在 builtin 共享 helper `invoke_tool_call`；⑥ `peri-agent/src/tools/invocation.rs:TOOL_PARAM_ALIASES` 已核无 cron/LSP 条目（零改动）；⑦ 端到端超时不对称已登记 R29 / §8 第 25 行；⑧ tick 验收只限驱动 MCP 的 ACP 共享 scheduler，禁止全仓 grep 零 tick | §3 IF-P3-04；§4.2/§4.3/§4.9；§5 R25/R29；§6；§8 第 7/15/25 行；§10 |
| **A32** tick 归属与代监督者 | tick **不放在 handler**：`BuiltinTransport.tick: Option<TickGuard>` + `into_parts()` 拆出 `(io, BuiltinInstanceSupervisor)`；`McpClientPool::spawn_builtin_transport` 是**唯一 spawn 点**；池表 `builtin_server_tasks` 的值类型由 `BuiltinServerTask` 换为 `BuiltinInstanceSupervisor`（`peri-middlewares/src/mcp/client.rs`）；`BuiltinInstanceSupervisor::close(timeout).await` 顺序固定：①`tick.take()` → `TickGuard::shutdown(timeout)`（`cancel()` → 有界 join，仅超时才 `abort()` + await）②server task `converge(timeout)` ③返回 `BuiltinCloseOutcome { tick, server }`，超时复用既有 `BUILTIN_CONVERGE_TIMEOUT`（`peri-middlewares/src/mcp/builtin/runtime.rs`，1000ms，不新造第二套策略）；`close_builtin_task` / `close_builtin_tasks`（`peri-middlewares/src/mcp/client/lifecycle.rs`）在 async 上下文内 await 收敛，**不新增第二条关闭路径**；`TickGuard::Drop` 只 `cancel()`（不 abort、不阻塞）；reconnect 顺序不变（`peri-middlewares/src/mcp/reconnect.rs` 先关旧代、后 spawn 新代），任一时刻同一 scheduler 至多一个 tick 驱动；机械清点（主 agent 已复核）：`register_builtin_task` 调用点 7 处（生产 `initialize.rs`、`reconnect.rs`；测试 `mcp/builtin/runtime_test.rs` ×5），`close_builtin_task(s)` 生产调用 6 处（`initialize.rs`、`reconnect.rs` ×2、`client/lifecycle.rs` ×3）+ 测试 4 处（`mcp/builtin/runtime_test.rs` ×3、`mcp/builtin_runtime_test.rs` ×1），返回类型消费点 `mcp/builtin/runtime_test.rs`、`mcp/builtin_runtime_test.rs`（`BuiltinServerExit` → `BuiltinCloseOutcome`） | §3 IF-P3-05；§4.2/§4.3；§5 R18/R25；§6 C-02/C-03/H-02；§8 第 7 行；§10 第 7 项 |
| **A33** 注入点与注入时序 | `BuiltinInstanceContext` 由**宿主装配** `peri-acp/src/host/assemble.rs`（H-04）构造并注入，必须早于 `McpClientPool::run_initialize(...)` 的调用（唯一生产调用点 `assemble.rs`）及其后台 spawn；`peri-middlewares/src/assembly/mcp.rs` **不参与**实例上下文注入（只做中间件装配与关闭集投影）；首次注入生效，重复注入（含同一 `Arc` 再注入）返回 typed `Err`，`initialize` 已开始后的首次注入同样拒绝（同一短锁保护 setter 与初始化标志，锁内禁 await，不能只依赖 `OnceLock::set`）；可见性只经 `peri_middlewares::assembly` 的公开类型/工厂，`peri-acp` 不得 `import peri_middlewares::mcp::builtin`；失败面沿既有 `insert_failed + commit_discovery_failure` 收口、`system_mcp` 闸门 fatal、不伪造 ready | §3 IF-P3-04；§4.3/§4.5；§5 R25；§6 H-02/H-04；§8 第 2/15 行；§10 第 8 项 |

## 0.1 设计决策校验记录（IF-P3-01…IF-P3-12 的代码事实核对）

采信 HEAD `a81e0ba6`；「成立」表示方案依据成立，不表示运行验收已完成。§0.1 与全文代码引用以符号名 + repository-relative 完整路径为索引，行号仅作辅助；拟新增契约按 §3 冻结。


| 接口 | 裁决 | 已核实依据（代码事实） | 修正 / 补充 |
| --- | --- | --- | --- |
| **IF-P3-01** 声明表新增两实例 | **成立（声明段语义需按 A4 落地）** | `BuiltinMcpInstance` / `BuiltinMcpTool` / `BUILTIN_MCP_INSTANCES` / `BUILTIN_RESERVED_INSTANCE_NAMES`（`peri-acp-types/src/builtin_mcp.rs`）为声明形状/实例/保留名；`tools_are_non_empty_and_unique_per_instance`（`peri-acp-types/src/builtin_mcp_test.rs`，由前一文件挂载）当前对所有实例都要求 `prompt_declaration` 模板，与新增 cron/lsp 的 `None` 冻结冲突 | C-01 增两实例与四个 effective name；四工具 `direct: false`、`prompt_declaration: None`；测试改为仅对 web/artifact 保持模板要求，并新增 cron/lsp 无声明断言，在 acceptance 记录具体修改函数 |
| **IF-P3-02** `direct: false` 三工具 + 单工具，三面契约 | **成立（按 A20 重写）** | `prepare_system_tools`（`peri-middlewares/src/mcp/system_tools.rs`）允许 required value 空数组；`McpMiddleware::prepared_static_bridges`（`peri-middlewares/src/mcp/middleware.rs`）的 `required.is_empty()` 只判无 system 依赖；`BaseTool::is_direct`（`peri-acp-types/src/tools.rs`）默认 false；W0 已实测首个直连表 18 项，摘要面过滤 `mcp__`，搜索面收录 effective name | 三面分列：①首个请求直连参数不含四工具；②摘要面只作裸名消失时点证据，迁移后 effective name 因 `format_deferred_list` 的 `!name.starts_with("mcp__")` 整体缺席；③搜索/执行面以 `SearchExtraTools` / `ToolIndex` 的裸名 XOR effective name 重命名映射为等价判据，不作字面集合相等。subagent / workflow 的 direct 面仍不含四工具；`ExecuteExtraTool` 走 effective name |
| **IF-P3-03** 默认层注入与条件工具面 | **成立（按 A21/A24/A26 重写）** | `peri-middlewares/src/mcp/builtin/mod.rs:apply_builtin_overlay`：注册表遍历派生 overlay；`peri-lsp/src/pool.rs:LspServerPool::new` 惰性构造，`LspServerPool::has_servers` 只看生效 `servers` 非空；`peri-middlewares/src/mcp/initialize.rs:McpClientPool::initialize_config` 对 `disabled: true` 跳过连接；`peri-middlewares/src/mcp/middleware.rs` 的 MetaHarness `policy_key = false` 只影响本 turn 投影，readiness 仍走 pool 配置 | 1. **注入前提**：builtin enabled 且实例未被配置关闭时，两实例自动注入；四类关闭分开，不销毁实例、handler、pool的结论仅适用于策略关闭。2. `lsp` 总是注入，不由 overlay 感知 LSP 配置；handler 必须在配置合并后构造，工具集按 `has_servers()` 快照；无配置为空列表，有生效配置即出现，与 server 进程 ready 无关。3. 重复注入返回 typed `Err`，首个注入生效，runtime 记录并拒绝；注入必须早于 initialize。4. `system_mcp = true` 仍要求 1R ready；工具列表可为空 |
| **IF-P3-04** 实例上下文与 pool 承载 | **需修正（A26）** | `peri-middlewares/src/mcp/tool_bridge.rs:McpToolBridge::invoke` 接收并实际使用 `ToolContext`（`effective_tool_dispatcher` / `session_id` / `turn_generation` / `invocation_id` / `cancellation`）；真正缺口在 builtin 共享 helper `peri-middlewares/src/mcp/builtin/web.rs:invoke_tool_call`，其自行构造 `ToolContext::new(&[], cwd)`，未传入宿主 cwd/session 上下文；`peri-middlewares/src/mcp/builtin/mod.rs` 为 `pub(crate)`；`peri-middlewares/src/mcp/builtin/runtime.rs` 当前有自由 `spawn_builtin_transport(instance, cwd)`；`peri-middlewares/src/mcp/initialize.rs` / `peri-middlewares/src/mcp/reconnect.rs` 已持 pool | context 类型/工厂必须经 `peri-middlewares::assembly` 公开给 `peri-acp`，不直接公开 builtin 模块；pool 方法承载 spawn；注入早于 initialize，重复注入 typed Err、首个生效。生命周期冻结：host shutdown 销毁 host pool/handler/tick；instance close 销毁对应 handler/task、保留组合根状态；reconnect 先销毁旧 handler/tick、保留同一 scheduler/pool，再建新代；本波只注入 host cwd 与共享状态；宿主 session 调用上下文透传仍为 wave 3 缺口，不因 bridge 已用 ctx 就宣称 builtin 已收到 ctx |
| **IF-P3-05** tick 任务内化 | **必须新增（按 A25）** | 当前 MCP 共享 scheduler tick 是宿主 task（`peri-acp/src/host/assemble.rs:assemble_server_config_with_mcp_profile`；`peri-acp/src/host/task_scope.rs:HostTaskKind::CronTick`）；TUI 私有 scheduler/tick 位于 `peri-tui/src/app/cron_state.rs:CronState::spawn_tick_task` / `peri-tui/src/app/mod.rs`，明确排除 | `CronMcpServer` 每代持 `TickGuard{cancel, join}`；显式 `shutdown().await` 先 cancel 再有界 join，Drop 仅兜底；reconnect 先停旧代；仅删除驱动 MCP 所用共享 scheduler 的 ACP Startup / CronTick task。关闭 >2×interval 无新触发且 task 已 join，重连后每 interval 恰 1 次；禁止以全仓 grep 零 tick 作为验收 |
| **IF-P3-06** 事件出口 | **成立（路线 B）** | `CronSchedulerPort`（`peri-acp-types/src/cron.rs`）含 `subscribe` / `list_tasks` / `toggle` / `remove` / `as_any`；`SessionCronBridge::start`（`peri-acp/src/session/cron_bridge.rs`）经端口订阅；`assemble_server_config_with_mcp_profile`（`peri-acp/src/host/assemble.rs`）在 MCP 池前构造 scheduler | 采纳路线 B；`serve_client_auto`（`peri-middlewares/src/mcp/client/transport.rs`）支持可选 `ChannelHandler`，不能将当前宿主调用传 `None` 扩大为所有 pool 只能用默认 handler；路线 A 成本与复核点见 §10 第 1 项 |
| **IF-P3-07** 分派重构 | **成立** | `BuiltinServerHandler` / `builtin_server_handler`（`peri-middlewares/src/mcp/builtin/web.rs`）当前位于 web 文件；`spawn_builtin_transport` / `spawn_builtin_link`（`peri-middlewares/src/mcp/builtin/runtime.rs`）消费该工厂，现有泛型 seam 为 `H: ServerHandler` | 迁移到 `peri-middlewares/src/mcp/builtin/dispatch.rs`（新文件），`peri-middlewares/src/mcp/builtin/mod.rs` 挂载；`peri-middlewares/src/mcp/builtin/web.rs` 与 `peri-middlewares/src/mcp/builtin/artifact.rs` 只留各自实例与共享助手；`peri-middlewares/src/mcp/builtin/web_test.rs` 中「工厂只覆盖已实现实例」用例随工厂迁移 |
| **IF-P3-08** lsp 实例与工具面门控 | **必须新增（按 A21）** | 迁移前两道门：`add_lsp`（`peri-middlewares/src/assembly/lsp.rs`）的配置非空注册，`LspMiddleware::collect_tools`（`peri-middlewares/src/lsp/middleware.rs`）的 `has_servers()`；`LspServerPool::new` / `has_servers`（`peri-lsp/src/pool.rs`）分别惰性注册/检查生效配置 | 门控收敛到 handler 构造：谓词 = 生效配置非空，handler 必须在配置合并后构造并快照工具集；不要求 server 进程 ready，不支持配置热更新。`LSP_TOOLS` 静态声明与空 `system_mcp_tools` 不冲突 |
| **IF-P3-09** 同步端口 | **必须新增（施工前冻结）** | 唯一触发点 `peri-middlewares/src/lsp/middleware.rs:LspMiddleware::after_tool`；现有 hook 参数只有 `AfterToolState` / `ToolCall` / `ToolResult`，没有 `ToolContext`；`peri-acp-types/src/ports.rs:LspPoolPort` 当前只有 `as_any` / `shutdown`；`peri-middlewares/src/lsp/middleware.rs` 现行顺序是先按 path 找 ready server，再读内容，再 `did_change` + `did_save`，且 `did_change` 失败后仍继续 `did_save` | 不新增 `LspSyncPort`；§3 IF-P3-09 冻结完整 path 签名：同步 ready 查询 + async `did_change` / `did_save`，typed error 在 `peri-acp-types/src/ports.rs`；cwd 来自 `AfterToolState` 的 `StateView::cwd()`，ready 之后才读磁盘内容；change 失败仍 save；单调用有序、跨调用不承诺全局 FIFO（沿用现状，不新增队列）。§3 实现者矩阵逐一列出四个实现文件与 owner；禁止默认 no-op，验收必须断言替身调用计数。端口不读文件，薄中间件 debug 降级且不影响工具结果 |
| **IF-P3-10** 两表迁移 | **成立** | `BUILTIN_INSTANCE_POLICY_KEYS` / `MIDDLEWARE_NAMES` / `MIDDLEWARE_TOOL_NAMES` 与不变式 tests（`peri-acp-types/src/meta_harness.rs`）；后两表分别仍含 `CronMiddleware` / `LspMiddleware` 和 `LSP` | 另需：`peri-acp/src/provider/config.rs` 的 known 集合由两表并集派生（`NOT` 硬编码）⇒ 自动生效；`MIDDLEWARE_TOOL_NAMES` 的 cron 三工具缺口**不补**（迁移后它们从不是 middleware 静态工具） |
| **IF-P3-11** 归一消费点 | **成立（需补 1 项）** | `peri-middlewares/src/permission/mod.rs:default_requires_approval` / `is_edit_tool` 先归一；`peri-middlewares/src/permission/mod.rs:sensitive_tool_entries` 仍保留 `name: "cron_register"`，且该表直接生成模型可见 Markdown；同文件 `builtin_tool_effective_name` 已提供从声明表解析 effective name 的范式；`peri-middlewares/src/subagent/mod.rs:is_mutation_tool`；`peri-tui/src/kit/tool_display.rs` 与 `peri-tui/src/truncate.rs` 已通过 `original_tool_name_of_effective` 在原样匹配失败后重试，`peri-tui/src/truncate.rs` 的 `"LSP"` 是归一后共享分支 | 保留敏感判定规则、14 项数量与顺序；模型可见 Markdown 的显示名改为从声明表解析 effective name。S-03 只新增 Cron/LSP 注册表条目后的回归断言，不新增 TUI 归一入口。另核对 `peri-agent/src/tools/invocation.rs:TOOL_PARAM_ALIASES`；`ToolFilterPolicy::canonical` 已归一，无需改 |
| **IF-P3-12** 能力声明与隔离 | **成立（隔离范围收紧）** | `peri-middlewares/src/mcp/builtin/web.rs:server_info` 只声明 `tools`；`peri-middlewares/tests/mcp_isolation_contract.rs` 现有测试明确关闭 builtin 注入并使用两台 stdio fixture server，不能证明 Cron/LSP builtin handler/tick/state 隔离 | 新实例沿用 `server_info`；transport/task/state 按 §8 第 15 行隔离；V-04 必须保留 stdio 回归并新增真实 builtin 路径，禁止把 off 夹具升级为 builtin 隔离证明；capability root/凭据仍 **UNVERIFIED** |

### 0.2 被否方案记录（评审需看到「想过且否掉」）

| 被否方案 | 否掉理由（代码事实） |
| --- | --- |
| **IF-D20**：实例自建状态 + 向宿主回传 handle | **本波不做**：状态对象需在后台 initialize task 创建，而宿主更早需要 cron 端口，代价是异步可用性协议与竞态处理；组合根注入复用现有 `notifier` / `McpSubscriptionPort` 范式。**复核点**：若未来改为实例自建，必须先冻结异步发布、竞态和关闭归属 |
| **IF-D21**：cron 触发走 MCP 协议通知（路线 A） | **本波不做，非协议绝对不可行结论**（A3/A16）：`peri-middlewares/src/mcp/client/transport.rs` 存在 `ChannelHandler` 分支，且 `peri-middlewares/src/mcp/initialize.rs` 传递它；当前被引用的宿主启动路径传 `None`。路线 A 的否决理由是接线、生命周期与审批通路成本，而非协议不可行或必须改泛型 seam。复核点：未来若要求动态工具面或对外订阅，基于当时 rmcp 版本重新核对 `ServerHandler`、client handler、通知语义与审批门 |
| **IF-D22**：LSP 工具面改 `direct: true` | **本波不做**：会改变直连参数、提示词和搜索对照面；代价是迁移语义扩大。**复核点**：未来提升 direct 时重建 A20 三面对照 |
| **IF-D23**：LSP 工具面改为条件注入 | **本波不做**：会让 MCP overlay 读取业务 LSP 配置域并承担顺序耦合；本波以 handler 构造快照实现。**复核点**：未来热更新先追加配置生命周期与 ToolContext 裁决 |

## 1. 目标与非目标

### 1.1 目标（本波次可证伪面）

1. `CronMiddleware` 的**能力面**（`cron_register` / `cron_list` / `cron_remove`）经 `cron` builtin MCP 实例提供，模型面名字为 `mcp__cron__cron_register` / `mcp__cron__cron_list` / `mcp__cron__cron_remove`，直连性为 **deferred**（与迁移前逐位一致）。
2. cron 的任务注册表、表达式解析、`next_fire` 计算与 **1s tick 驱动**成为 `cron` 实例内部状态/行为；宿主只保留「事件接收端口」（`CronSchedulerPort::subscribe`）与「UI 投影端口」（`list_tasks` / `toggle`）。
3. `LspMiddleware` 的**能力面**（单工具 `LSP`，10 个 operation）经 `lsp` builtin MCP 实例提供，模型面名字 `mcp__lsp__LSP`，直连性为 **deferred**。
4. `LspServerPool`（含 language server 子进程、诊断状态、open-file 缓存）成为 `lsp` 实例内部状态；文件变更同步由宿主侧薄中间件经端口显式发送。
5. 两个链槽位按 A7/A8 处置：`ChainSlot::Cron` 删除、`ChainSlot::Lsp` 保留位置但装载 `LspSyncMiddleware`；两表（`MIDDLEWARE_NAMES` / `BUILTIN_INSTANCE_POLICY_KEYS`）完成迁键。
6. 归一（effective name ↔ 原始名）在**判定面等价**：`cron_list` / `cron_remove` / `LSP` 不进入审批、不计 mutation；`cron_register` 仍审批 + mutation。

### 1.2 非目标（显式排除，避免范围蔓延）

- `workspace` 实例与其全部能力（`Read`/`Write`/`Edit`/`Glob`/`Grep`/`folder_operations`/`Bash`/Skill 工具/Todo/GitWatch）——wave 3。
- `side-projects/local-mcp-server` 的任何改动。
- MCP 协议通知（`notifications/*`）与订阅能力的引入（A16；本波不做，记录代价与未来复核点，非协议绝对不可行结论）。
- `peri-lsp` crate 的行为重构（只作为库复用；`didClose` 缺失、`is_content_modified` 判别丢失等既有缺口**不修**）；A23 仅扩展既有 `LspPoolPort` 适配，不改协议业务实现。
- 多 cwd LSP 的 per-session 恢复与配置热更新（A21/A22，登记为 wave 3 依赖）。
- TUI 双 scheduler 缺陷、print/stdio 无 tick 缺陷、LSP 同步覆盖缺口（A15，只登记）。
- `session/delete` 不再关闭 LSP pool（A11/A22）；host shutdown 才负责有界关闭全部 language server。

## 2. 事实行（代码事实，规划依据）

> 全文代码事实、§0.1 表与代码引用统一采信 HEAD `a81e0ba6`；主索引是符号名 + repository-relative 完整路径，行号仅作辅助。W0 的记录提交 `b1651aee` / 采集基线 `226fa2bc` 仍是历史证据，不混作当前代码版本。拟新增符号/测试均为施工目标，不能当成已有实现。

### 2.1 cron（当前实现）

| 事实 | 位置 |
| --- | --- |
| `CronScheduler`：内存 `HashMap<String, CronTask>` + 主 `trigger_tx` + `extra_trigger_txs: Vec<UnboundedSender>` | `CronScheduler`（`peri-middlewares/src/cron/mod.rs`） |
| 主 `trigger_tx` 的接收端在生产被丢弃（tick 走 `extra_trigger_txs` 广播路径；主通道失败仅 `warn`） | `CronScheduler::tick`（`peri-middlewares/src/cron/mod.rs`）；`assemble_server_config_with_mcp_profile`（`peri-acp/src/host/assemble.rs`）；`ProductionChainAssembler::assemble`（`peri-middlewares/src/assembly.rs`） |
| MCP 共享 scheduler 的 1s tick 由 ACP Startup / CronTick task 驱动；四个 `drive_cron_tick` 调用者保留差异 | `assemble_server_config_with_mcp_profile`（`peri-acp/src/host/assemble.rs`）、`HostTaskKind::CronTick`（`peri-acp/src/host/task_scope.rs`）；`drive_cron_tick` 字段赋值：`peri-tui/src/launch.rs` 为 true，`peri-tui/src/cli_print.rs`、`peri-acp/src/host/stdio/mod.rs`、`peri-acp/src/host/workspace.rs` 为 false |
| 工具面：`cron_register` / `cron_list` / `cron_remove`，均未覆写 `is_direct()` ⇒ deferred；无 `prompt_declaration` | Cron 三个 `BaseTool` 实现（`peri-middlewares/src/cron/tools.rs`）；默认值 `BaseTool::is_direct`（`peri-acp-types/src/tools.rs`） |
| `CronMiddleware` 只实现 `collect_tools` + `name()`，无 hook / prompt / state | `CronMiddleware`（`peri-middlewares/src/cron/middleware.rs`） |
| 触发事件消费链：subscribe → bridge → continuation → 审批 → enqueue → prompt | `SessionCronBridge::start`（`peri-acp/src/session/cron_bridge.rs`）、`run_cron_continuation_scheduler`（`peri-acp/src/host/continuation.rs`）、`approve_scheduled_trigger` / `dispatch_prompt_turn`（`peri-acp/src/host/prompt.rs`） |
| 审批在入队之前，合成工具名 `cron_trigger` | `SessionCronBridge`（`peri-acp/src/session/cron_bridge.rs`）；`approve_scheduled_trigger`（`peri-acp/src/host/prompt.rs`） |
| 内置 skill 用裸名教模型调用三工具，声明 `cron_register` 在审批模式必弹窗 | Cron 工具调用说明（`peri-middlewares/src/skills/builtin/skills/cron/SKILL.md`） |

### 2.2 lsp（当前实现）

| 事实 | 位置 |
| --- | --- |
| 单工具 `LSP`，10 个 operation；`peri-middlewares/src/lsp/tool.rs:LspTool::timeout` 返回 `None`；`LspTool::invoke` 的 `_ctx` 未使用 | `peri-middlewares/src/lsp/tool.rs:LspTool::timeout` / `LspTool::invoke`（采信 HEAD `a81e0ba6`） |
| pool 为 **per-session**：三处实际构造调用分别位于 `prepare_existing`、`handle_new`、`handle_fork`（其中 `handle_fork` 为此前遗漏），helper 为 `create_session_lsp_pool`，删除逻辑在 `handle_delete`；构造参数 `(cwd, configs)`，`root_uri = path_to_uri(cwd)` | `peri-acp/src/host/requests/session_lifecycle.rs:prepare_existing`、`handle_new`、`handle_fork`、`create_session_lsp_pool`、`handle_delete`；`peri-lsp/src/pool.rs:LspServerPool::new` / `root_uri`（采信 HEAD `a81e0ba6`） |
| pool 的跨 turn 复用经 `LspPoolPort` downcast（H1） | `peri-middlewares/src/assembly/lsp.rs:add_lsp`；`peri-acp-types/src/ports.rs:LspPoolPort` |
| MCP bridge 实际使用调用上下文，但 builtin 执行 helper 丢失宿主上下文；外层有 120s 超时而 `LspTool::timeout()` 为 `None` | `peri-middlewares/src/mcp/tool_bridge.rs:McpToolBridge::invoke`（`TOOL_CALL_TIMEOUT`、`effective_tool_dispatcher`、`session_id`、`turn_generation`、`invocation_id`、`cancellation`）；`peri-middlewares/src/mcp/builtin/web.rs:invoke_tool_call`（`ToolContext::new(&[], cwd)`）；`peri-middlewares/src/lsp/tool.rs:LspTool::timeout` |
| 双门控：配置非空才注册 middleware；`has_servers()` 才产出工具 | `add_lsp`（`peri-middlewares/src/assembly/lsp.rs`）、`LspMiddleware::collect_tools`（`peri-middlewares/src/lsp/middleware.rs`）、`LspServerPool::has_servers`（`peri-lsp/src/pool.rs`） |
| 文件同步唯一触发点：`after_tool` 先按 path 找 ready server 再读内容；change 失败仍 save；`_result` 未用、错误仅 debug、恒 `Ok(())`，hook 无 `ToolContext` | `LspMiddleware::after_tool`（`peri-middlewares/src/lsp/middleware.rs`）；`TOOL_WRITE` / `TOOL_EDIT`（`peri-middlewares/src/tool_search/core_tools.rs`）；`AfterToolState` / `StateView::cwd`（`peri-agent/src/middleware/capabilities.rs`） |
| `didOpen` 为惰性：查询前读文件并 `did_open`（幂等），读失败不阻塞 | `LspTool::ensure_file_open`（`peri-middlewares/src/lsp/tool.rs`） |
| 诊断通道：模型显式 `LSP(operation="diagnostics")` 读 registry，无 hook / prompt / 事件注入；`on_update` 回调无生产消费者 | `LspTool::invoke`（`peri-middlewares/src/lsp/tool.rs`）、`DiagnosticsRegistry`（`peri-lsp/src/diagnostics.rs`） |
| language server 为独立子进程（`kill_on_drop(true)`，`ProcessTree` 归属），cwd/root 来自 pool `root_uri` | `LspTransport::spawn`（`peri-lsp/src/jsonrpc/transport.rs`）、`LspClient::do_start`（`peri-lsp/src/client/lifecycle.rs`） |
| 宿主退出按 session 收集 pool 并 shutdown，以 `Arc::ptr_eq` 去重 | `shutdown_host`（`peri-acp/src/host/shutdown.rs`） |
| 配置合并：global settings.json `lspServers` < 插件 manifest，插件注入 `CLAUDE_PLUGIN_ROOT` + 展开 `${VAR}` | `load_merged_lsp_servers`（`peri-middlewares/src/assembly/lsp.rs`）；`load_global_lsp_config` / `lsp_config_from_plugin` / `expand_env_vars`（`peri-lsp/src/config.rs`） |
| 宿主装配中 LSP 配置合并在 MCP 池构造、initialize task 启动之后 | `assemble_server_config_with_mcp_profile`（`peri-acp/src/host/assemble.rs`）中的 `McpClientPool::run_initialize` 与 `load_merged_lsp_servers` |
| LSP servers/pool 进入装配上下文供链使用 | `dispatch_prompt_turn`（`peri-acp/src/host/prompt.rs`）、`AssemblyContext::lsp_servers` / `lsp_pool`（`peri-agent/src/session/factory.rs`） |

### 2.3 共同事实（wave 1 遗产，本波复用）

| 事实 | 位置 |
| --- | --- |
| 注册表形状与查表函数 | `BuiltinMcpInstance` / `find` / `original_tool_name_of_effective`（`peri-acp-types/src/builtin_mcp.rs`） |
| overlay 六规则 + 注入策略 + 关闭集判定（`closed_instances` / `is_closed`） | `peri-middlewares/src/mcp/builtin/mod.rs`（`apply_builtin_overlay` / `BuiltinInjectionPolicy` / `closed_instances`） |
| 默认层注入点（loader step 6.5，唯一读 env 入口） | `peri-middlewares/src/mcp/config.rs`（`load_merged_config_full` → `..._with_paths` → step 6.5） |
| 同进程链路（duplex + serve_server + server task 表 + 有界收敛） | `spawn_builtin_link` / `BuiltinServerTask::converge`（`peri-middlewares/src/mcp/builtin/runtime.rs`）；`McpClientPool::builtin_server_tasks`（`peri-middlewares/src/mcp/client.rs`）；`close_builtin_task` / `close_builtin_tasks`（`peri-middlewares/src/mcp/client/lifecycle.rs`） |
| 三分类 transport（`Stdio` / `Http` / `Builtin`）与分类函数 | `peri-middlewares/src/mcp/transport.rs:TransportKind` / `TransportConfig::kind`；`peri-middlewares/src/mcp/initialize.rs`、`peri-middlewares/src/mcp/reconnect.rs` 消费分类；`peri-middlewares/src/mcp/client/transport.rs:serve_client_auto` 只负责签名与 client handler 适配 |
| `call_tool` 结果映射（成功 `Complete(success)` / 失败 `Complete(error(固定文本))` / 未知名 `invalid_params`） | `invoke_tool_call`（`peri-middlewares/src/mcp/builtin/web.rs`，IF-D14） |
| 四个关闭过滤面 | `SystemReadySnapshot::required_tools` / `McpMiddleware::open_typed_bridges`（`peri-middlewares/src/mcp/middleware.rs`）、`build_parent_tools`（`peri-middlewares/src/assembly/preparation.rs`）、`builtin_tools` / `build_tools`（`peri-middlewares/src/assembly/workflow.rs`） |
| 归一 helper 与消费点 | `original_tool_name_of_effective`（`peri-acp-types/src/builtin_mcp.rs`）；`default_requires_approval` / `is_edit_tool`（`peri-middlewares/src/permission/mod.rs`）；`is_mutation_tool`（`peri-middlewares/src/subagent/mod.rs`）；`format_tool_args`（`peri-tui/src/kit/tool_display.rs`）；`summarize_input` / `summarize_output`（`peri-tui/src/truncate.rs`）；`TOOL_PARAM_ALIASES`（`peri-agent/src/tools/invocation.rs`）；matcher（`peri-middlewares/src/hooks/matcher.rs`）；`ToolFilterPolicy::canonical`（`peri-agent/src/session/tool_catalog.rs`）；`infer_tool_kind`（`peri-acp/src/event/tool_projection.rs`） |
| 声明段收集（模板来源 = 声明表） | `collect_declarations` / `builtin_declaration`（`peri-middlewares/src/tool_search/declaration.rs`） |
| 链槽位与蓝图 | `ChainSlot` / `production_blueprint`（`peri-agent/src/session/factory.rs`） |

## 3. 冻结接口（IF-P3-01…IF-P3-12）

> 冻结语义：实施期不得单方面改形状；需要变更时在本文件追加裁决记录（A 号）并同步 sub-plan。owner 列指向 §6 任务号。

### IF-P3-01 声明表新增两实例（owner：C-01）

```rust
// peri-acp-types/src/builtin_mcp.rs（新增，形状复制 WEB_TOOLS/ARTIFACT_TOOLS）
const CRON_TOOLS: &[BuiltinMcpTool] = &[ /* cron_register, cron_list, cron_remove */ ];
const LSP_TOOLS: &[BuiltinMcpTool] = &[ /* LSP */ ];
// BUILTIN_MCP_INSTANCES 追加两条：
//   BuiltinMcpInstance { name: "cron", instance: "cron", policy_key: "CronMiddleware", tools: CRON_TOOLS }
//   BuiltinMcpInstance { name: "lsp",  instance: "lsp",  policy_key: "LspMiddleware",  tools: LSP_TOOLS  }
```

- 逐工具 `effective_name` 冻结字面量：`mcp__cron__cron_register` / `mcp__cron__cron_list` / `mcp__cron__cron_remove` / `mcp__lsp__LSP`（由 `mcp::builtin::tests` 的字面量测试与 `effective_tool_name()` 对齐）。
- 逐工具 `direct: false`、`prompt_declaration: None`（A4）。
- `BUILTIN_RESERVED_INSTANCE_NAMES` **不改**（两个名字已在表内）。

### IF-P3-02 工具面直连性与 deferred 归属（owner：C-01 / V-02）

- 三面冻结：①首个 LLM 请求直连参数；②摘要面；③搜索/执行可达性。四个工具在 typed bridge 后 `is_direct() == false`，主链 deferred；subagent `parent_tools` / workflow agent 不含四工具。
- 摘要面不是等价对照面：`format_deferred_list` 对 `mcp__` 前缀整体过滤，迁移后四个 effective name 整体缺席，只作裸名消失的时点证据。
- 搜索面（`SearchExtraTools` / `ToolIndex`）是唯一等价对照面：每个工具迁移前后满足裸名 XOR effective name，按重命名映射判断，不作字面集合相等；`ExecuteExtraTool` 使用 effective name。
- `system_mcp_tools` 空数组 ⇒ 不提升 direct、不做必需工具校验。

### IF-P3-03 默认层注入与就绪闸门（owner：H-02 / V-02）

1. **注入前提**：builtin enabled 且实例未被配置关闭时，两实例经 loader step 6.5 自动注入；缺失 ⇒ 完整条目（`protocol_version = None`、`system_mcp = Some(true)`、`system_mcp_tools = Some([])`、`source = ConfigSource::Builtin`）。
2. 用户写 `{}` ⇒ 与 `web` 同构；非法片段/保留名接管返回 typed error；`disabled` 只表示 MCP 配置层跳过连接；MetaHarness `policy_key = false` 只影响本 turn 投影，readiness 仍走 pool 配置；`PERI_MCP_BUILTIN=off` 表示 builtin 零注入；物理 `close` 才停止任务并释放资源。仅策略关闭保留 handler/pool/readiness，不得把四类关闭混称为“关闭”。
3. `system_mcp = true` ⇒ 1R 阶段必须 ready；工具列表为空不等于未 ready。
4. **LSP 门控**：`lsp` 在上述前提下总是注入；工具集只在 handler 构造时按生效配置非空快照决定，不由 MCP overlay 表达。
5. `LspServerPool::has_servers()` = 生效配置非空，与 server 进程 ready 无关；handler 构造必须晚于配置合并；不支持配置热更新。

### IF-P3-04 实例上下文载体与 pool 承载（owner：H-02）

```rust
// peri-middlewares::assembly 公开的上下文载体/工厂；builtin 模块本身不对 peri-acp 开放
pub struct BuiltinInstanceContext {
    pub cwd: String,                                  // artifact 复用；lsp 作为 pool root_uri 来源
    pub cron: Option<CronInstanceInput>,              // 见 IF-P3-05
    pub lsp: Option<LspInstanceInput>,                // 见 IF-P3-08；context 内为具体类型 `Arc<LspServerPool>`；宿主侧投影为 `Arc<dyn LspPoolPort>`（K8 冻结）
    pub closed: std::collections::BTreeSet<String>,  // closed_instances(...) 派生；A24 关闭集，A33 随上下文注入
}
pub struct CronInstanceInput { pub scheduler: Arc<Mutex<CronScheduler>>, pub tick_enabled: bool }
pub struct LspInstanceInput { pub pool: Arc<LspServerPool> }
```

- **上下文缺口边界（F1）**：`McpToolBridge::invoke`（`peri-middlewares/src/mcp/tool_bridge.rs`）已经使用 dispatcher/session/turn/invocation/cancellation；未贯通的是宿主 cwd/session → builtin 工具执行，`invoke_tool_call`（`peri-middlewares/src/mcp/builtin/web.rs`）另造 `ToolContext::new(&[], cwd)`。本波采用 host cwd + 共享 pool；不新增 session 路由协议，wave 3 才恢复 per-session，上述缺口不得改写成“bridge 不使用 ctx”。
- `builtin_server_handler(instance: &str, ctx: &BuiltinInstanceContext) -> Option<BuiltinServerHandler>`。
- **构造入口改为 pool 方法**：`McpClientPool::spawn_builtin_transport(&self, instance: &str) -> Result<BuiltinTransport, BuiltinSpawnError>`（读取 pool 的 `execution_cwd` 与上下文）；`peri-middlewares/src/mcp/initialize.rs` / `peri-middlewares/src/mcp/reconnect.rs` 只调用该方法，不扩两处签名。
- **注入点与注入时序（A33）**：`BuiltinInstanceContext` 由**宿主装配** `peri-acp/src/host/assemble.rs`（H-04）构造并注入，必须早于 `McpClientPool::run_initialize(...)` 的调用（唯一生产调用点 `peri-acp/src/host/assemble.rs`）及其后台 spawn；`peri-middlewares/src/assembly/mcp.rs` **不参与**实例上下文注入（只做中间件装配与关闭集投影）。`set_builtin_instance_context(...)` 首次注入生效，重复注入（含同一 `Arc` 再注入）返回 typed `Err`，`initialize` 已开始后的**晚注入同样拒绝**（同一短锁保护 setter 与初始化标志，锁内禁 await，不能只依赖 `OnceLock::set`），runtime 记录并拒绝，首个上下文继续生效。可见性只经 `peri_middlewares::assembly` 的公开类型/工厂，`peri-acp` 不得 `import peri_middlewares::mcp::builtin`。注入失败面沿既有 `insert_failed` + `commit_discovery_failure` 收口、`system_mcp` 闸门 fatal、不伪造 ready。
- 未注册实例维持当前 typed `UnknownInstance`；已注册但无 handler/context 的实际错误变体以 H-02/H-05 对当前 runtime 复测结果为准，不静默套用旧 `HandlerNotWired` 断言。

**三态生命周期冻结（A26）**：

| 状态 | 保留对象 | 显式销毁/关闭对象 |
| --- | --- | --- |
| host shutdown | 无 host 级 handler/task/pool；验收保留关闭证据 | 两实例 transport、handler task、Cron tick、LSP pool 及全部 language server；有界等待并断言无 orphan；`pool.shutdown()` 必须被 await（有界），不得改为 `begin_shutdown()` 后立即返回 |
| instance close（物理操作） | 组合根持有的 scheduler/pool 与实例注册状态 | 对应 transport/handler task/tick 显式 shutdown + join；readiness 不能继续宣称 ready；不关闭共享 LSP pool |
| MetaHarness 策略关闭（非物理操作） | 实例/handler/pool/tick；readiness 仍走 pool 配置 | 仅本 turn 工具投影与对应同步目标关闭；`LspSyncMiddleware:false` 只关同步，不触发 task close |
| reconnect | 同一组合根 scheduler/pool 与上下文 | 旧 transport/handler/tick 先显式关闭并 join（tick join 由代监督者 `BuiltinInstanceSupervisor` 承担，A32），再建立新一代；任一时刻同一 scheduler 只有一个 tick 驱动 |

### IF-P3-05 cron 实例与 tick 内化（owner：C-02 / C-03）

```rust
// peri-middlewares/src/mcp/builtin/cron.rs（C-02）
pub(crate) struct CronMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,   // 复用 cron/tools.rs 三个 BaseTool，不重写 schema；**不持 tick**
}

// peri-middlewares/src/mcp/builtin/runtime.rs（H-02）
pub(crate) struct TickGuard { cancel: CancellationToken, join: JoinHandle<()> }
pub(crate) struct BuiltinInstanceSupervisor { tick: Option<TickGuard>, server_task: BuiltinServerTask }
pub(crate) struct BuiltinCloseOutcome { pub(crate) tick: TickCloseOutcome, pub(crate) server: BuiltinServerExit }
pub(crate) struct BuiltinTransport { /* io + tick: Option<TickGuard> */ }
impl BuiltinTransport { pub(crate) fn into_parts(self) -> (TransportIo, BuiltinInstanceSupervisor) }
```

- **tick 归属（A32）**：tick 在 `McpClientPool::spawn_builtin_transport` 内 spawn **一次**（1s interval，语义逐位搬运既有宿主 tick 的 `interval.tick()` → `scheduler.lock().tick()`）；handler 不持有 tick、`CronMcpServer::new` 不 spawn；`BuiltinTransport.tick: Option<TickGuard>` 经 `into_parts()` 拆出 `(io, BuiltinInstanceSupervisor)`，池表 `builtin_server_tasks` 的值类型由 `BuiltinServerTask` 换为 `BuiltinInstanceSupervisor`（`peri-middlewares/src/mcp/client.rs`）。`McpClientPool::spawn_builtin_transport` 是**唯一 spawn 点**。
- **关闭契约（A32）**：`TickGuard::shutdown(timeout)` 顺序固定为 `cancel()` → 有界 join，仅超时才 `abort()` + await；`BuiltinInstanceSupervisor::close(timeout).await` 顺序固定：①`tick.take()` → `TickGuard::shutdown(timeout)` ②server task `converge(timeout)` ③返回 `BuiltinCloseOutcome { tick, server }`；超时复用既有 `BUILTIN_CONVERGE_TIMEOUT`（`peri-middlewares/src/mcp/builtin/runtime.rs`，1000ms，不新造第二套策略）。`TickGuard::Drop` 只 `cancel()`（不 abort、不阻塞、不承担正常收敛）。host shutdown / instance close / reconnect 三态分别断言 supervisor/tick/server task/pool 的保留/销毁对象。
- **唯一关闭路径（H-02，F11）**：`McpClientPool::close_builtin_task` 与 `McpClientPool::close_builtin_tasks`（`peri-middlewares/src/mcp/client/lifecycle.rs`）在 async 上下文内 await `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)` 收敛，**不新增第二条关闭路径**；`remove_server` / `set_disabled` / pool shutdown / initialize 失败 / reconnect 均复用上述入口；批量关闭不能绕过 `BuiltinInstanceSupervisor` 直接 `converge`。未收敛不得建新代；不能以 `Drop` 冒充正常关闭证据。机械清点（主 agent 已复核）：`register_builtin_task` 调用点 7 处（生产 `peri-middlewares/src/mcp/initialize.rs`、`peri-middlewares/src/mcp/reconnect.rs`；测试 `peri-middlewares/src/mcp/builtin/runtime_test.rs` ×5），`close_builtin_task(s)` 生产调用 6 处（`peri-middlewares/src/mcp/initialize.rs`、`peri-middlewares/src/mcp/reconnect.rs` ×2、`peri-middlewares/src/mcp/client/lifecycle.rs` ×3）+ 测试 4 处（`peri-middlewares/src/mcp/builtin/runtime_test.rs` ×3、`peri-middlewares/src/mcp/builtin_runtime_test.rs` ×1），返回类型消费点 `peri-middlewares/src/mcp/builtin/runtime_test.rs`、`peri-middlewares/src/mcp/builtin_runtime_test.rs`（`BuiltinServerExit` → `BuiltinCloseOutcome`）。
- **reconnect（A32）**：顺序不变（`peri-middlewares/src/mcp/reconnect.rs` 先关旧代、后 spawn 新代）；任一时刻同一 scheduler 至多一个 tick 驱动。
- **可观察断言四条（A32）**：①关闭后 >2×interval 无新触发且 `tick_is_finished()` 为真；②重连后每 interval 恰 1 次 tick；③`tick_enabled=false` 无驱动但三工具可用；④guard join 状态可观测（`#[cfg(test)] builtin_tick_is_finished(&self, server_name) -> Option<bool>`，`peri-middlewares/src/mcp/client/lifecycle.rs`）。
- 工具面 = 三个工具（静态集合），不做条件门控（cron 迁移前总是注册）。
- `call_tool` 走 IF-D14 唯一实现；业务错误原因的退化见 §10 第 5 项。

### IF-P3-06 cron 事件端口（owner：H-03）

- 宿主继续持有 `Arc<dyn CronSchedulerPort>`（`CronSchedulerPortHandle`），指向**同一** `CronScheduler` 实例（A1）。
- `subscribe` / `list_tasks` / `toggle` 语义逐位不变；`SessionCronBridge`、`run_cron_continuation_scheduler`、`approve_scheduled_trigger` **零改动**。
- **注册路径唯一**：任务注册只经 `mcp__cron__cron_register`（MCP 工具面）；宿主端口不再暴露 register（现状亦无）。

### IF-P3-07 分派文件重构（owner：H-01）

- 新文件 `peri-middlewares/src/mcp/builtin/dispatch.rs`：`BuiltinServerHandler` 枚举（每实例一个变体）+ `builtin_server_handler` 工厂 + `impl ServerHandler` 三分派。
- `peri-middlewares/src/mcp/builtin/web.rs` 只保留：共享助手（`server_info` / `rmcp_tool_from_base` / `list_tools_of` / `invoke_tool_call`）+ `WebMcpServer`；`peri-middlewares/src/mcp/builtin/artifact.rs` 同理。
- 行为逐位不变：相同输入产生相同 `ServerInfo` / 工具列表 / `call_tool` 映射（由既有 `peri-middlewares/src/mcp/builtin/web_test.rs` / `peri-middlewares/src/mcp/builtin/artifact_test.rs` 用例守护）。

### IF-P3-08 lsp 实例与工具面门控（owner：L-01）

```rust
// peri-middlewares/src/mcp/builtin/lsp.rs
pub(crate) struct LspMcpServer { tools: Vec<Arc<dyn BaseTool>>, pool: Arc<LspServerPool> }
```

- **门控谓词冻结**：生效 LSP 配置非空，即 `LspServerPool::has_servers()`；`LspServerPool::new` 惰性构造且不拉起 language server，故与进程 ready 无关。
- `LspMcpServer` 构造必须晚于配置合并（H-04 顺序），按该时点谓词快照工具集：真为 `vec![LspTool::new(pool.clone())]`，假为 `vec![]`；不支持配置热更新（显式非目标）。
- `list_tools` 返回该快照；`call_tool` 走 IF-D14；不覆写 `discover`；`server_info` 只声明 `tools`。

### IF-P3-09 lsp 同步端口与薄中间件（owner：L-03）

- **端口合流冻结（A23 / F7，以下是拟新增契约，不是当前实现）**：保留 `LspPoolPort::as_any` / `shutdown`，不新增第二端口。继续使用 `#[async_trait::async_trait]`，在 `peri-acp-types/src/ports.rs:LspPoolPort` 增加以下完整签名；`Path` 指 `std::path::Path`，均接收文件系统绝对 path 而非 URI，由 `peri-lsp/src/pool.rs` 实现转换 URI：

```rust
fn ready_for(&self, path: &std::path::Path) -> bool;
async fn did_change(&self, path: &std::path::Path, text: &str) -> Result<(), LspSyncError>;
async fn did_save(&self, path: &std::path::Path) -> Result<(), LspSyncError>;
```

- **typed error**：`LspSyncError` 定义在 `peri-acp-types/src/ports.rs`（契约 crate），变体冻结为 `NoServer` / `Protocol`（与 A30 一致）；实现方映射 `peri-lsp` 错误，不把底层错误文本/文件内容跨端口输出。新增方法无默认 no-op；所有实现者必须显式实现。
- **cwd/内容来源**：`LspSyncMiddleware::after_tool`（拟改 `peri-middlewares/src/lsp/middleware.rs`）只有 `AfterToolState` / `ToolCall` / `ToolResult`，没有 `ToolContext`。cwd 直接来自 `AfterToolState` 继承的 `StateView::cwd()`（`peri-agent/src/middleware/capabilities.rs`）；从 `ToolCall.input["file_path"]` 取 path，绝对路径原样、相对路径以 `state.cwd()` 解析。匹配仍仅 `{Write, Edit}`，不扩大同步覆盖，也不以 `ToolResult` 内容代替磁盘内容。
- **ready-before-read**：先 `ready_for`（内部沿用 `LspServerPool::server_for_file` + `LspClient::is_ready`，不启动 server），假则不读文件、不发通知；真才 `tokio::fs::read_to_string`。读失败 debug 降级并返回 `Ok(())`；发送前 ready 消失返回 `NoServer`，不得补拉进程。
- **失败与并发顺序**：单次 hook 内等待 `did_change` 完成后**始终**尝试 `did_save`，包含前者返回 Err；两者错误分别 debug 降级，恒 `Ok(())`，不改变工具结果。同一文件跨调用维持现状：**不承诺全局 FIFO、原子 change/save 对或物理写入顺序**，允许不同调用交错；本波不新增同步队列/版本协议。验收须覆盖并发调用各自的 change→save 顺序与失败后 save，不得把单调用顺序升级成跨调用串行保证。

| 实现者文件（完整路径） | 承担任务 | 必需适配与防假绿断言 |
| --- | --- | --- |
| `peri-lsp/src/pool.rs` | L-02 | `impl LspPoolPort for LspServerPool`：ready 查询、path→URI、typed error 映射、协议委派 |
| `peri-acp/src/host/requests_test.rs` | H-04 | LSP mock 显式实现新增方法；修改 session/delete 不关闭共享 host pool 的计数断言 |
| `peri-acp/src/host/stdio/run_server_integration_test.rs` | H-04 | 端口替身显式实现；session 持 pool 语义改为共享 host pool |
| `peri-lsp/src/pool_test.rs` | L-02 | 端口替身显式实现；ready/change/save 调用计数和参数断言，不能仅通过 downcast 测试 |

- 若未来追加默认实现，必须先追加裁决且显式断言同步替身确实被调用；默认 no-op 的 exit 0 不计同步通过。
- 关闭交叉矩阵：只关 `LspSyncMiddleware` ⇒ LSP 工具仍可调用，且同步不读文件/不发送；只关 `LspMiddleware` ⇒ 工具不可见，同步目标关闭且同步不读文件/不发送；两者都开 ⇒ 正常同步；物理 pool close ⇒ 停止 server 任务。
- `LspPoolPort` 继续由 `peri-lsp::LspServerPool` 实现；本波不改 `peri-lsp` 的协议业务行为。

### IF-P3-10 两表迁移与静态清单（owner：S-02）

- `MIDDLEWARE_NAMES`（`peri-acp-types/src/meta_harness.rs`）：删 `"CronMiddleware"` / `"LspMiddleware"`，增 `"LspSyncMiddleware"`。
- `BUILTIN_INSTANCE_POLICY_KEYS`（`peri-acp-types/src/meta_harness.rs`）：增 `"CronMiddleware"` / `"LspMiddleware"`（顺序与 `BUILTIN_MCP_INSTANCES` 一致）。
- `MIDDLEWARE_TOOL_NAMES`（`peri-acp-types/src/meta_harness.rs`）：删 `"LSP"`，cron 三工具**不入表**。
- 同文件 `tests` 的三条不变式与 `peri-middlewares/src/assembly_test.rs` 的槽位序列 / 槽位名映射 / 链名序列 / 已知键全集 / 工具名集合断言同步更新为**强度不降**形态。

### IF-P3-11 归一与判定等价（owner：S-01 / S-03）

- 判定型（替换）：`peri-middlewares/src/permission/mod.rs:default_requires_approval`、`is_edit_tool`、`peri-middlewares/src/subagent/mod.rs:is_mutation_tool` 对四个 effective name 归一后判定，结果必须与裸名逐项相等；`peri-middlewares/src/permission/mod.rs:sensitive_tool_entries` 保留敏感规则/14 项数量/顺序，但渲染模型可见 Markdown 时从声明表解析 effective name。
- 匹配型（原样优先）：`peri-tui/src/kit/tool_display.rs`、`peri-tui/src/truncate.rs` 已经在原样匹配失败后经 `original_tool_name_of_effective` 重试；`peri-tui/src/truncate.rs` 的 `"LSP"` 是归一后共享分支。本波 S-03 只新增 Cron/LSP 注册表条目后的回归断言，不新增 TUI 归一入口。
- 事件载荷 / transcript / ACP 事件 / MCP wire 仍为 effective name（**不**归一）。
- `TOOL_PARAM_ALIASES`（`peri-agent/src/tools/invocation.rs`）：核对无 `LSP` 裸名条目（已核：无命中）⇒ 无需改；若实施期发现新增条目，按同一归一规则处理。

### IF-P3-12 能力声明与隔离（owner：L-01 / V-04）

- `server_info` 只声明 `tools`；不声明 `resources` / 不启用订阅 / 不覆写 `discover`。
- 隔离契约（待验证）：真实 Cron/LSP builtin transport 独立、handler task 独立（关闭一个不影响另一个）、Cron tick 与 LSP pool 状态不互相销毁；同一实例的组合根/handler 仍按 A1 共享状态。`isolation_fixture`（`peri-middlewares/tests/mcp_isolation_contract.rs`）当前以 `PERI_MCP_BUILTIN=off` 启动两台 stdio fixture，只证明 stdio 局部回归。V-04 必须保留这组回归并新增 builtin enabled 的真实 handler/tick/state 路径；off 零注入另行断言，禁止升级为 builtin 隔离通过。
- capability root / 凭据隔离沿用 wave 1 口径 **UNVERIFIED**（无 per-instance 可观察面，见 §10 第 6 项）。

## 4. 文件所有权矩阵（本波次全部改动面，逐文件唯一 owner）

> 「新增/修改」列不区分，两者都必须配对测试；「测试挂载」列为空即视为**假绿**（§9 规则 2）。
> 本表之外的文件**不得**在本波次改动；发现必须改动时先在本文件追加裁决记录。测试挂载列列出的文件也属于唯一 owner 清单：默认随本行 owner，若另有独立行则仅以独立行 owner 为准；C-03 的 tick 测试由 C-02 持有的测试文件承载，V-02 持有 runtime 测试文件，不产生双 owner。全部文件引用写 repository-relative 完整路径，不以表头、`等` 或模糊 glob 推断。以下是未来实施清单，本次 v3 修订不改其中任何生产/测试/acceptance 文件。

### 4.1 契约层（`peri-acp-types`）

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-acp-types/src/builtin_mcp.rs` | C-01 | 增 `CRON_TOOLS` / `LSP_TOOLS` + 两实例条目；两实例声明、工具常量和测试挂载由 C-01 单一持有 | `peri-acp-types/src/builtin_mcp_test.rs`（现有挂载；C-01 修改具体函数） |
| `peri-acp-types/src/meta_harness.rs` | S-02 | 两表迁键（IF-P3-10） | 本文件 `mod tests`（既有三条不变式） |
| `peri-acp-types/src/cron.rs` | H-03 | **不改**（`CronSchedulerPort` 契约零改动） | — |
| `peri-acp-types/src/ports.rs` | L-02 | 将同步能力合流到 `LspPoolPort`（A23/A30）：`ready_for` / `did_change` / `did_save` **无默认实现**；`LspSyncError { NoServer, Protocol }` 同 crate | `peri-lsp/src/pool_test.rs` 具名端口用例 |

### 4.2 builtin 行为层（`peri-middlewares/src/mcp/builtin/`）

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-middlewares/src/mcp/builtin/mod.rs` | H-02 | 挂载 `dispatch` / `cron` / `lsp` / `runtime`；由 assembly 公开上下文类型/工厂，不直接放开 builtin 模块；模块挂载与 `peri-middlewares/src/cron/middleware.rs` 删除、`ChainSlot::Cron` 摘除须在**同一原子提交**内完成（A31②），禁止 `0 tests` 或 dead-code 豁免绕过 | `peri-middlewares/src/mcp/builtin/builtin_test.rs`（既有挂载） |
| `peri-middlewares/src/mcp/builtin/dispatch.rs`（新） | H-01 | `BuiltinServerHandler` + 工厂 + `impl ServerHandler` 三分派；H-05 只接线新增变体，文件写入权仍归 H-01 | `peri-middlewares/src/mcp/builtin/dispatch_test.rs`（新，`#[path]` 挂载） |
| `peri-middlewares/src/mcp/builtin/web.rs` | H-01 | 只保留共享助手 + `WebMcpServer`；删除枚举/工厂 | `peri-middlewares/src/mcp/builtin/web_test.rs`（既有；搬迁后的工厂用例移入 `peri-middlewares/src/mcp/builtin/dispatch_test.rs`） |
| `peri-middlewares/src/mcp/builtin/artifact.rs` | H-01 | 不改行为（构造签名随上下文载体调整） | `peri-middlewares/src/mcp/builtin/artifact_test.rs`（既有） |
| `peri-middlewares/src/mcp/builtin/runtime.rs` | H-02 | 新增 `TickGuard` / `BuiltinInstanceSupervisor` / `BuiltinCloseOutcome` / `BuiltinTransport.tick` + `into_parts()`（A32）；`spawn_builtin_transport` 迁为 pool 方法承载；`spawn_builtin_link` 泛型 seam **不变** | `peri-middlewares/src/mcp/builtin/runtime_test.rs`（既有） |
| `peri-middlewares/src/mcp/builtin/cron.rs`（新） | C-02 | `CronMcpServer`（只持工具面，不持 tick）+ 三工具映射（IF-P3-05） | `peri-middlewares/src/mcp/builtin/cron_test.rs`（新，`#[path]` 挂载） |
| `peri-middlewares/src/mcp/builtin/lsp.rs`（新） | L-01 | `LspMcpServer` + 配置非空门控（IF-P3-08） | `peri-middlewares/src/mcp/builtin/lsp_test.rs`（新，`#[path]` 挂载） |
| `peri-middlewares/src/mcp/builtin_apply_test.rs` | V-02 | loader 层断言扩展（两实例的默认条目 / `{}` 语义 / 保留名 / off / 非法片段） | 既有挂载于 `peri-middlewares/src/mcp/mod.rs` |
| `peri-middlewares/src/mcp/builtin_runtime_test.rs` | V-02 | 生产启动路径断言扩展（两实例 ready / direct 集合为空 / 关闭矩阵 / tick 关闭无残留 / 门控） | 既有挂载于 `peri-middlewares/src/mcp/mod.rs` |

### 4.3 MCP 中间件、装配与外部集成测试（完整路径逐行列出）

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-middlewares/src/lib.rs` | H-03 | C-04 / L-03 删除 `CronMiddleware` / `LspMiddleware` 旧 re-export，消费点随删，不留 deprecated shim（§9 规则 5） | `peri-middlewares/src/assembly_test.rs` |
| `peri-middlewares/src/mcp/client.rs` | H-02 | 增池字段 + 一次性 setter（`BuiltinInstanceContext`）；池表 `builtin_server_tasks` 值类型换 `BuiltinInstanceSupervisor`（A32） | `peri-middlewares/src/mcp/client_test.rs`（既有） |
| `peri-middlewares/src/mcp/client/lifecycle.rs` | H-02 | 池表值类型换 `BuiltinInstanceSupervisor`；`close_builtin_task` / `close_builtin_tasks` 改 await `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)`；新增 `#[cfg(test)] builtin_tick_is_finished(&self, server_name) -> Option<bool>` | `peri-middlewares/src/mcp/builtin/runtime_test.rs`、`peri-middlewares/src/mcp/builtin_runtime_test.rs` |
| `peri-middlewares/src/mcp/initialize.rs` | H-02 | builtin 分支改调 pool 方法；注入顺序断言由 H-04 接线；`disabled: true` 跳过连接 | `peri-middlewares/src/mcp/builtin_runtime_test.rs` 具名用例 |
| `peri-middlewares/src/mcp/reconnect.rs` | H-02 | 同上；reconnect 先停旧 tick 再建新代 | `peri-middlewares/src/mcp/builtin_runtime_test.rs` 具名用例 |
| `peri-middlewares/src/mcp/config.rs` | — | **不改**（overlay 由注册表派生） | — |
| `peri-middlewares/src/mcp/transport.rs` | — | **不改**三分类事实源；`TransportKind` 与分类函数由此持有 | — |
| `peri-middlewares/src/assembly.rs` | H-03 | 删 `ChainSlot::Cron` 分支；`ChainSlot::Lsp` 分支改注册 `LspSyncMiddleware`；`open_builtin_bridges` **不改** | `peri-middlewares/src/assembly_test.rs` |
| `peri-middlewares/src/assembly/lsp.rs` | H-03 | `add_lsp` 改造（门控 → 薄中间件），复用/扩展 `LspPoolPort`；配置合并后构造 | `peri-middlewares/src/assembly_test.rs` |
| `peri-middlewares/src/assembly/mcp.rs` | H-03 | 只做中间件装配与关闭集投影；**不注入**实例上下文（A33：注入在宿主装配 `peri-acp/src/host/assemble.rs`） | `peri-middlewares/src/assembly_test.rs` |
| `peri-middlewares/src/assembly_test.rs` | H-03 | 槽位序列、链名、关闭矩阵与 `AssemblyContext` 断言；H-03 单一写入 | 具名测试函数 |
| `peri-middlewares/src/assembly/preparation.rs` / `peri-middlewares/src/assembly/workflow.rs` | — | **不改**（只消费 direct） | — |
| `peri-middlewares/tests/mcp_isolation_contract.rs` | V-04 | 保留两台 stdio fixture 回归；新增真实 builtin 路径的 transport/task/state 隔离与生命周期收敛，禁止 off 夹具作为 builtin 隔离证明 | 真实 builtin 路径具名测试 + stdio 回归 |
| `peri-middlewares/src/cron/mod.rs` | C-04 | 保留 scheduler + `CronSchedulerPortHandle`；收口时逐位保持 tick 语义 | `peri-middlewares/src/cron/mod_test.rs`（既有，13 例必须全绿） |
| `peri-middlewares/src/cron/middleware.rs` | C-04 | **删除**（能力面已迁 MCP） | — |
| `peri-middlewares/src/cron/tools.rs` | C-02 | 不改行为（复用为实例工具） | `peri-middlewares/src/cron/tools_test.rs`（既有） |
| `peri-middlewares/src/lsp/mod.rs` | L-03 | 删除旧 `LspMiddleware` re-export（`mod.rs:5`），换出 `LspSyncMiddleware`；保留 `LspTool` 等其余导出，不留 deprecated shim（A31③/§9 规则 5） | `peri-middlewares/src/lsp/middleware_test.rs`、`peri-middlewares/src/assembly_test.rs` |
| `peri-middlewares/src/lsp/middleware.rs` | L-03 | 删除 `LspMiddleware` 工具面，保留/改造为 `LspSyncMiddleware`；按 ready path 后读文件，冻结 cwd/内容来源，不假定 `ToolContext` | `peri-middlewares/src/lsp/middleware_test.rs`（改造） |
| `peri-middlewares/src/lsp/tool.rs` | L-01 | 不改行为（复用为实例工具；`timeout() -> None` 事实保留） | 既有 |
| `peri-middlewares/src/lsp/formatters.rs` | L-01 | 不改行为（复用为实例工具） | 既有 |
| `peri-middlewares/src/permission/mod.rs` | S-01 | 保留敏感判定规则/数量/顺序；模型可见 Markdown 名称从声明表解析 effective name | `peri-middlewares/src/permission/mod_test.rs` 具名用例 |
| `peri-middlewares/src/subagent/mod.rs` | S-01 | 不改判定逻辑；断言扩展 | `peri-middlewares/src/subagent/mod_test.rs` 具名用例 |
| `peri-middlewares/src/hooks/matcher.rs` | S-01 | 不改（匹配型天然支持）；具名断言 | 既有 |
| `peri-middlewares/src/tool_search/declaration.rs` | — | **不改**（四个工具 `prompt_declaration: None`，声明段零变化） | `peri-middlewares/src/tool_search/declaration_test.rs` 具名断言 |
| `peri-middlewares/src/skills/builtin/skills/cron/SKILL.md` | S-05 | 裸名 → effective name，并写关闭/退化语义 | — |

### 4.4 Agent 层（`peri-agent`）

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-agent/src/session/factory.rs` | H-03 | 删 `ChainSlot::Cron`；`AssemblyContext` 字段调整（`cron_scheduler` 保留语义、`lsp_pool` 语义变更）；`peri-middlewares/src/assembly_test.rs` 由 H-03 单一写入 | 无测试模块 ⇒ `peri-middlewares/src/assembly_test.rs` |
| `peri-agent/src/session/exec/stage_builder.rs` | H-03 | `StageBuildInput::lsp_pool` 共享 host pool 语义及传递联动；保留 cron 事件端口 | `peri-agent/src/session/exec/stage_builder/builder_v2_test.rs` |
| `peri-agent/src/session/exec/stage_builder/agent.rs` | H-03 | `AssemblyContext` 的 `lsp_pool` 传入联动 | `peri-middlewares/src/assembly_test.rs` |
| `peri-agent/src/session/exec/stage_builder/builder_v2_test.rs` | H-03 | 同步装配断言，不能保留 session pool 旧前提 | 由 `peri-agent/src/session/exec/stage_builder.rs` 挂载 |
| `peri-agent/src/session/tool_catalog.rs` | — | **不改**（已归一） | — |
| `peri-agent/src/tools/invocation.rs` | — | **不改**（已核无 `LSP` 裸名条目） | — |

### 4.5 ACP host（`peri-acp`）

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-acp/src/host/assemble.rs` | H-04 | ① LSP 配置合并前移到 MCP 池初始化之前；② 构造 host 级 pool 与 cron scheduler；③ 注入 context；④ 删除 MCP 共享 scheduler 的 cron tick 宿主 task | `peri-acp/src/host/mcp_v4_builtin_test.rs`、`peri-acp/src/host/mcp_v4_startup_test.rs` 终态具名函数 |
| `peri-acp/src/host/task_scope.rs` | H-04 | 删 `HostTaskKind::CronTick` | 具名 host task 断言 |
| `peri-acp/src/host/mod.rs` | H-04 | 本波动作是删 ACP tick task 并保留 `HostAssemblyInput::drive_cron_tick` 注入语义；`AcpServerConfig` 加共享 host pool；挂载 `mcp_v4_wave2` 终态模块（不是 baseline） | 具名 host 装配用例 |
| `peri-acp/src/host/shutdown.rs` | H-04 | host shutdown 有界关闭唯一 LSP pool，并断言全部 language server 收敛 | `host::shutdown` 具名用例 |
| `peri-acp/src/host/requests/session_lifecycle.rs` | H-04 | 删除 session 级 pool 构造点；`prepare_existing`、`handle_new`、`handle_fork` 的三处实际调用点与 `create_session_lsp_pool` helper 一并收口；`handle_delete` 不再关闭 host pool | `peri-acp/src/host/requests_test.rs` mock + session/delete 关闭 pool 断言 |
| `peri-acp/src/host/prompt.rs` | H-03 | AssemblyContext 传入同步 | 具名 assembly 用例 |
| `peri-acp/src/host/continuation.rs` | — | **不改**（IF-P3-06 零改动） | — |
| `peri-acp/src/session/cron_bridge.rs` | — | **不改**（IF-P3-06 零改动） | — |
| `peri-acp/src/session/bridges.rs` | — | **不改**（IF-P3-06 零改动） | — |
| `peri-acp/src/event/tool_projection.rs` | S-01 | `infer_tool_kind` 归一后走既有分支 | `event::mapper` 具名用例 |
| `peri-acp/src/host/requests_test.rs` | H-04 | 增加 LSP pool mock 与 session/delete 关闭 pool 断言 | 具名 host 请求用例 |
| `peri-acp/src/host/stdio/run_server_integration_test.rs` | H-04 | 增加端口实现与 session 持 pool 断言 | 具名 stdio 集成用例 |
| `peri-acp/src/host/mcp_v4_wave2_test.rs`（新） | V-03 | 终态三面、触发、同步、关闭、reconnect、取消收敛；函数均在 §8 具名，不能用 baseline 代替 | 由 `peri-acp/src/host/mod.rs` 挂载 `host::mcp_v4_wave2` |
| `peri-acp/src/host/mcp_v4_builtin_test.rs` | V-03 | wave 1 回归、必要时补两实例断言，不替代终态 wave 2 测试 | 由 `peri-acp/src/host/mod.rs` 挂载 `host::mcp_v4_builtin` |
| `peri-acp/src/host/mcp_v4_startup_test.rs` | V-03 | 扩展 readiness/timeout/取消收敛断言；终态必须使用具名函数 | 具名函数 |
| `peri-acp/src/host/mcp_v4_wire_fixture_test.rs` | V-03 | 复用 wire/搜索执行夹具，补充 effective name 与延迟夹具证据 | 具名函数 |
| `peri-acp/src/host/mcp_v4_wave2_baseline_test.rs` | V-01 | 仅保留迁移前 baseline；不得把 baseline 函数当作 V-03 终态验收 | `wave2_baseline_first_request_and_deferred_summary`、`wave2_baseline_lsp_tool_visible_when_server_configured` |

### 4.6 TUI

| 文件 | owner | 动作 | 测试挂载 |
| --- | --- | --- | --- |
| `peri-tui/src/kit/tool_display.rs` | S-03（只审计） | `format_tool_args` 已原样优先、归一后重试，不改生产逻辑 | `peri-tui/src/kit/tool_display_test.rs` |
| `peri-tui/src/kit/tool_display_test.rs` | S-03 | 新增 `cron_lsp_effective_names_reuse_existing_display` 回归 | 由 `peri-tui/src/kit/tool_display.rs` 挂载 |
| `peri-tui/src/truncate.rs` | S-03（只审计） | `summarize_input` / `summarize_output` 已归一重试，`"LSP"` 为共享分支，不改生产逻辑 | `peri-tui/src/truncate_test.rs` |
| `peri-tui/src/truncate_test.rs` | S-03 | 新增 `cron_lsp_effective_names_reuse_existing_summaries` 回归 | 由 `peri-tui/src/truncate.rs` 挂载 |
| `peri-tui/src/app/cron_state.rs` | — | **不改**（A15 TUI 私有 scheduler/tick 缺陷登记） | — |
| `peri-tui/src/app/mod.rs` | — | **不改**（A15 TUI 私有 tick task 明确排除在 MCP 共享 scheduler 验收之外） | — |
| `peri-tui/src/kit/panels/cron.rs` | — | **不改**（A15 既有缺陷登记） | — |

### 4.7 文档与索引

| 文件 | owner | 动作 |
| --- | --- | --- |
| `docs/code-index/peri-acp-types.md` | S-05 | 同步声明表与端口 |
| `docs/code-index/peri-middlewares.md` | S-05 | 同步 cron/lsp builtin、同步中间件与装配 |
| `docs/code-index/peri-lsp.md` | S-05 | 同步端口实现与生命周期 |
| `docs/code-index/peri-acp.md` | S-05 | 同步 host 装配与 pool 生命周期 |
| `docs/code-index/peri-agent.md` | S-05 | 同步槽位与上下文消费点 |
| `docs/code-index/peri-tui.md` | S-05 | 同步已有归一入口的回归说明 |
| `docs/reference/mcp-ecosystem.md` | S-05 | 补两个实例的配置、关闭五维、多 cwd 功能退化与 wave 3 恢复依赖 |
| `docs/design/middleware-system.md` | S-05 | 槽位表与关闭键语义同步 |
| `peri-middlewares/src/skills/builtin/skills/cron/SKILL.md` | S-05 | effective name 与关闭/退化说明同步 |
| `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-acceptance.md`（既有 W0 记录） | V-06 | 只追加现场证据与三态判定；不回填裁决；证据挂载于 acceptance 各具名小节 |

### 4.8 审计后可不改的消费点（非写入清单）

| 文件 | 复核责任 | 审计依据 |
| --- | --- | --- |
| `peri-acp/src/provider/config.rs` | S-02 | `AppConfig::validate_meta_harness` 已消费两表并集 |
| `peri-acp/src/session/frozen.rs` | H-03 | `build_meta_harness_state` 已消费两表并集 |
| `peri-middlewares/src/mcp/middleware.rs` | V-02 | `McpMiddleware::with_builtin_closures` 只改本 turn 投影，readiness 仍按 pool 配置 |
| `peri-middlewares/src/mcp/tool_bridge.rs` | V-02 | `McpToolBridge::invoke` 已使用 ctx 与 120s 超时；本波不改，session 透传留 wave 3 |
| `peri-middlewares/src/mcp/system_tools.rs` | V-02 | `prepare_system_tools` 空数组语义沿用 |
| `peri-tui/src/launch.rs` | H-04 | `drive_cron_tick: true` 保留 |
| `peri-tui/src/cli_print.rs` | H-04 | `drive_cron_tick: false` 保留 |
| `peri-acp/src/host/stdio/mod.rs` | H-04 | `drive_cron_tick: false` 保留 |
| `peri-acp/src/host/workspace.rs` | H-04 | `drive_cron_tick: false` 保留 |

### 4.9 原待生命周期方案的文件（v3 调用点已冻结，待实现）

| 文件 | 唯一 owner | 动作与测试 |
| --- | --- | --- |
| `peri-middlewares/src/mcp/client/lifecycle.rs` | H-02 | **已并入 §4.3（H-02）**（A32 冻结后不再是待定项）：池表值类型换 `BuiltinInstanceSupervisor`，单/批量 close await `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)`，`peri-middlewares/src/mcp/builtin/runtime_test.rs` / `peri-middlewares/src/mcp/builtin_runtime_test.rs` 验证单/批量 close、reconnect、失败清理 |
| `peri-agent/src/session/exec/executor/context.rs` | H-03 | **审计后不改**（A30：cwd/内容来源为装配期注入 + `AfterToolState::cwd()`，端口不读文件）；`SessionContext::lsp_pool` 保留端口、共享 host pool 语义且不新增 owner；更新旧 session 级注释并核对传入，`peri-middlewares/src/assembly_test.rs` 验证；无需扩 `after_tool` hook |
| `peri-lsp/src/pool.rs` | L-02 | 必须实现 §3 IF-P3-09 新方法；`peri-lsp/src/pool_test.rs` 验证 |
| `peri-lsp/src/pool_test.rs` | L-02 | `StubPool` 显式实现新增端口方法，并断言 ready/change/save 调用；挂载于 `peri-lsp/src/pool.rs` |

## 5. 覆盖登记（R1–R29）

> 口径：每条登记「被覆盖的既有契约」+「本波的处置」。标 `→ 不改` 的条目必须在验收记录中给出「未改且仍成立」的证据（测试名或代码位置）。

| R | 既有契约 / 事实 | 本波处置 |
| ---: | --- | --- |
| R1 | 设计文档 v4-part-1 §完整性：`CronMiddleware` / `LspMiddleware` 目标为「完全下放」 | 落实为 builtin 实例；`LspSyncMiddleware` 的**新增**属宿主 seam（A7），在 §10 第 3 项登记为对设计文档的显式细化 |
| R2 | A1 注入点唯一（loader step 6.5） | → 不改；新实例自动派生 |
| R3 | A2 `PERI_MCP_BUILTIN` 紧急闸门语义 | → 不改；off 时四实例零注入 |
| R4 | A3 保留实例名保护（`cron` / `lsp` 已在保留表） | → 不改；补充断言：接管 `cron` / `lsp` 必须 typed error |
| R5 | A4 归一原则（判定型替换 / 匹配型原样优先） | 扩展到四个新 effective name（IF-P3-11）；S-03 仅在已有 TUI 回退入口补 Cron/LSP 注册表新增后的回归断言，不改归一算法 |
| R6 | A5 直连性声明 | 四个工具统一 `direct: false`（IF-P3-02） |
| R7 | A6 三工具面等价 | 三面分别断言（IF-P3-02） |
| R8 | A7 两表形态与三条不变式 | 迁键 + 强度不降（IF-P3-10） |
| R9 | A9 声明段不得丢失 | 四个工具 `prompt_declaration: None` ⇒ 声明段**零变化**（IF-P3-11 / `peri-middlewares/src/tool_search/declaration_test.rs`） |
| R10 | A10/A12/A16/A17 相关 wave 1 裁决 | → 不改；由 §8 第 19 行以具名测试/acceptance 证据闭合，不以旧测试 exit 0 代替 |
| R11 | IF-D1 三分类 transport | → 不改；新实例走同一分支，由 H-05 接线并由 §8 第 20 行验收 |
| R12 | IF-D2 `ConfigSource::Builtin` 传播面 | → 不改；两新实例进入 `discover_tool` / `status` 穷举 match，由 §8 第 21 行验收 |
| R13 | IF-D4 注册表为唯一事实源 | 新实例、工具常量和 owner 归并在 C-01；dispatch 新变体由 H-01 单一写入，§8 第 20 行验收 |
| R14 | IF-D6 七个归一消费点 | 同 IF-P3-11，由 S-01 和 §8 第 11 行验收 |
| R15 | IF-D8 槽位与挂载点删除 | `ChainSlot::Cron` 删除；`ChainSlot::Lsp` 保留并挂 `LspSyncMiddleware`；§8 第 22 行具名验收 |
| R16 | IF-D13 直连性在 `build_typed_tool_bridges` 生效 | → 不改；§8 第 3/19 行断言四工具 direct 与三面结果 |
| R17 | IF-D14 `call_tool` 结果映射唯一实现 | 复用；固定错误文本退化由 §8 第 23 行具名测试/acceptance 记录 |
| R18 | IF-D12 reconnect 与 builtin task 归属 | reconnect 先停旧 tick，再建新代；§8 第 7/15 行观测单驱动与 join |
| R19 | H1：会话级 LSP pool 跨 turn 复用；`session/delete` 关闭 pool | **已裁决功能退化**（A11/A22）：pool 迁 host 级；单 cwd 等价，多 cwd 共享 root_uri；§8 第 13 行 + §10 第 2 项 + 用户文档登记 |
| R20 | 迁移前工具错误文本（`CronError` 两变体 / `LspToolError` 六变体） | **语义变更**（§10 第 5 项）：IF-D14 固定文本；§8 第 23 行具名测试/acceptance 给出清单 |
| R21 | `MIDDLEWARE_TOOL_NAMES` 的 cron 三工具缺口（既有） | **不补**；迁移后不属于 middleware 静态工具；§8 第 24 行断言静态表与索引 |
| R22 | 既有缺陷三件（TUI 双 scheduler / print-stdio 无 tick / LSP 同步覆盖缺口） | **不修**（A15）；V-06 在 acceptance 具名登记「仍存在」 |
| R23 | 关闭语义五维矩阵 | 新增：四类关闭分开验证：MCP 配置 `disabled: true` 跳过连接；MetaHarness `policy_key = false` 只影响本 turn 投影且 readiness 仍走 pool 配置；`PERI_MCP_BUILTIN=off` 零注入；物理 close 停任务/释放资源。可见性 / 同步 / tick / readiness / 物理生命周期分别断言 |
| R24 | 两实例 readiness / 空工具列表 / 配置门控契约 | 新增：builtin enabled 且未关闭时注入；1R ready；LSP 工具集由生效配置非空快照，非进程 ready；非法 overlay 与关闭均具名验收 |
| R25 | 实例 transport/task/state 隔离与生命周期收敛 | 新增：transport、handler task、scheduler/pool 状态隔离；close/reconnect 单驱动；host shutdown 有界关闭全部 LSP server；**四类关闭**（MCP 配置 `disabled:true` 跳过连接 / MetaHarness `policy_key=false` 只关本 turn 投影 / `PERI_MCP_BUILTIN=off` 零注入 / 物理 close 停任务）+ A32 代监督者三态（host shutdown / instance close / reconnect）对象保留/销毁表 |
| R26 | 配置 overlay 空对象 / 非法片段 / 默认注入契约 | 新增：`{}`、disabled、保留名接管、`PERI_MCP_BUILTIN=off` 和非法片段的 typed 结果与注入前提 |
| R27 | 文档、代码索引与内置 skill 同步义务 | 新增：S-05 负责 `docs/code-index/**`、`docs/reference/mcp-ecosystem.md`、关闭/退化说明和内置 skill；V-05 只复核 |
| R28 | W0 基线与收口门禁证据完整性 | 新增：W0 三面基线、搜索 XOR、迁移后命令、exit、`test result`、具名函数、实际命中名单和计数完整落在 acceptance；V-06 收口 |
| R29 | MCP bridge 外层调用时限与 builtin/LSP 端到端时限差异 | 新增登记：`peri-middlewares/src/mcp/tool_bridge.rs:McpToolBridge::invoke` 的 `TOOL_CALL_TIMEOUT = 120s` 包裹 MCP 调用，而 `peri-middlewares/src/lsp/tool.rs:LspTool::timeout` 返回 `None`；复用 `LspTool` 不等于端到端 timeout 等价，§8 新增延迟夹具验收 |

## 6. 任务表（含验证命令）

> 验证命令一律为精确过滤器；`0 tests` 视为失败（§9 规则 1）。凡能确定为单一函数者一律追加 `--exact`；模块前缀用 `::` 结尾。所有命令在 worktree `peri-v4p3` 上顺序执行，**不并发跑 cargo**。

### W1（契约与行为层；不与 W2 并行，先完成依赖契约）

| 任务 | 内容 | 依赖 | 验证命令（具名模块/函数；`0 tests` 失败） |
| --- | --- | --- | --- |
| **C-01** | `peri-acp-types/src/builtin_mcp.rs` 单一 owner：`CRON_TOOLS` / `LSP_TOOLS`、两实例条目；修改 `peri-acp-types/src/builtin_mcp_test.rs:tools_are_non_empty_and_unique_per_instance` 以落实 `prompt_declaration: None` | — | `cargo test -p peri-acp-types --lib -- builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance` |
| **C-02** | `CronMcpServer`（不持 tick，复用 `cron/tools.rs`）+ 三工具映射 | C-01 | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::cron_server_maps_three_tools_without_tick --exact` |
| **C-03** | 代监督者 tick 关闭 / reconnect 单驱动与 join 断言（消费 A32 API：`BuiltinInstanceSupervisor` / `TickGuard::shutdown` / `BuiltinCloseOutcome`） | C-02 | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers --exact` |
| **C-04** | 删除 `peri-middlewares/src/cron/middleware.rs`、收口 `peri-middlewares/src/cron/mod.rs`，保持 scheduler/port 语义；并**同批**摘除 `ChainSlot::Cron`（`peri-agent/src/session/factory.rs` 变体 + 槽位表 + `peri-middlewares/src/assembly.rs` 分支）与 `CronMiddleware` 旧 re-export；**A8 的 cron 半边迁键同批落地**：`MIDDLEWARE_NAMES` 删 `"CronMiddleware"`、`BUILTIN_INSTANCE_POLICY_KEYS` 增 `"CronMiddleware"`（否则旧关闭键 `CronMiddleware: false` 变未知键被拒；LSP 半边归 L-03） | C-02 | `cargo test -p peri-middlewares --lib -- cron::tests::test_tick_fires_trigger`；`cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-acp-types --lib -- meta_harness::tests`（期望：仅 `builtin_instance_policy_keys_match_declaration_table` 因 LSP 半边未迁而红，其余全绿） |
| **L-01** | `LspMcpServer` + 按配置非空快照工具面 | C-01 | `cargo test -p peri-middlewares --lib -- mcp::builtin::lsp::tests::list_tools_follows_configured_server_set` |
| **L-02** | `LspPoolPort` 按 IF-P3-09/A30 加 ready 查询和 async typed change/save（无默认实现）；`peri-lsp/src/pool.rs` / `peri-lsp/src/pool_test.rs` 显式实现，无默认 no-op；host 替身由 H-04 写入 | — | `cargo test -p peri-lsp --lib -- pool::tests::port_ready_for_reflects_routed_server_state --exact`（A30 具名用例；另跑旧回归 `pool::tests::test_lsp_pool_port_downcast_roundtrip --exact`）；断言 ready/change/save 调用计数与参数才计同步闭合 |
| **L-03** | `LspSyncMiddleware` 薄中间件（读文件、顺序发送、失败降级）；`peri-middlewares/src/lsp/mod.rs` 与 `peri-middlewares/src/lib.rs` 旧 `LspMiddleware` re-export 随删，`peri-middlewares/src/assembly/lsp.rs` 机械换名（门控重排仍归 H-03）；**A8 的 lsp 半边迁键同批落地**：`MIDDLEWARE_NAMES` 删 `"LspMiddleware"` 增 `"LspSyncMiddleware"`、`MIDDLEWARE_TOOL_NAMES` 删 `"LSP"`、`BUILTIN_INSTANCE_POLICY_KEYS` 增 `"LspMiddleware"` | L-02 | `cargo test -p peri-middlewares --lib -- lsp::middleware::tests::write_sync_orders_change_then_save`；`cargo test -p peri-acp-types --lib -- meta_harness::tests`（本批后迁键全绿）；`cargo test -p peri-middlewares --lib -- assembly::tests` |
| **H-01** | `peri-middlewares/src/mcp/builtin/dispatch.rs` 搬迁、`peri-middlewares/src/mcp/builtin/web.rs` 收敛；dispatch 文件唯一 owner | — | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::dispatch_factory_covers_implemented_instances_only --exact` |
| **H-02a** | **A32 代监督者先行**（同一 owner 拆分前置）：`TickGuard` / `BuiltinInstanceSupervisor` / `BuiltinCloseOutcome` / `BuiltinTransport.tick` + `into_parts()`；`peri-middlewares/src/mcp/client.rs` 表值类型与 `peri-middlewares/src/mcp/client/lifecycle.rs` 的 `close_builtin_task(s)` 改为 await `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)`；**必须先于 C-02/C-03**（它们消费该 API），H-02 的 context/pool 方法部分仍在 W2 | H-01 | `cargo test -p peri-middlewares --lib -- mcp::builtin::runtime::tests::supervisor_close_orders_tick_before_server_task --exact` |

### W2（装配与宿主接线；W1 完成后串行）

| 任务 | 内容 | 依赖 | 验证命令（具名模块/函数；`0 tests` 失败） |
| --- | --- | --- | --- |
| **H-02** | `BuiltinInstanceContext` 公开 assembly 工厂 + 关闭集字段 + pool 字段/setter + pool 方法 + `BuiltinInstanceSupervisor` / `TickGuard` / `BuiltinCloseOutcome`（A31/A32/A33）；一次性注入/晚注入 typed Err | C-01/L-01/H-01 | `cargo test -p peri-middlewares --lib -- mcp::builtin::runtime::tests::supervisor_close_orders_tick_before_server_task --exact`；`cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances --exact` |
| **H-03** | `peri-middlewares/src/assembly/lsp.rs`、`peri-middlewares/src/assembly.rs`、`peri-middlewares/src/assembly/mcp.rs`、`peri-agent` 槽位与 `peri-middlewares/src/assembly_test.rs` 单一写入；注入晚于配置合并 | L-02/L-03/H-02 | `cargo test -p peri-middlewares --lib -- assembly::tests::lsp_pool_port_injected_registers_middleware`；`cargo test -p peri-middlewares --lib -- assembly::tests::middleware_names_match_production_blueprint` |
| **H-04** | 宿主装配重排、host pool/shutdown、删除三处 session pool 构造与共享 scheduler 的 ACP Startup/CronTick task；修改两份 host LSP 端口替身 | H-03/C-04 | 拟新增 `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown --exact`；另记录修改后的 session/delete 与 stdio pool 具名函数及命中名单；`cargo test -p peri-acp --lib -- host::requests::tests::lifecycle_cases::test_delete_active_session_does_not_shutdown_shared_host_lsp_pool --exact`（终态函数名以代码为准）；baseline 不计终态 |
| **H-05** | dispatch 集成：新增 enum 变体 + 工厂接线；文件写入 owner 仍为 H-01，依赖 C-02/L-02 | C-02/L-02/H-01/H-02 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::dispatch_covers_cron_and_lsp_variants --exact` |
| **S-01** | permission/subagent/tool projection 判定归一 | H-03 | `cargo test -p peri-middlewares --lib -- permission::tests::builtin_effective_names_match_original_name_policy`；`cargo test -p peri-middlewares --lib -- subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names` |
| **S-02** | **两表迁键终态复核**（迁键本身已按 A8 拆到 C-04 的 cron 半边与 L-03 的 lsp 半边，各自与链上摘除同批以保证 known-key 与不变式全绿）：复核 `MIDDLEWARE_NAMES` = 链槽位名 + `LspSyncMiddleware`、`BUILTIN_INSTANCE_POLICY_KEYS` = 四实例 policy_key、`MIDDLEWARE_TOOL_NAMES` 无 `LSP`；发现漂移回写 `peri-acp-types/src/meta_harness.rs`（唯一 owner） | C-04/L-03 | `cargo test -p peri-acp-types --lib -- meta_harness::tests`；`cargo test -p peri-middlewares --lib -- assembly::tests::middleware_names_match_production_blueprint` |
| **S-03** | TUI 已有归一入口上的 Cron/LSP 注册表新增回归，不改生产归一逻辑 | C-01/S-01 | 拟新增 `cargo test -p peri-tui --lib -- kit::tool_display::tests::cron_lsp_effective_names_reuse_existing_display --exact`；`cargo test -p peri-tui --lib -- truncate::tests::cron_lsp_effective_names_reuse_existing_summaries --exact`；分别记录实际命中名单与计数 |
| **S-04** | 内置 skill、文档与索引文本改动清单预审（不写 S-05 文件） | C-01/S-01 | 人工逐文件列出待同步项；不得宣称已落地 |
| **S-05** | 文档与索引落地：`docs/code-index/**`、`docs/reference/mcp-ecosystem.md`、关闭/退化语义、内置 skill 文本；V-05 只复核 | H-04/S-04 | 人工核对 `DOC-UPDATE-001` 清单；acceptance 记录文件清单 |

### W3（验证与收口）

| 任务 | 内容 | 依赖 | 验证命令（具名模块/函数；`0 tests` 失败） |
| --- | --- | --- | --- |
| **V-01** | **迁移前基线**：在生产改动前采集 W0 | — | 已有 `host::mcp_v4_wave2_baseline::wave2_baseline_first_request_and_deferred_summary`、`wave2_baseline_lsp_tool_visible_when_server_configured`；acceptance §2 记录命令/计数 |
| **V-02** | builtin overlay / ready / 空工具列表 / 关闭五维 / tick / 门控 | W1/H-02 | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests::overlay_inserts_complete_default_entries`；`cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances` |
| **V-03** | host 三面、cron 触发、LSP 同步、关闭交叉、multi-cwd/shutdown/reconnect；终态用拟新增 `peri-acp/src/host/mcp_v4_wave2_test.rs`，由 `peri-acp/src/host/mod.rs` 挂载 `mcp_v4_wave2` | W2/S-05 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary --exact --nocapture`；其余 §8 拟新增终态具名函数各自同样 `--exact` 执行；记录「新增/修改函数名 + 实际命中名单」，命中必须为目标函数而非 baseline；新增函数未挂载或命中 0 即失败 |
| **V-04** | `peri-middlewares/tests/mcp_isolation_contract.rs` 保留 stdio transport/task/state 回归，并新增真实 builtin handler/tick/state 隔离 | H-04/H-05 | `cargo test -p peri-middlewares --test mcp_isolation_contract`（按测试目标名运行，禁止追加不存在的模块过滤器）；acceptance 记录 stdio 回归与 builtin 新增/修改函数名、实际命中名单、命令和计数 |
| **V-05** | 文档/索引/内置 skill / `DOC-UPDATE-001` 复核（不写 S-05 文件） | S-05 | 人工逐文件核对；acceptance 记录文件清单 |
| **V-06** | 验收记录撰写与收口证据：每条矩阵具名函数、命令、exit、`test result`、计数 | V-01…V-05 | acceptance §4–§7；缺任一计数或 `0 tests` 判失败 |

### 全量门禁（收口必跑；实施阶段执行，本次计划修订不执行）

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
bash scripts/check-layer-imports.sh    # 若存在；否则按仓库既有门禁脚本
```

## 7. wave 划分与闸门

| 阶段 | 闸门（不满足不得进入下一阶段） |
| --- | --- |
| **W0 基线** | V-01 已在迁移前 HEAD 采集：首个 LLM 请求直连表 18 项、摘要 mcp 前缀过滤、搜索面裸名 XOR 事实，证据在 acceptance §2；后续不得改写 W0 |
| **W1 行为层** | C-01…C-04、L-01…L-03、H-01 完成；`mcp::builtin::{dispatch,cron,lsp,runtime}` 与 `cron::tests` 全绿，`0 tests` 失败；不触碰装配面 |
| **W2 装配** | H-02…H-05、S-01…S-05 完成；配置合并先于 handler 构造；`assembly::tests`、`peri-acp` 具名 host 用例、两表不变式、TUI 全绿 |
| **W3 端到端** | V-02/V-03/V-04 全绿；覆盖 cron 注册→tick→触发→审批→continuation、Write→didChange→didSave、关闭五维、多 cwd/shutdown/reconnect |
| **W4 收口** | V-05/V-06 完成；全量门禁四命令全绿；每条验收行有函数名/命令/exit/计数；acceptance 三态列完整，未闭合项标 `PARTIAL` / `UNVERIFIED` / `BLOCKED` |

## 8. 验收矩阵（可证伪面）

> 本表 `host::mcp_v4_wave2::*` 均为拟新增终态函数，唯一文件为 `peri-acp/src/host/mcp_v4_wave2_test.rs`，由 `peri-acp/src/host/mod.rs` 挂载；baseline 唯一文件为 `peri-acp/src/host/mcp_v4_wave2_baseline_test.rs`。`mcp::builtin_runtime_tests::*` / `mcp::builtin::cron::tests::*` / `mcp::builtin::lsp::tests::*` 的唯一测试文件分别为 `peri-middlewares/src/mcp/builtin_runtime_test.rs` / `peri-middlewares/src/mcp/builtin/cron_test.rs` / `peri-middlewares/src/mcp/builtin/lsp_test.rs`。新增/修改函数必须按 §9 规则 13 记录实际命中名单；符号名和完整路径一起索引，不以模块宽过滤器充数。

| # | 断言 | 承担任务 | 证据形式 |
| ---: | --- | --- | --- |
| 1 | 保留名接管 `cron` / `lsp`（`command`/`url`）加载期 typed error；`{"cron":{}}` / `{"lsp":{}}` 语义与 `{"web":{}}` 同构；非法关闭片段被拒 | V-02 | `peri-middlewares/src/mcp/builtin_apply_test.rs` 具名用例 + 命令输出 |
| 2 | 两实例在 1R 前 ready（transport + 协议初始化 + 能力协商 + `tools/list`）；注入失败 / 超时 ⇒ fatal 且不发布 ready | V-02 | `peri-middlewares/src/mcp/builtin_runtime_test.rs` 具名用例 |
| 3 | **三面与搜索重命名对照**：①首个 LLM 请求直连参数不含四工具；②摘要面迁移前可见裸名、迁移后因 `mcp__` 过滤四个 effective name 整体缺席，只作裸名消失证据；③搜索/执行面（`SearchExtraTools` / `ToolIndex`）逐工具满足裸名 XOR effective name，按重命名映射判等价，不作字面集合相等 | V-01 / V-03 | 基线 `peri-acp/src/host/mcp_v4_wave2_baseline_test.rs:wave2_baseline_first_request_and_deferred_summary` / `wave2_baseline_lsp_tool_visible_when_server_configured`；终态 `wave2_final_first_request_and_deferred_summary`（**已落地**，同一函数覆盖「LSP 配置非空」与「无 LSP 配置」两侧）+ `lsp_handler_constructed_after_config_merge`（**已落地**，配置合并在 handler 构造之前）；原计划名 `wave2_final_lsp_tool_visible_when_server_configured` 已并入前者 B 段，作废（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；命令 + `test result` + 实际命中名单 |
| 4 | `system_mcp_tools == []` ⇒ 不提升 direct、不做必需工具校验 | V-02 | `mcp::builtin_runtime_tests::startup_gate_stages_declared_direct_tools_and_required_set`；具名输出 |
| 5 | **LSP 门控**：builtin enabled 且未被配置关闭时实例 ready；`LspServerPool::has_servers()` = 生效配置非空，无配置时空工具，有配置即出现；不以 server 进程 ready 为谓词；构造晚于配置合并且不支持热更新 | V-02 / H-04 | 拟新增 `list_tools_follows_configured_server_set`（`peri-middlewares/src/mcp/builtin/lsp_test.rs`）与 `lsp_handler_constructed_after_config_merge`（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；命令 + 实际命中名单 + 计数 |
| 6 | **cron 事件链路端到端**：effective register → tick → 触发 → 审批 → `QueuedMessage(MessageSource::CronTrigger)` → continuation | V-03 | `host::mcp_v4_wave2::cron_register_tick_approval_continuation` + 审批调用计数 |
| 7 | **tick 生命周期**：每代代监督者一个 `TickGuard`（`CronMcpServer` 不持 tick）；关闭显式 cancel + join；>2×interval 无新触发且 task 已 join；reconnect 先停旧代，随后每 interval 恰 1 tick；验收范围仅为驱动 MCP 所用共享 scheduler 的 ACP Startup / CronTick task，TUI 私有 `peri-tui/src/app/cron_state.rs:CronState::spawn_tick_task` 明确排除；禁止全仓 grep 零 tick | C-03 / V-02 / V-03 | `tick_shutdown_joins_task_and_stops_triggers`、`tick_reconnect_has_single_driver_per_interval`（已落地 `c99847f2`，`peri-middlewares/src/mcp/builtin/cron_test.rs`）、拟新增 `reconnect_has_single_tick_driver`（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；命令 + `test result` + task join 观测 |
| 8 | **LSP 同步链路**：`after_tool` 无 `ToolContext`，cwd 来自 `AfterToolState::cwd()`；path ready 检查先于读文件；change 失败仍 save，错误 debug 降级且不改变工具结果；同文件跨调用无 FIFO 保证，分别验证各调用顺序 | L-02 / L-03 / V-03 | **已落地**（`peri-middlewares/src/lsp/middleware_test.rs`，8 例）：`write_sync_orders_change_then_save` / `not_ready_skips_read_and_notifications` / `not_ready_with_missing_path_skips_file_access` / `change_error_still_attempts_save` / `relative_path_resolved_against_state_cwd` / `non_write_tools_ignored` / `missing_or_non_string_file_path_skips_port` / `read_failure_degrades_to_ok`；原计划名 `not_ready_skips_file_read` → `not_ready_skips_read_and_notifications`、`change_failure_still_saves` → `change_error_still_attempts_save`，`concurrent_sync_preserves_per_call_order` **未落地**（本行已冻结「同文件跨调用无 FIFO 保证」：单次调用内顺序由 `write_sync_orders_change_then_save` 覆盖，端到端计数由 `mcp::builtin_runtime_tests::mw_await_lsp_accounting` 覆盖）；**拟新增** `lsp_sync_does_not_change_tool_result`（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；断言 ready/change/save 替身计数、参数、失败顺序，不能只测 downcast |
| 9 | 两表三条不变式 | S-02 / V-02 | `peri_acp_types::meta_harness::tests::builtin_instance_policy_keys_match_declaration_table`、`middleware_names_and_builtin_policy_keys_are_disjoint`、`known_key_union_covers_builtin_policy_keys`；命令 + passed 计数 |
| 10 | **四类关闭 × 五维矩阵**：合法 MCP `disabled: true` 配置在 initialize 跳过连接、无 handler/tick、不宣称 ready（非法组合仍校验报错）；MetaHarness `policy_key=false` 只影响本 turn 投影，保留 handler/pool/tick，readiness 仍按 pool 配置；`PERI_MCP_BUILTIN=off` 零注入、无 builtin handler/tick/工具；物理 close 停对应任务，instance close 保留组合根状态，host shutdown 才关共享 LSP pool。另测策略交叉：仅 `LspSyncMiddleware:false` 工具仍可用且不读/不发；仅 `LspMiddleware:false` 工具与同步目标关闭且不读/不发；两者开正常同步，两者关均关闭。五维为可见性/同步/tick/readiness/物理生命周期 | V-02 / V-03 | 拟新增 `policy_close_five_dimensions` / `four_close_sources_are_distinct`（`peri-middlewares/src/mcp/builtin_runtime_test.rs`）、`lsp_sync_close_cross_matrix`（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；矩阵逐格记录，不把 disabled/off 当策略关闭 |
| 11 | 归一判定等价：effective list/remove/LSP 不审批、非 mutation；effective register 审批 + mutation；permission 模型可见 Markdown 显示名从声明表解析 effective name，但保留敏感规则数量/顺序 | S-01 / V-02 | 拟新增 `permission::tests::builtin_effective_names_match_original_name_policy` / `permission::tests::cron_sensitive_markdown_uses_registry_effective_name`（`peri-middlewares/src/permission/mod_test.rs`），`subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names`（`peri-middlewares/src/subagent/mod_test.rs`）；S-03 两个 TUI 回归函数及命中名单见 §6 |
| 12 | 声明段零变化：四个 effective name 的 `prompt_declaration` 均为 None，不进入声明段；现有 web/artifact 模板声明保持 | V-02 / C-01 | 修改 `peri_acp_types::tests::tools_are_non_empty_and_unique_per_instance` + `mcp::builtin::tests::builtin_prompt_declaration_matches_registry`；`tool_search::declaration_test` 具名函数；命令 + 计数 |
| 13 | **LSP pool 作用域变更**：单 cwd root_uri 逐字等价；多 cwd 共享 host cwd 明确为功能退化；host shutdown 有界关闭全部 language server，观测无 orphan；wave 3 经 `ToolContext` 恢复 per-session | H-04 / V-03 / S-05 | `host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown`；acceptance 语义变更、资源计数与用户文档 |
| 14 | 既有缺陷三件仍存在（不修） | V-06 | acceptance 具名登记现状与证据位置 |
| 15 | **真实 builtin 隔离**：Cron/LSP transport、handler task、tick/state 可观测独立；关闭互不影响，host shutdown / instance close / reconnect 收敛；off 零注入单独断言，现有 stdio 回归不证明 builtin 隔离 | V-04 / V-03 | `cargo test -p peri-middlewares --test mcp_isolation_contract`；保留 `distinct_instances_keep_distinct_pool_entries_and_handle_identity` / `each_instance_wire_carries_only_its_own_requests` / `disabling_one_instance_leaves_the_other_transport_intact` / `builtin_injection_off_leaves_pool_with_exactly_the_fixture_servers`，拟新增根级 `instances_have_independent_transport_task_and_state`（与既有四个 stdio fixture 用例**不同**：本用例走真实 builtin cron/lsp；原计划名 `builtin_cron_lsp_have_independent_handler_tick_and_state` 作废，两名字指同一用例，子计划沿用现名；均在 `peri-middlewares/tests/mcp_isolation_contract.rs`）；另测 `lifecycle_state_matrix`（`peri-acp/src/host/mcp_v4_wave2_test.rs`），记录两组实际命中名单/计数 |
| 16 | **三面迁移等价**：直连参数、摘要时点、搜索/执行可达性分别对照；等价判据只有搜索面裸名 XOR effective name 重命名映射 | V-01 / V-03 | baseline/终态具名函数，acceptance 记录两次命令、exit、`test result`、计数 |
| 17 | 文档、代码索引、内置 skill 与 permission 模型文本无裸名工具调用残留；`sensitive_tool_entries` / `format_sensitive_tools`（`peri-middlewares/src/permission/mod.rs`）保留敏感判定规则、14 项数量与顺序，模型显示名经 `builtin_tool_effective_name` 查注册表。文档写明四类关闭×五维、多 cwd 退化/wave 3 依赖 | S-01 / S-05 / V-05 | §4.7 完整文件清单 + `DOC-UPDATE-001`；`cron_sensitive_markdown_uses_registry_effective_name`（`peri-middlewares/src/permission/mod_test.rs`）验证渲染输出，不能把内部敏感判定裸名常量当成文本残留 |
| 18 | 全量门禁四命令全绿 | V-06 | 每条命令原文 + exit + `test result`/计数；不得用旧测试通过替代 |
| 19 | **R10** wave 1 兼容语义（transport、能力声明、验证口径）不被本波破坏 | V-02 / V-03 | 具名新增/修改函数 + 命令 + exit + `test result` + passed 计数 |
| 20 | **R12/R13** `ConfigSource::Builtin` 穷举传播、注册表唯一事实源和 dispatch 新变体接线 | H-05 / V-02 | 已有 `mcp::builtin::tests::effective_tool_names_covers_registry` + 新增 `dispatch::tests::all_registered_instances_have_handler` / `builtin_source_propagates`；acceptance 记录新增函数、命令、exit、计数 |
| 21 | **R15** 槽位删除/保留与挂载点：Cron 删除，Lsp 为 sync；**R12** 状态/发现传播同时覆盖 | H-03 / S-02 | `assembly::tests::production_chain_has_only_lsp_sync_slot`、`meta_harness::tests::known_builtin_keys_are_exhaustive` |
| 22 | **R17** IF-D14 结果映射仍为唯一实现且 effective 调用成功/失败形态一致 | H-05 / V-02 | `mcp::builtin::dispatch::tests::call_tool_uses_shared_result_mapping` |
| 23 | **R20** 工具错误文本退化清单与固定脱敏文本显式记录 | V-06 / S-05 | acceptance 列出新增/修改测试函数名、命令、计数及六类 LSP/两类 Cron 文案 |
| 24 | **R21** cron 三工具不进入 `MIDDLEWARE_TOOL_NAMES`，effective 名进入 builtin 注册表/搜索执行面 | S-02 / V-02 | `meta_harness::tests::cron_tools_are_not_middleware_static_tools` + `host::mcp_v4_wave2::search_execution_uses_effective_names` |
| 25 | **R29：外层 120s timeout 与取消收敛**：真实 builtin LSP 延迟夹具分别覆盖正常返回、超过 `TOOL_CALL_TIMEOUT`、调用方取消；复用 `LspTool::timeout() -> None` 不代表端到端等价。分别观测调用返回、handler 内在途请求、取消与显式 close 后 task join；不得把 client timeout 当作 server 自动取消成功 | L-01 / V-02 / V-03 / V-06 | 拟新增 `builtin_lsp_timeout_and_cancellation_converge`（`peri-middlewares/src/mcp/builtin_runtime_test.rs`）与 host 取消具名函数（`peri-acp/src/host/mcp_v4_wave2_test.rs`）；延迟夹具记录时钟/120s 边界、调用/在途计数、取消/close/join；完整命令、实际命中名单、exit、`test result`、计数入 acceptance，收敛失败标 `BLOCKED` |

## 9. 施工规则

1. **`0 tests` 即失败**：任何跳过 / 过滤掉全部用例的命令结果不得计为通过。
2. **新模块必须配对测试并挂载**：`#[path]` 挂载缺失 = 假绿；`_test.rs` 后缀是层导入检查脚本的豁免判据（wave 1 的 R37 教训）。
3. **单一 owner**：同一文件同一波次只有一个 owner 任务；跨任务改动先在 §4 追加行。
4. **无测试模块的 crate 不挂断言**：`peri-agent/src/session/factory.rs` 无测试模块 ⇒ 断言挂到 `peri-middlewares/src/assembly_test.rs`（wave 1 A14 口径）。
5. **删除优于兼容**：迁移后立即删除 `peri-middlewares/src/cron/middleware.rs` 与 `LspMiddleware` 的活体结构，不留 deprecated shim、不留 `#[allow(dead_code)]` 兜底（含 `peri-middlewares/src/mcp/builtin/mod.rs` 的过期模块级豁免）。
6. **不并发跑 cargo**：所有验证命令串行执行，避免 fd / 锁竞争导致的假阴性。
7. **夹具不含真实 secret**：新增夹具不得抄录 env / headers / URL 认证信息；统一临时 `HOME` / 配置目录。既有 stdio 隔离回归使用 `PERI_MCP_BUILTIN=off`；真实 builtin 隔离、超时/取消验收必须显式启用 builtin 并只装配受控配置，不能沿用 off 夹具冒充 builtin 路径。
8. **错误文本脱敏**：`BuiltinSpawnError` / `BuiltinOverlayError` / `call_tool` 失败文本只含实例名、工具名与固定规则文本，不含路径 / env / 凭据。
9. **登记不静默**：语义变更（A11 / R20）必须在 acceptance 的「语义变更」小节逐条列出，禁止只写「行为等价」。
10. **禁止范围蔓延**：A15 三件既有缺陷、`peri-lsp` 内部缺口、workspace 相关改动一律不进本波；发现必要的连带改动时先写裁决记录。
11. **事件载荷用 effective name**：归一仅用于判定 / 匹配；transcript / ACP 事件 / MCP wire 不得改动。
12. **文档与索引同步**（`DOC-UPDATE-001`）：代码改动涉及路由、符号、槽位表、skill 文本时，同批更新 `docs/code-index/**`、`docs/reference/**`、`docs/design/**` 与内置 skill。
13. **非零旧测试通过不计闭合**：每条验收行必须具名到具体测试函数与 repository-relative 文件；acceptance 必须给出「新增/修改函数名 + 实际命中名单」、完整命令、exit、`test result` 与 passed 计数。`cargo test -p peri-acp --lib -- host::mcp_v4_wave2` 会命中既有 `host::mcp_v4_wave2_baseline`（挂载于 `peri-acp/src/host/mod.rs`），不得当作终态通过；V-03 用完整终态函数名 + `--exact`，核对命中名单无 baseline 替代。V-04 用 `cargo test -p peri-middlewares --test mcp_isolation_contract` 按目标执行，现有根级函数名不带目标名模块前缀；stdio/off 回归与真实 builtin 新增用例分别计数。仅旧测试非零通过、宽过滤器通过或 `0 tests` 均不得闭合该行。

## 10. 风险与登记项

| # | 项 | 状态 | 说明与依据 |
| ---: | --- | --- | --- |
| 1 | cron 触发走 MCP 协议通知（路线 A） | **本波不做，非协议绝对不可行结论**（A3/A16） | `peri-middlewares/src/mcp/client/transport.rs` 存在 `ChannelHandler` 分支，`peri-middlewares/src/mcp/initialize.rs` 传递它；当前被引用的宿主启动路径传 `None`。否决理由是接线、生命周期与审批通路成本；`ServerHandler` 方法面不等于全部发送 API，不作协议不可行或必须改泛型 seam 的断言。本波保留 `CronSchedulerPort` 路线 B。复核点：未来基于当时 rmcp 版本重新核对 handler、通知语义与审批门 |
| 2 | LSP pool 由 per-session 变 per-host（A11/A22/F1） | **已裁决的功能退化，必须文档化** | `McpToolBridge::invoke`（`peri-middlewares/src/mcp/tool_bridge.rs`）已使用 ctx；真正缺口是 `invoke_tool_call`（`peri-middlewares/src/mcp/builtin/web.rs`）另造 `ToolContext::new(&[], cwd)`，未贯通宿主 cwd/session 上下文。本波沿用 host cwd/root_uri；单 cwd 的 root_uri 等价不扩张为 timeout 等价（第 12 项），多 cwd 明确退化。`session/delete` 不再关 pool；host shutdown 有界关闭全部 language server；wave 3 才恢复 per-session 调用上下文 |
| 3 | `LspSyncMiddleware` 与 `LspPoolPort` 合流（A7/A23/F7） | **原施工阻塞已由 v3 契约消除，待实现验证** | §3 IF-P3-09 冻结 path/async 完整签名、契约 crate typed error、ready-before-read、change 失败仍 save、单调用有序但跨调用无 FIFO 保证；cwd 从 `AfterToolState::cwd()`、内容从磁盘读取。四个实现者必须显式实现且有调用计数，禁止 no-op 假绿；两策略关闭键交叉矩阵见 §8 第 10 行 |
| 4 | 既有缺陷三件 | **不修，登记** | TUI 私有双 scheduler/tick、print/stdio 无 tick、LSP 同步覆盖缺口保持现状；本波“宿主无第二驱动”只针对驱动 MCP 所用共享 scheduler 的 ACP Startup / CronTick task，不包含 `peri-tui/src/app/cron_state.rs:CronState::spawn_tick_task`；禁止全仓 grep 零 tick |
| 5 | 工具错误文本退化（R20） | **已知语义变更，显式登记** | IF-D14 把 Err 映射为固定脱敏文本；受影响文案清单由 acceptance §4/§8 第 23 行记录；本波不新增结构化原因例外 |
| 6 | capability root / 凭据隔离 | **UNVERIFIED**（沿用 wave 1 A13） | 无 per-instance 可观察面（`capability_profile` 与 `execution_cwd` 均 pool 级） |
| 7 | `cron` tick 生命周期与关闭顺序（A25/A32） | **已冻结，待实施观测** | 每代代监督者（`BuiltinInstanceSupervisor`）一个 `TickGuard{cancel, join}`（handler 不持 tick）；显式 `shutdown().await`，Drop 仅兜底；同 scheduler 单驱动，reconnect 先停旧再起新。断言：关闭后 >2×interval 无新触发且 task 已 join；重连后每 interval 恰 1 tick。若 runtime 收敛不触发显式 close，先追加裁决，不得以 Drop 代替 |
| 8 | `system_mcp` 硬依赖与注入时序 | **风险接受，时序已冻结** | 两实例 `system_mcp: true`，ready 失败 fatal；handler 未接线/上下文缺失是编程错误面，fail-fast 合理；上下文必须在 initialize 前一次性注入，重复注入 typed Err、首个生效，runtime 记录并拒绝 |
| 9 | LSP 配置前移的装配顺序风险 | **待验证** | 配置合并必须先于 handler 构造；需核对插件加载和 settings 读路径，具名 host 测试记录顺序 |
| 10 | 摘要面变化（A20） | **已知语义变更，显式登记** | `format_deferred_list` 的 `!name.starts_with("mcp__")` 使四个 effective name 整体缺席摘要；摘要只作裸名消失时点证据，搜索面才作重命名 XOR 对照 |
| 11 | 用户配置关闭键对物理生命周期的误读 | **已登记，四类关闭分开** | `peri-middlewares/src/mcp/initialize.rs:McpClientPool::initialize_config` 的 `disabled: true` 跳过连接；`peri-middlewares/src/mcp/middleware.rs` 的 `policy_key = false` 只影响本 turn 投影且 readiness 仍走 pool 配置；`PERI_MCP_BUILTIN=off` 零注入；物理 `close` 才停任务。仅策略关闭保留 handler/pool/readiness；五维矩阵见 §8 第 10 行与用户文档 |
| 12 | MCP bridge 外层 120s timeout（F2 / R29） | **新登记：端到端语义变化，待延迟夹具验证** | `McpToolBridge::invoke` / `TOOL_CALL_TIMEOUT`（`peri-middlewares/src/mcp/tool_bridge.rs`）以 120s 包裹 `peer.call_tool`；`LspTool::timeout`（`peri-middlewares/src/lsp/tool.rs`）返回 `None`。复用业务工具不等于端到端 timeout 等价，也不证明 handler 自动收到取消；本波不修改 bridge 时限，§8 第 25 行分别验证超时、取消和显式 close 后收敛 |

## 11. 复用的既有接口（不得改语义）

| 接口 | 说明 |
| --- | --- |
| IF-M1 `system_mcp` / `system_mcp_tools` / `system_mcp_timeout` 三字段与校验 | 本波只消费，不改形状 |
| IF-M2 `prepare_system_tools` / `with_direct` / 原始名精确匹配 / all-or-nothing | 空数组语义按现状 |
| IF-M3 `DiscoveryEvidence` 严格 discovery | 不改 |
| IF-M4 `before_react_start` 启动闸门（不新增 hook） | 不改 |
| IF-M5 `StartupToolUpdate` 原子提交 | 不改 |
| IF-D1 / IF-D2 / IF-D11 / IF-D12 三分类 transport、`ConfigSource` 传播、`transport_type`、reconnect 归属 | 不改；三分类事实源是 `TransportKind` / `TransportConfig::kind`（`peri-middlewares/src/mcp/transport.rs`）；`serve_client_auto`（`peri-middlewares/src/mcp/client/transport.rs`）是 client handler 适配而非三分类定义 |
| IF-D13 `build_typed_tool_bridges` 直连性生效点 | 不改 |
| IF-D14 `call_tool` 结果映射唯一实现 | 复用；退化见 §10 第 5 项 |
| IF-D15 归一 helper（纯查表） | 不改；新实例字面量进入该表的索引 |
| `CronSchedulerPort`（`peri-acp-types/src/cron.rs`） | 不改 |
| `LspPoolPort`（`peri-acp-types/src/ports.rs`） | **复用并合流同步能力**（A23/F7）：当前仅 `as_any` / `shutdown`；新增 path ready 查询与 async typed `did_change` / `did_save` 的完整契约、错误归属、实现者矩阵以 §3 IF-P3-09 为准。端口不读文件；薄中间件从 `AfterToolState::cwd()` 解析路径，ready 后才读文件 |
| `LspServerPool`（`peri-lsp/src/pool.rs`） | 复用惰性配置注册、`has_servers()` 与 shutdown，不改内部协议业务行为；工具门控非进程 ready。host pool 退化依据是 builtin helper 未透传宿主 cwd/session，不是 bridge 不使用 ctx（§10 第 2 项） |
| `CronScheduler`（`peri-middlewares/src/cron/mod.rs`）、Cron 工具（`peri-middlewares/src/cron/tools.rs`）、`LspTool`（`peri-middlewares/src/lsp/tool.rs`）、格式化函数（`peri-middlewares/src/lsp/formatters.rs`） | 复用业务实现，不推出端到端完全等价：`McpToolBridge::invoke`（`peri-middlewares/src/mcp/tool_bridge.rs`）已使用 ctx 且外层 120s；`invoke_tool_call`（`peri-middlewares/src/mcp/builtin/web.rs`）仍另造工具上下文，时限与上下文边界见 §10 第 2/12 项 |

## 12. sub-plan 索引

> sub-plan 文件在主计划冻结后撰写；文件名按下表固定，不得在实施中自行改名。

| sub-plan | 覆盖 | 文件 |
| --- | --- | --- |
| **C**（cron 实例） | IF-P3-01/02/05/06；任务 C-01…C-04 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-c-cron-instance.md` |
| **L**（lsp 实例） | IF-P3-01/02/08/09；任务 L-01…L-03 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-l-lsp-instance.md` |
| **H**（宿主语义与装配） | IF-P3-03/04/07/10/12；任务 H-01…H-05、S-01…S-05 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-h-host-assembly.md` |
| **V**（验证） | §7 闸门、§8 矩阵；任务 V-01…V-06 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-v-verification.md` |

> 主计划是唯一裁决源；sub-plan 内的矛盾表述以本文件 §3（冻结接口）与 §5（覆盖登记）为准。
>
> 四份 sub-plan 已随 `c96b20fa` 提交；其中的 A30 / A32 / A33 落点以本文件 §0 为准，sub-plan 与本节冲突时以主计划为准。
