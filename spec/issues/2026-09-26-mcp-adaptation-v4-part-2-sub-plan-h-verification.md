# MCP adaptation v4-part-2 — sub-plan H：wave 1 验证与证据

> **主 plan 覆盖（优先于本文）**：主计划 [`2026-09-26-mcp-adaptation-v4-part-2-plan.md`](2026-09-26-mcp-adaptation-v4-part-2-plan.md) 的 §5 覆盖登记优先于本文任何表述。涉及本文的覆盖如下（**实施前必读**）：
>
> | 覆盖 | 内容 | 本文受影响处 |
> | --- | --- | --- |
> | **R7** | 跨层「首个 LLM 请求 tools」与 fatal 路径断言归 **V-02**（`peri-acp` host seam）；crate 内只断言可观察层 | 本文 V-01/V-02 的实现文件固定为 `peri-acp/src/host/mcp_v4_builtin_test.rs`（模块 `host::mcp_v4_builtin`，**不是** `_tests`）；命令按主 plan §6 为准 |
> | **R8** | 「真正调用已提升为 direct 的 builtin 工具 + 同时观察审批与 wire」用例归 **V-01**（crate 内，`with_direct` 是 `pub(crate)`，`tests/` 集成层无法升级 bridge） | **本文 §6.3 的两层设计被主 plan 收窄**：主证据改为 crate 内 `mcp::builtin_runtime`（用真实 builtin 实例），host seam 只做复证。§6.3 的「主证据用 stdio fixture」不再是主证据，改为**对照补充** |
> | **R16** | `transport_type` **必须**新增 `"builtin"` 三分类（IF-D11，owner E-03） | **本文 §5 V-04 / §6.4 的处置被推翻**：隔离用例**可以**用 `transport_type == "builtin"` 区分 builtin 与 stdio；`mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言仍**不得修改**，只复跑 |
> | **R19** | **任务编号仲裁**：本文使用 `V-01…V-06`，与主 plan §6 的 `V-01…V-05` 语义不同；执行以主 plan §6 为准 | 见下表 |
> | 主 plan §7 | Wave 顺序固定为 W0 → W1(W1: E-01/E-02) → W2(E-03/I-01/S-01) → W3(I-02/I-03/S-02) → W4(V-01/V-02/V-03) → W5(V-04/V-05) | 本文 §8 的任务表只保留**证据设计**权威；**批次归属以主 plan §7 为准** |
> | 主 plan §9 规则 4 | **禁用过宽过滤器**（`permission` 会命中 `permission::auto_classifier::tests`；`mcp::builtin` 会命中 `builtin_spike`；`-- session::factory` 恒 0 tests 判失败） | 本文所有命令已改为精确模块名：`permission::tests`、`subagent::tests`、`mcp::builtin_runtime`、`mcp::builtin_apply`、`mcp::builtin::web`、`mcp::builtin::artifact`、`tool_search::declaration`、`host::mcp_v4_builtin`、`host::mcp_v4_wire_fixture` |
>
> **任务映射（H → 主 plan §6）**：
>
> | 本文 task | 主 plan task | 说明 |
> | --- | --- | --- |
> | V-01（host 握手与 ready 三路径） | **V-02** | 落 `peri-acp/src/host/mcp_v4_builtin_test.rs`；model 替身与 broker 注入见 IF-H3 |
> | V-02（首个 LLM 请求三冻结名） | **V-02** | 同一文件；**并与 V-06 的迁移前基线逐项对照**（A12） |
> | V-00（前置核实 U3 等） | **主 plan V-06**（W0，**先于任何生产改动**）：建 host 侧 wire 夹具 + 录迁移前基线 | 夹具文件 = `peri-acp/src/host/mcp_v4_wire_fixture.rs`（**唯一 owner V-02**，A10） |
> | V-03（审批 + wire 的 BLOCKED 收口） | **V-01（crate 内主证据）+ V-02（host 复证，用 V-06 的夹具）** | 二者都必须存在；V-03b 的条件性保持（U3 依赖 → 见 IF-H3） |
> | V-04（两实例隔离） | **V-01**（断言落 `peri-middlewares/src/mcp/builtin_runtime_test.rs`，模块 `mcp::builtin_runtime`） | ⚠ 主 plan 的 V-03 是**隔离/宿主策略回归复跑**（既有断言不改，只允许夹具 env 改动）；隔离断言本身并入 V-01 的 crate 内测试文件，**按 A13 的可观察五项**重写 |
> | V-05（策略一致性验收行） | **S-01 的实现侧测试**，由 **V-01** 的行内命令复跑 | 无独立 task |
> | V-06（能力关闭） | **V-01**（四个面）+ **I-02**（配置语义） | 见 G §6.10 |
> | V-07（全量门禁） | **V-04**（验收记录前的复跑） | 命令不变 |
> | V-08（acceptance 记录） | **V-04** | 文件名一致；**结构须含「迁移前基线」小节与未验证项清单模板**（A12/A19） |
>
> **总纪律**：`0 tests` 一律视为失败（主 plan §9 规则 3）。本文件每一行都给出「承担测试的模块路径与模块名」，模块名必须与 `cargo test -- <filter>` 的过滤器**逐字符**对应（主 plan §9 规则 4）。夹具的「真实 vs 假 peer」边界逐条写明；不覆盖的内容逐条声明（§9）。

## 0. 裁决记录 A1–A19 落点（复核用索引）

| 裁决 | 本文件落点 |
| --- | --- |
| **A1** 注入点唯一（step 6.5） | 不涉及（归 I-02/E）；H 的断言只观察结果（首个 LLM 请求 tools / pool.configs） |
| **A2** `PERI_MCP_BUILTIN` 语义 | §7（acceptance 必须记录：off ⇒ 两实例工具均不在首个请求、能力面为零、**不是**回退到旧实现）；§8 任务表（V-02/V-03 的命令与断言行）；§9 未覆盖项 |
| **A3** 保留实例名 | **§5 V-01 夹具策略（重写）**：错误注入**不得**用「同名用户覆盖」；§8（V-01 依赖）；§10 H-R 新增 |
| **A4** 生效名归一原则 | §8（V-05 策略一致性复跑引用 S-01/S-02 的精确过滤器）；§9 |
| **A5** 直连性声明（IF-D13） | §5 V-02（direct 排他性断言）；§8；§12 |
| **A6** 三个工具面 | §5 V-06（关闭矩阵覆盖 workflow 面）；§9 |
| **A7** 两张名单 | §8（V-06 引用 S-02 的 `provider/config.rs` 断言）；§9 |
| **A8** TUI 在范围内 | §9（`--workspace --lib` 已含 `peri-tui`；TUI 断言归 S-08，H 只登记未覆盖声明） |
| **A9** 声明段不得丢失 | **§9「声明段消失」条已作废**：改为「声明仍产出（S-02 断言）」；§10 |
| **A10** V-03 夹具 | **§3 IF-H1（新增夹具文件行）+ IF-H3（U2/U3 结论）+ §5 V-03（重写）+ §6.3（两层设计）+ §8** |
| **A11** 隔离夹具 env | **§5 V-04 夹具段 + §8（V-03 行）+ §10 H-R5**：只改夹具 env（`PERI_MCP_BUILTIN=off`）+ 新增 off 断言，既有断言不改 |
| **A12** 基线证据 | **§5 V-06（W0，主 plan 新增 task）+ §7 §2 条 + §8**：迁移前 HEAD 录首个 LLM 请求工具名，与迁移后逐项对照 |
| **A13** 隔离断言 | **§5 V-04（重写）**：capability root / 凭据 ⇒ UNVERIFIED；替换为五项可观察断言；§7（acceptance 未验证项清单）；§9 |
| **A14** 任务号与过滤器 | §3 IF-H1（模块与过滤器表）；§8 全部命令；旧装配编号统一读作 `I-03`；`-- session::factory` 禁用（该文件无测试模块） |
| **A15** `call_tool` 映射（IF-D14） | §5 V-01/V-03（builtin 实例的失败形态断言引用 IF-D14）；§12 |
| **A16** duplex 容量 | **§5 V-01/V-03（大 payload 用例）+ §9**：`BUILTIN_DUPLEX_BUF` 只影响背压，不是帧上限 |
| **A17** `{"web": {}}` 语义 | §5 V-06（配置语义由 I-02 断言，H 复跑并记录） |
| **A18** 关闭片段形状 | §5 V-06（非法组合必须被加载期拒绝的反例由 I-02 承担，H 登记验收行）；§9 |
| **A19** 主计划唯一裁决源 | §7（acceptance 结构含「迁移前基线」小节 + 未验证项清单模板）；§8；§12 |

## 1. 元信息

- 日期：2026-09-26。状态：验证规划，**未实施**。本轮只写本文，不写生产代码、不改设计文档、不提交。
- 范围：wave 1（Web MCP / Artifact MCP）的可观察断言 + 承担测试 + 命令 + 判定标准 + 全量门禁。
- 判据：每一项验收要求都能落到「一条可复现命令 → 明确的 pass/fail 观察量」，并且证据强度与实际断言层次一致（不把 crate 内单测说成端到端）。
- 事实基线来自：`docs/design/mcp-adaptation-v4-part-1.md` 契约 1–7、v4-part-1 plan §8 的 honest grading、v4-part-1 acceptance §3/§4 的 UNVERIFIED / BLOCKED、`peri-middlewares/src/mcp/builtin_spike_test.rs` 的实测结论。
- 依赖：本波的能力实现归 sub-plan F（实例迁移）与 sub-plan G（策略与投影）；builtin runtime 归 sub-plan E。H 不实现能力，只出证据。

## 2. 事实基线（已核实）

### 2.1 可复用的既有夹具与测试形态

| 事实 | 位置 |
| --- | --- |
| `peri-acp` host seam 的 system MCP 夹具：真实 `.mcp.json` + 真实 `run_initialize` + 真实 `McpClientPool` + `HomeRedirect` + `CountingModel`（记录 `ModelRequest.tools`）+ 真实 `run_session_loop`；模块名 `host::mcp_v4_startup_tests`（11 用例） | `peri-acp/src/host/mcp_v4_startup_test.rs:105-347`、挂载 `peri-acp/src/host/mod.rs:52` |
| `CountingModel::first_request_tool_names()` 读**首个** `ModelRequest.tools` 的名字；`first_request_system_text()` 读首个请求的系统消息文本（deferred 摘要断言面） | `peri-acp/src/host/mcp_v4_startup_test.rs:162-181` |
| host 夹具的 server 是真实 **node 子进程**（`FIXTURE_SCRIPT`，模式 `hang` / 工具清单由 argv 给出），由 `system_server(mode, tools, required, timeout_ms)` / `ordinary_server(mode, tools)` 构造 | `peri-acp/src/host/mcp_v4_startup_test.rs:55-103`、`:317-337` |
| 失败断言形态：模型调用 0 次 + `ExecutionFailureKind::Internal` + `TurnEnded(Error)` + ACP 投影 `-32000`/`kind=internal`（`assert_fatal_without_reason`） | `peri-acp/src/host/mcp_v4_startup_test.rs:368-` |
| crate 内 seam 夹具：`arc<McpClientPool>` + `tokio::io::duplex(4096)` 上的**假 peer**（`spawn_fake_peer`）+ 真实 `serve_client_auto` + `try_commit_connection`；模块 `mcp::mcp_v4_seam`（6 用例） | `peri-middlewares/src/mcp/mcp_v4_seam_test.rs`（挂载 `peri-middlewares/src/mcp/mod.rs:61-63`） |
| builtin spike 夹具：`tokio::io::duplex(8 KiB)` 上的**真实** `rmcp::serve_server`，server 读半加 `MethodTap` 记录线路级 method 序列；模块 `mcp::builtin_spike_tests`（6 用例） | `peri-middlewares/src/mcp/builtin_spike_test.rs:95-192`、`:311-377`、挂载 `peri-middlewares/src/mcp/mod.rs:65-69` |
| 宿主策略契约夹具（外部集成测试）：真实 rmcp client service + duplex JSON-RPC fixture + 真实 `MiddlewareChain` + `dispatch_tools`；server 侧记录真实收到的 `tools/call`（**wire 事实，非 mock 计数**）；模块 = 测试二进制 `mcp_host_policy_contract`（5 用例） | `peri-middlewares/tests/mcp_host_policy_contract.rs:53-73`、`:486-`、`:857-` |
| 隔离契约夹具（外部集成测试）：两个真实 node 子进程，各自 wire 日志（`#recv <payload>`）、各自 pid；`requests` / `methods` / `tool_call_names` / `boot_pid` 四个观测原语；模块 = `mcp_isolation_contract`（3 用例） | `peri-middlewares/tests/mcp_isolation_contract.rs:163-240` |
| `McpTaskOwner::shutdown()` 返回可断言 `.is_complete()` 的收尾报告 | 同上 `:226-239`；`peri-middlewares/src/mcp/task_scope.rs:180`、`:272` |
| `McpClientHandle` 字段是 `pub`（外部测试可直接构造/读取 `peer`、`tools`、`status`、`url`） | `peri-middlewares/src/mcp/mcp_v4_seam_test.rs:198-211`（结构体字面量） |
| `McpClientHandle` **无** credential 字段；`McpClientPool::capability_profile` 与 `McpConnectionKey` 非 public | v4-part-1 acceptance §3（已记录为 UNVERIFIED） |
| `transport_type` 由 `h.url.is_some()` 二值推导 → builtin 实例（`url == None`）报告为 `"stdio"` | `peri-middlewares/src/mcp/client/status.rs:172`、`:203`、`:224`；隔离测试的断言在 `mcp_isolation_contract.rs:379-385` |
| 生产装配面入口：`build_middleware_chain(&ProductionChainAssembler, &ctx)`；测试用 `assemble_names` / `assemble_tool_names` 读链名与工具名 | `peri-middlewares/src/assembly_test.rs:470-524`、`peri-middlewares/src/assembly.rs:411-412`（`mod tests`） |
| 全量门禁的分工：`cargo test --workspace --lib`（part-1 收口复跑为 1803 passed / 10 ignored）；`cargo clippy --workspace --all-targets -- -D warnings`；`cargo fmt --all --check` | v4-part-1 acceptance §5.1 |
| `cargo test --workspace`（含 `peri-tui` 集成与 `e2e/`）在 part-1 中**未运行**（非门禁） | v4-part-1 acceptance §8 |

### 2.2 v4-part-1 遗留的验证缺口（本波必须收口的对象）

| 缺口 | 出处 | wave 1 的收口条件 |
| --- | --- | --- |
| 契约 5 的**凭据隔离**、**capability root 隔离**、**五实例落地** = UNVERIFIED | v4-part-1 acceptance §3 | wave 1 只把「Web / Artifact 两实例落地」从「未落地」变成「已落地」；**凭据与 capability root 的 per-instance 可观察性本波仍不改变**（`McpClientHandle` 无 credential 字段、`capability_profile` 非 public）→ 本波**不得**声称契约 5 完成 |
| 契约 6 的 **BLOCKED**：没有用例真正**调用**已提升为 direct 的 MCP 工具并同时观察审批与 wire | v4-part-1 acceptance §4 末行 | 本波必须给出**一条**这样的用例（V-03），或在无法实现时显式写回 BLOCKED 并说明被什么挡住（§6.3 的两层设计） |
| 契约 6 的 SubAgent / Workflow / Goal / PTC = UNVERIFIED | 同上 | wave 1 只补「subagent `parent_tools` 与 workflow agent 工具在迁出后能力等价」（V-06），其余保持 UNVERIFIED |

## 3. 接口冻结（验证层）

### IF-H1：测试模块布局与过滤器（冻结，供主 plan §4 的所有权矩阵直接引用）

| 模块路径（`cargo test` 过滤器） | 载体文件 | 挂载点 | 主 plan task / owner |
| --- | --- | --- | --- |
| `mcp::builtin_runtime` | `peri-middlewares/src/mcp/builtin_runtime_test.rs`（新） | `peri-middlewares/src/mcp/mod.rs`（由 I-02 统一挂载） | **V-01** |
| `mcp::builtin_apply` | `peri-middlewares/src/mcp/builtin_apply_test.rs`（新） | 同上 | **I-02** |
| `mcp::builtin::web` / `mcp::builtin::artifact` | `peri-middlewares/src/mcp/builtin/web.rs`、`artifact.rs`（新，**不是** `builtin/instances/**`） | `peri-middlewares/src/mcp/builtin/mod.rs`（I-01 追加子模块声明） | **I-01** |
| `host::mcp_v4_builtin` | `peri-acp/src/host/mcp_v4_builtin_test.rs`（新） | `peri-acp/src/host/mod.rs`（单 owner） | **V-02** |
| `host::mcp_v4_wire_fixture`（**A10 新增夹具**） | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（新：node 脚本含 **wire 日志** + `tools/call` 分支；**复刻**的工具调用 model 替身；自带 ≥1 条自检测试，保证过滤器不是 0 tests） | `peri-acp/src/host/mod.rs`（同 owner） | **V-06（W0 建）→ V-02（W4 用）** |
| `mcp::builtin_spike`（既有，**只复跑**） | `peri-middlewares/src/mcp/builtin_spike_test.rs` | 已挂载于 `mcp/mod.rs` 末（模块名 `builtin_spike_tests`） | **E-00** |

**硬约束（三条）**：
1. `peri-acp/src/host/mod.rs` 是**单 owner 文件**：本文所有 host 断言**全部**落 `mcp_v4_builtin_test.rs`，夹具落 `mcp_v4_wire_fixture.rs`，两者同属 **V-02/V-06**（同一 owner 序列：V-06 在 W0 建夹具与基线，V-02 在 W4 复用），**没有第二个 owner**。
2. 新增 `*_test.rs` / 夹具模块必须在实现模块内挂 `#[cfg(test)] #[path = "<name>.rs"] mod <module_name>;`；`mcp/mod.rs` 的两个新测试模块（`builtin_apply_tests`、`builtin_runtime_tests`）由 **I-02 一次挂载**（主 plan §4），V-01 不得自行添加。
3. **不得**复用 `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT` 作为 V-03 的 wire 夹具（A10：该脚本**无 wire 日志、无 `tools/call` 分支**，`:101` 的兜底会把 `tools/call` 回成 `-32601`，导致断言永远不成立却看似运行）。

### IF-H2：夹具边界（冻结）

| 层次 | 允许的夹具 | 禁止 |
| --- | --- | --- |
| builtin **协议/握手/ready**（V-01） | 真实 `rmcp::serve_server` + 真实 `serve_client_auto` + `tokio::io::duplex`（复用 spike 形态）；handler 用**真实** `WebMcpServer` / `ArtifactMcpServer`（主 plan **I-01**，落 `mcp/builtin/web.rs` / `artifact.rs`） | 不得用 `readiness_test.rs` 式手搓 JSON-RPC 假 peer 顶替握手证据；假 peer 只允许用于**错误注入**（超时/取消/畸形响应），且必须在测试名与文档注释里写明它是假 peer |
| **首个 LLM 请求 tools**（V-02） | 真实 `run_session_loop` + counting model（part-1 的 `McpStartupHarness` 形态）；builtin 走**真实配置注入路径**（builtin 默认层） | 不得直接构造 `direct_definitions` 或调用 `prepare_system_tools` 来「证明」注入；那只是同一函数的自证 |
| **审批 + wire**（V-03） | 真实 `MiddlewareChain` + `dispatch_tools` + 真实 broker（批准/拒绝）+ **server 侧真实收到请求**为 wire 事实 | 不得用「mock `McpClientHandle` 的 `call_tool` 计数」代替 wire；不得只断言「审批看到了名字」就称「调用被拦」 |
| **隔离**（V-04） | builtin 两实例用真实 handler + 真实 pool；stdio 对照用真实 node 子进程 | 不得用 `transport_type` 区分 builtin 与 stdio（§6.4） |
| **能力关闭**（V-06） | 真实装配面 `ProductionChainAssembler` + `disabled` 集合 | 不得只断言「middleware 不在链上」就称能力已关闭（ARC-CAPABILITY-CLOSURE-001） |

### IF-H3：三条「必须先核实」的前置事实（owner 开工前必须解决）

| # | 未知 | 影响 | 核实方法 | 未解决时的处置 |
| --- | --- | --- | --- | --- |
| **U1** | ~~`peri-acp` host 夹具能否**注入 broker**~~ → **已核实：可行**。`SessionContext` 的 `broker: Arc<dyn UserInteractionBroker>` 与 `permission_mode: Arc<SharedPermissionMode>` 都是 `pub` 字段（`peri-agent/src/session/exec/executor/context.rs:155-195`，broker 在 `:194`、permission_mode 在 `:195`），`make_session_context` 是 `pub(super)`（`peri-acp/src/host/executor_flow_test.rs:280`），同属 `host` 模块树的 V-02（`host::mcp_v4_builtin`）可直接改写/注入这两个字段 | 决定审批链证据能否落在 host seam | 已核实（见左）；owner 仍需在实现时确认 `PermissionMiddleware` 在该配置下确实装配（`assembly.rs` 的 `ChainSlot::Permission` 分支要求 broker + mode 均 `Some`） | 仅在实现时发现装配被跳过时才降级为「审批链证据仅在 crate 内」 |
| **U2** | host 夹具能否驱动**第二次 Reason 的工具调用** → **已核实：形态存在，但不可复用**。`host::executor_flow_tests::PtcScriptedModel`（`peri-acp/src/host/executor_flow_test.rs:2153`）返回 `ModelMessage::assistant(vec![], vec![tool_call])` + `StopReason::ToolUse`（`:2194-2202`），即「真实 dispatch 一次工具调用」的 model 形态已有实现 | 决定 V-03 能否落在 host seam | **A10 结论：`PtcScriptedModel` 是私有 struct（无 `pub`）⇒ 必须在 `mcp_v4_wire_fixture.rs` 内照其形态复刻一份**（不得 `use`，也不得修改 `executor_flow_test.rs`；IF-H1 的单 owner 约束） | 复刻后 V-03 可落在 host seam；复刻失败才退到 crate 内兜底 |
| **U3** | sub-plan E 是否提供 builtin 实例侧的 **wire 观测面**（例如把 builtin 的 `duplex` 读半或 server task 句柄以 `pub(crate)` 暴露给测试） | 决定 V-03b（wave 1 真实 builtin 实例的 wire）能否成立 | 读 E 的实现（`peri-middlewares/src/mcp/builtin/**`） | V-03b 记为 **UNVERIFIED**，理由写「builtin 实例无 per-instance wire observable」，不得用「server 侧 handler 计数器」冒充 wire（见 IF-H2 的精神：计数不是 wire） |

**注意**：U3 若无法解决，V-03 仍然**可以**收口 v4-part-1 的 BLOCKED 缺口——该缺口问的是「被提升为 direct 的工具走审批链时，审批与 wire 是否各自只发生应有的次数」，与「该实例是 builtin 还是 stdio」无关（提升路径由 `system_mcp_tools` 决定，与 transport 无关）。因此 V-03 的**主证据**用一个 **stdio system MCP fixture**（其 wire 天然可观测）承担，是**更强的证据**，而不是退而求其次。这一点必须在 acceptance 里写清楚，避免被读成「没测 builtin」。

### IF-H4：证据强度分级口径（沿用 part-1，不升格）

| 级别 | 含义 | 本波适用 |
| --- | --- | --- |
| **强** | 在宿主真实执行路径上断言可观察结果（首个 LLM 请求入参、真实 wire 请求、真实进程 pid） | V-02、V-03、V-04（部分）、V-05 |
| **中强** | 纯函数/装配函数的完整覆盖 | V-05、V-06（链与工具集合） |
| **PARTIAL** | 只覆盖已落地连接的局部，不能支撑契约全文 | V-04（凭据 / capability root） |
| **UNVERIFIED / BLOCKED** | 无运行时证据，必须显式写出理由 | V-03b（若无 wire 面）、V-06 的 TUI/ACP 端到端面 |

**禁止**：把 `cargo test --workspace --lib` 全绿写成「wave 1 验收通过」；它只证明无回归。

## 4. 验收要求 → 断言 → 测试 → 命令 → 判定 矩阵

> 每一行的「0 tests 判定」都必须是「否」；任何一行出现 `running 0 tests` / `test result: ok. 0 passed` 即该行失败。

| # | 验收要求 | 可观察断言 | 承担测试（模块路径与模块名） | 命令 | 0 tests 判定 |
| ---: | --- | --- | --- | --- | --- |
| V-01a | builtin 实例走**真实协议握手**后才 ready | 实例 `ClientStatus::Connected`；`handle.peer` 非空且 `peer_info()` 可读；线路 method 序列**首帧为 `server/discover`** 且**全程不含 `initialize`**（F-IF-F0 A） | `host::mcp_v4_builtin` → `builtin_instance_completes_real_handshake_before_ready` | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | 否 |
| V-01b | 握手/发现失败路径：不发布 ready、首个 prompt fatal、模型调用 0 次 | `ClientStatus::Failed(_)`；`ExecutionFailureKind::Internal`；`counting.call_count() == 0`；`TurnEnded(Error)` | 同上 → `builtin_instance_handshake_failure_is_fatal_without_model_call`（错误注入：让 builtin handler 的 `list_tools` 返回 `McpError`，走真实 `list_discovered_tools` 失败分支） | 同上 | 否 |
| V-01c | 超时路径：超时**不是** cancel | fatal 错误（非 `Interrupted`）；无 `LoopResult::Interrupted`；模型 0 次 | 同上 → `builtin_instance_timeout_is_fatal_not_cancelled`（错误注入：builtin handler 延迟响应，`system_mcp_timeout` 设为小值） | 同上 | 否 |
| V-01d | 取消路径：`Cancelled → Interrupted` | 启动期取消 → `LoopResult::Interrupted`（或 `TurnEnded` 的 interrupted 形态，按 part-1 的既有断言口径）且模型 0 次 | 同上 → `builtin_instance_cancellation_maps_to_interrupted` | 同上 | 否 |
| V-02 | **首个 LLM 请求**的工具列表含三个冻结 effective name 且为 direct | `counting.first_request_tool_names()` **包含** `mcp__web__WebSearch`、`mcp__web__WebFetch`、`mcp__artifact__artifact`；同 server 的**未**列入 `system_mcp_tools` 的工具**不**在首个请求中（用 Artifact 实例无法造，改用 stdio fixture 的对照实例，见 §6.2）；`first_request_system_text()` 的 deferred 摘要**不含**这三个名字 | `host::mcp_v4_builtin` → `builtin_instances_expose_frozen_effective_names_on_first_model_request` | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | 否 |
| V-03 | **BLOCKED 收口**：真正**调用**被提升为 direct 的工具，同时观察审批与 wire；approve 恰好 1 次 `tools/call`，reject 0 次 | approve：broker 收到 `(effective_name, input)` 且 wire 上 `tools/call` 计数 **== 1**、`params.name` 为**原始工具名**；reject：wire 上 `tools/call` 计数 **== 0** 且调用结果为 `UserRejected` | 主证据：`host::mcp_v4_builtin` → `promoted_direct_tool_approval_calls_wire_exactly_once` / `promoted_direct_tool_rejection_never_reaches_wire`（夹具 = `system_mcp: true` + `system_mcp_tools` 的 **stdio node fixture**，其 wire 日志是天然 observable） | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | 否 |
| V-03b | wave 1 的**真实 builtin 实例**上的同一断言 | 同 V-03，但实例为 builtin `web` | **条件成立时**：`mcp::builtin_runtime` → `builtin_instance_promoted_tool_approval_is_wire_observable`。**不成立时**（U3 未解决）：**UNVERIFIED**，在 acceptance 写明理由与 U3 的核实方法 | 条件成立时 `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | 条件成立时「否」；不成立时该行**不得**出现在命令表（避免空跑） |
| V-04a | 两实例的**独立 transport** | 关闭 `web` 不影响 `artifact`：关闭后 `artifact` 仍 `Connected` 且其工具仍可列出；反向亦然 | `mcp::builtin_runtime` → `closing_one_builtin_instance_does_not_affect_the_other` | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | 否 |
| V-04b | 两实例的**独立 handle / pool entry** | `pool.get_client("web")` 与 `get_client("artifact")` 均为 `Some` 且**不** `Arc::ptr_eq`；pool entry 数 ≥ 2；`all_server_infos()` 逐实例一条（**应当**断言 `transport_type == "builtin"`，见 §6.4 / R16） | 同上 → `builtin_instances_have_distinct_pool_entries_and_handles` | 同上 | 否 |
| V-04c | **A13 替换项**：重连只动一个实例 | 重连 `web` 后 `web` 的 generation **递增**，`artifact` 的 generation **不变** 且其句柄仍是同一 `Arc` | 同上 → `reconnecting_one_builtin_instance_does_not_touch_the_other` | 同上 | 否 |
| V-04d | **A13 替换项**：per-instance wire 不串（**条件性**） | 仅当 E 提供 per-instance wire 观测面时：发往 `web` 的请求只出现在 `web` 的 wire 上。**不成立时**记 `UNVERIFIED`，理由 = 「builtin 实例无 per-instance wire observable；计数器不算 wire」 | 同上 → `builtin_instances_do_not_share_wire`（条件性） | 条件不成立时**不产生命令**，只在 acceptance 记录 UNVERIFIED | N/A |
| V-04z | **不可证伪项（A13）**：capability root 隔离 + 凭据隔离 | **两行都不产生命令**：`capability_profile` 是 pool 级（`apps.rs:229-268`）、`bind_execution_cwd` 是 pool 级（`initialize.rs:161`）、`McpClientHandle` 无 credential 字段 ⇒ 记 **UNVERIFIED**，写进 acceptance 的未验证项清单（含「为什么不可证伪」与「后续如何证伪」） | 无 | N/A | **禁止**用「构造参数不同」这类代码阅读结论替代 |
| V-04e | 关闭互不影响（重复覆盖，矩阵形式） | 三组配置（都开 / 关 web / 关 artifact）下 `(web 可见, artifact 可见)` 分别为 `(T,T)` / `(F,T)` / `(T,F)`，且**不**存在 `(F,F)` 的连带 | 同上 → `builtin_instance_disable_matrix` | 同上 | 否 |
| V-05a | 策略一致性（等价性） | 对 `BUILTIN_MCP_INSTANCES` 逐项：`default_requires_approval(eff) == default_requires_approval(raw)`、`is_edit_tool(eff) == is_edit_tool(raw)`、`is_mutation_tool(eff) == is_mutation_tool(raw)` | `peri-middlewares` lib 测试：`permission::tests`（G-S-01 = 主 plan S-01）+ `subagent::tests`（同 S-01） | `cargo test -p peri-middlewares --lib -- permission::tests`；`cargo test -p peri-middlewares --lib -- subagent::tests` | 否 |
| V-05b | 策略一致性（未知 `mcp__*` 反证） | `mcp__filesystem__read_file` / `mcp__github__create_issue` / `mcp__unknown__anything` 的 `default_requires_approval == true` 且 `is_mutation_tool == true`；`mcp_` / `mcp` / `mcp_read_resource` == false；`original_tool_name_of_effective("mcp__web__fetch") == None` | 同上 | 同上 | 否 |
| V-05c | 三条冻结判定结果被钉住 | `mcp__web__WebSearch` / `mcp__web__WebFetch` → `requires_approval == true`；`mcp__artifact__artifact` → `== false`；三者 `is_edit_tool == false`、`is_mutation_tool == false` | 同上 | 同上 | 否 |
| V-06a | 能力关闭：禁用实例后工具从 **session-local 视图**消失 | `assemble_tool_names` 在 `disabled = {"web"}` 时不含 `mcp__web__WebSearch` / `mcp__web__WebFetch`；`disabled = {"artifact"}` 时不含 `mcp__artifact__artifact`；未禁用时三者都在 | `peri-middlewares` lib 测试：`assembly`（`assembly::tests`，主 plan I-03 拥有，本行只**复跑**并登记其名） | `cargo test -p peri-middlewares --lib -- assembly::tests` | 否 |
| V-06b | 能力关闭的**关闭面**（ARC-CAPABILITY-CLOSURE-001） | 禁用 `web` 后：① 顶层链工具集合、② `parent_tools`（subagent 继承）、③ workflow agent 工具集合，三者**同时**不含两个 Web effective name；反向同理 | `mcp::builtin_runtime` → `disabling_web_instance_removes_tools_from_all_three_paths` | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | 否 |
| V-06c | 遗留关闭键不静默失效 | `"WebMiddleware": false` → 实例 `Disabled` 且工具从视图消失；`"ArtifactMiddleware": false` 同理；两键互不影响 | `peri-middlewares` lib 测试：`mcp::builtin_apply`（主 plan I-02 拥有）+ `assembly::tests`（I-03）（`provider/config.rs` **无需改动**，R15） | `cargo test -p peri-middlewares --lib -- mcp::builtin_apply`；`cargo test -p peri-middlewares --lib -- assembly::tests` | 否 |
| V-06d | `MIDDLEWARE_TOOL_NAMES` 不再误剔同名工具（IF-F5 反证） | 共享表里名为 `WebFetch` 的人工条目在 disabled 链下**仍被保留** | `peri-agent` lib 测试：`stage_builder`（主 plan S-02 拥有） | `cargo test -p peri-agent --lib -- session::exec::stage_builder` | 否 |
| V-07a | 全量 lib 回归 | 无失败、无 ignore 增量、**无 `0 tests`** | 全 workspace | `cargo test --workspace --lib` | 否 |
| V-07b | lint 门禁 | exit 0、无 warning | 全 workspace | `cargo clippy --workspace --all-targets -- -D warnings` | N/A（非测试命令） |
| V-07c | 格式门禁 | exit 0 | 全仓库 | `cargo fmt --all --check` | N/A |

### 4.1 判定与记录规则

- 每条命令必须记录：`exit code`、`test result:` 行原文（passed / failed / ignored）、以及**是否命中预期模块**。`-- --test-threads=1` 用于所有涉及 `HOME` 重定向或真实子进程的用例。
- 命令返回 `0 tests` → 该行**失败**，并且必须修**挂载**而不是放宽过滤器。
- 过滤器命中数**多于**预期不是失败，但必须核对是否误命中邻近模块（part-1 plan §9 规则 4 的教训：`host::prompt` 会命中 `prompt_dispatch`）。
- 新增测试文件的挂载复核（part-1 plan §9 规则 2）：`git status` 的每个新 `*_test.rs` 都必须能在对应生产文件里找到 `#[path]` 挂载；两个 `tests/*.rs` 由 cargo 自动发现，**不**需要挂载，但必须确认它们出现在 `cargo test -p peri-middlewares --test <name>` 的输出里而非「0 tests」。

## 5. 每个任务的详细规格

### V-01：builtin 实例的真实握手与 ready 三路径

**文件**：`peri-acp/src/host/mcp_v4_builtin_test.rs`（新）、`peri-acp/src/host/mod.rs`（挂模块，单 owner）。
**模块名**：`host::mcp_v4_builtin`。
**夹具策略**：复用 `mcp_v4_startup_test.rs` 的 `HomeRedirect` / `CountingModel` / `McpStartupHarness` 形态（同 crate，可 `use super::mcp_v4_startup_tests::...`；若该模块的条目未 `pub(crate)`，在**本 task 内**把它们提升为 `pub(crate)` 并在报告里写明——这属于测试模块间共享，不改生产代码）。server 侧：`web` / `artifact` 用**真实 builtin 实例**（走 builtin 默认层，不写 `.mcp.json`）。
> **A3 修正（必须遵守）**：错误注入项**不得**使用「与保留实例同名的用户覆盖条目」——`web` / `artifact`（以及预留的 `cron` / `lsp` / `workspace`）是**保留实例名**，用户配置为它们声明 `command`/`url` 会在加载期报 typed error（A3）。错误注入改用**不同名**的用户 stdio server（例如 `.mcp.json` 里的 `fail_fixture` / `hang_fixture`，同样标 `system_mcp: true` + `system_mcp_tools`），从而仍得到「一个可控对端 + 仍是系统 MCP」的组合；若要注入 builtin 实例自身的失败，只能由 I-01 提供 `#[cfg(test)]` 构造器。

**四条例外的具体实现路径（owner 必须在报告里写明实际用了哪条）**：
- V-01b（失败）：首选让 builtin handler 的 `list_tools` 返回 `McpError`（若 F 的 handler 提供 `#[cfg(test)]` 构造器）。若 F 不提供，则改用**异名**用户配置条目 + node fixture 返回畸形 `tools/list`（与 part-1 的 `system_mcp_tool_discovery_failure_is_not_an_empty_tool_list` 同法）。**两条路径都必须落在真实 `list_discovered_tools` 失败分支**。
- V-01c（超时）：**异名**用户配置条目指向 **hang 模式**的 node fixture，`system_mcp_timeout` 设 200–500 ms（与 part-1 的 `system_mcp_timeout_is_fatal_not_cancelled` 同法）。
- V-01d（取消）：启动期取消。参照 part-1 既有用例的取消注入点（若 part-1 无该用例，则本 task 必须先核实 `run_session_loop` 的取消入口，并在报告里写出实际注入位置；**不得**凭猜测写用例）。

**不覆盖**：跨进程/跨机器的隔离；builtin 实例的凭据（无凭据）；Windows（沿用 `#[cfg(not(windows))]` 口径，仅 macOS 本机验证）。

### V-02：首个 LLM 请求的三个冻结 effective name

**文件**：同 V-01（可放在同一模块或同文件的新 `#[tokio::test]`）。
**断言三点**（缺一不可）：
1. 首个请求 `tools` 含三个冻结名（逐字比较；`McpToolBridge::description()` 带 `[MCP:{server}] ` 前缀，**不**影响名字）。
2. **direct 的排他性**：需要一个「同 server 有工具但未被 `system_mcp_tools` 列入」的对照。用 stdio fixture 的 system MCP（`system_mcp_tools: ["echo"]`，`tools/list` 返回 `echo` 与 `glob` 两个）断言 `glob` 不在首个请求、而在系统消息的 deferred 摘要里——这与 part-1 的 `system_mcp_ready_exposes_required_tools_on_first_model_request` 同法，属**复用既有证据形态**。
3. **deferred 摘要不含**三个冻结名（`first_request_system_text()` 断言），证明它们没有被同时算成 deferred。

**不覆盖**：模型是否真的会调用这些工具（V-03 覆盖调用，不覆盖「模型自发选择」）。

### V-03：BLOCKED 缺口的收口（本波最重要的一行为）

**主证据（必须实现）**：`host::mcp_v4_builtin`。

夹具（全部真实；**A10 重写**）：
- **夹具文件（唯一 owner V-02）**：`peri-acp/src/host/mcp_v4_wire_fixture.rs`（W0 由 **V-06** 建，W4 由 V-02 复用）。其 node 脚本必须**同时**具备：① 每个收到的 JSON-RPC 行落到**自己的** wire 日志（`#recv <payload>`，复用 `mcp_isolation_contract.rs:57-110` 的 `FIXTURE_SERVER_JS` 形态）② 显式的 **`tools/call` 分支**（回带身份的固定结果）。**不得**复用 `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT`（无 wire 日志、无 `tools/call`，`:101` 兜底回 `-32601`）。
- `.mcp.json` 声明上述 **stdio node fixture**（**异名**，例如 `wire_fixture`；A3 禁止占用保留名），`system_mcp: true`、`system_mcp_tools: ["echo"]`、`system_mcp_timeout` 给足。
- broker：可控的 `UserInteractionBroker` 替身，记录收到的 `InteractionContext`（含 tool name 与 input），并返回 `ApprovalDecision::Approve` / `Reject`。
- model：一个能**返回工具调用**的 model 替身：**在夹具文件内复刻** `PtcScriptedModel` 的形态（`executor_flow_test.rs:2153`、`:2194-2202`）——该 struct 是**私有**的，**不得** `use`、也**不得**修改 `executor_flow_test.rs` 或 `mcp_v4_startup_test.rs` 的 `CountingModel`（IF-H1 单 owner）。

断言（approve）：
1. broker 恰好收到 **1** 次审批请求，其工具名为 `mcp__{fixture_server}__echo`（effective name），input 为模型给出的参数（**不是** wire 上的形态）。
2. fixture 的 wire 日志中 `tools/call` 请求**恰好 1 条**，且 `params.name` 为**裸名** `echo`（wire 不出现 effective name）。
3. 工具返回值进入 transcript（result 载荷可辨认）。

断言（reject）：
1. broker 收到 1 次审批请求。
2. wire 日志中 `tools/call` 请求**恰好 0 条**（含握手期的 `tools/list` 与 `initialize` 等其它 method 允许存在，断言只针对 `tools/call`）。
3. 工具结果表达用户拒绝（沿用 D-04 的 `EffectiveToolErrorCode::UserRejected` 口径），且**不**重试。

**这条为什么能收口 part-1 的 BLOCKED**：part-1 的阻塞原因是 `with_direct` / `prepare_system_tools` 是 `pub(crate)`，外部集成测试层无法把工具提升为 direct。此处**提升走真实配置路径**（`system_mcp_tools` → 启动闸门 → 真实 `prepare_system_tools`），不触碰任何 `pub(crate)`，因此端到端可观察。

**兜底路径（仅在实现期发现 U1/U2 的实际装配被跳过、无法在 host seam 生效时启用）**：crate 内 `mcp::builtin_runtime`（`pub(crate)` 可达），用真实 `McpMiddleware` + 真实 duplex fixture 复现同样两条断言。选择兜底时，acceptance 必须写「审批链证据在 `peri-middlewares` crate 内层」并给出 U1/U2 的具体阻塞点（**不得**只写「host seam 不方便」）。

**V-03b（wave 1 真实 builtin 实例上的同一断言）**：仅当 U3（E 是否提供 builtin 侧 wire 观测面）成立时执行。**不成立时的正确处置**：记为 **UNVERIFIED**，理由 = 「builtin 实例的 duplex 未以可观察形式暴露给测试；用 server 侧 handler 计数器会退化为 mock 计数，不满足 IF-H2 的 wire 纪律」。**禁止**为了填满矩阵而用计数器冒充。

**不覆盖**：多轮工具调用；并行工具批次；PTC 内部调用路径。

### V-04：两实例隔离

**文件**：`peri-middlewares/src/mcp/builtin_runtime_test.rs`（新，**crate 内**；主 plan R8/§4 已把 BLOCKED 收口与隔离断言都归到该文件，因为 `with_direct` / `prepare_system_tools` 是 `pub(crate)`，`tests/` 集成层触达不到）。
**夹具**：builtin 默认层（不写 `.mcp.json`）+ `HomeRedirect` 风格的 `EnvIsolation`（复用 `mcp_isolation_contract.rs:140-160` 的形态，**不**建立 workspace 级共享 helper，与既有文件的自包含约定一致）+ `McpTaskOwner` 收尾断言 `.is_complete()`。
> **A11**：凡走**生产 `run_initialize`** 的既有夹具（`tests/mcp_isolation_contract.rs` 的 `EnvIsolation`、`peri-acp/src/host/mcp_v4_startup_test.rs` 的 `HomeRedirect`）必须在该守卫内设 **`PERI_MCP_BUILTIN=off`**（并各**新增**一条「off 时无 builtin 实例」断言），使注入两个 builtin 后既有精确集合断言仍绿。**既有断言一字不改**；这条夹具改动已在主 plan §5 R28 登记为允许改动。

**五组断言（A13 重写；capability root 与凭据不再作为可证伪项）**：
- **V-04a（关闭互不影响）**：关闭（策略键 `"WebMiddleware": false`）后，`artifact` 仍能完成一次**真实 `tools/call`**（端到端，非只断言配置）。
- **V-04b（独立 handle / 连接键）**：两实例的 `Arc<McpClientHandle>` **非同一**（`!Arc::ptr_eq`）且 `McpConnectionKey` 不同；`transport_type` 均为 `"builtin"`。
- **V-04c（重连只动一个实例）**：重连 `web` 后 `web` 的 generation **递增**，而 `artifact` 的 generation **不变**（且其句柄仍是同一 `Arc`）。
- **V-04d（wire 不串；条件性）**：仅当 E 提供 per-instance wire 观测面（`#[cfg(test)]` 暴露 duplex 读半或 method 序列）时执行——断言发往 `web` 的请求只出现在 `web` 的 wire 上。**不成立时记 UNVERIFIED**，理由 = 「builtin 实例的 duplex 未以可观察形式暴露；用 server 侧计数器会退化为 mock 计数，不满足 IF-H2 的 wire 纪律」。
- **V-04e（三组配置的可见性矩阵）**：`WebMiddleware=false` / `ArtifactMiddleware=false` / `两者都 false` 下，两实例的工具在首个请求与 deferred 摘要中的出现情况。

**必须降级的两项（A13，不得写成同义反复）**：**capability root 隔离**与**凭据隔离**在 builtin 形态下**不可证伪**——`capability_profile` 是 pool 级字段（`mcp/apps.rs:229-268`，构造点 `:218`）、`bind_execution_cwd` 也是 pool 级（`initialize.rs:161`）、`McpClientHandle` 无 credential 字段。因此：① 两者一律记 **UNVERIFIED** 并写进 acceptance 的「未验证项清单」；② **禁止**用「两个实例构造参数不同」「两个 handler 是不同 struct」这类**代码阅读结论**替代运行时证据；③ 若需要 per-instance capability 的可观察量，只能作为**后续批次**的接口需求（E 侧新增 `#[cfg(test)]` 访问器）单列，不在本波谎称已测。

**`transport_type` 的三分类处置（⚠ 已被主 plan R16 改写，以此为准）**：
- 现象（迁移前）：`client/status.rs:172/203/224` 由 `url.is_some()` 二值推导 → builtin 实例会报告 `"stdio"`；既有 `mcp_isolation_contract.rs:379-385` 断言 `"stdio"`。
- **处置 = 按主 plan IF-D11 新增第三分类**：`transport_type_of(source, url)` 单一 helper，`Builtin → "builtin"`，其余分支逐位不变；owner = **E-03**。
- 因此 V-04b **必须**断言 builtin 实例在 `all_server_infos()` 与 `snapshot()` 中 `transport_type == "builtin"`（测试名冻结为 `builtin_instances_report_builtin_transport_type`），并在 `mcp::builtin_apply` 断言 config-only 行（`status.rs:224`）用 `sc.source` 得到同一结果。
- 既有 `mcp_isolation_contract.rs` 的 `"stdio"` 断言**保持原样**（其夹具是 stdio 实例，语义未变且属「只复跑、不改断言」范围）。**A11 修正**：主 plan §5 R28 已在 W4 的 V-03 名下**登记**该文件的**唯一允许改动**——`EnvIsolation`（`:140-160`）增加 `PERI_MCP_BUILTIN=off` 与其 Drop 恢复 + **新增**一条「off 时 pool 恰有两台 server」断言（既有断言一字不改）；`peri-acp/src/host/mcp_v4_startup_test.rs` 的 `HomeRedirect` 同法处理。

**不覆盖**：**凭据隔离与 capability root 隔离（A13：一律 UNVERIFIED，理由 = pool 级字段不可证伪）**；跨进程隔离；builtin 实例与 stdio 实例之间的隔离（本波两实例都是 builtin，若需要该对照，用 V-03 的 stdio fixture 作参照物，断言两者 wire 互不出现对方的标识）。

### V-05：策略一致性

**文件**：无新文件——**验证由实现侧的测试承担**（主 plan S-01 的 `permission/mod_test.rs` 与 `subagent/mod_test.rs`）。H 的职责是：
1. 把 §4 的 V-05a/b/c 三条作为**验收行**登记，指定命令与判定；
2. 在 acceptance 中**引用具体测试函数名**（缺名不接受）。
**不覆盖**：PermissionMode（AcceptEdit / AutoMode / Bypass）与三条判定组合的穷举；hook policy 对 effective name 的匹配（属 G-S-10 的文档面，无断言）。

### V-06：能力关闭

**文件**：`peri-middlewares/src/mcp/builtin_runtime_test.rs`（新，**crate 内**）。
- **V-06b 的三条路径怎么读（A6 后为四个面）**：
- 顶层链工具：`build_middleware_chain(&ProductionChainAssembler, &ctx)` → `chain.collect_tools(cwd)`。
- `parent_tools`：`assembly/preparation.rs` 的 `build_parent_tools`（若为 `pub(super)` 不可达，则**从装配面间接观察**：`ChainAssembly` 的 `subagent_mw` 槽位为 `Some` 时其内部 `parent_tools` 是否可读）。**A6 后**：该面必须断言 builtin 工具为 **direct**（`is_direct()`），因为子 agent 链无 ToolSearch；若 `parent_tools` 不可读则该路径降级为 **UNVERIFIED**，理由写「`parent_tools` 无外部可观察面」，并**不得**用代码阅读替代。
- workflow agent 工具：`assembly/workflow.rs` 的 `WorkflowAgentMiddlewareFactory` / `default_workflow_middleware_factory`（`peri-middlewares/src/assembly.rs:22` 的 `pub use`）产出的工具集合。**A6 后**：该面必须**含** `mcp__web__*`（未关闭时），并随 `WebMiddleware` 策略键关闭而消失（旧「workflow 天然不含 MCP bridge」的写法作废）。
- deferred bridge 收集：`mcp/middleware.rs` 的 `static_tool_bridges`（A6 面①）。

**若三条路径中有任一条无外部可观察面**，该路径的断言上移到 crate 内（`peri-middlewares` lib 测试，归主 plan I-03 的 `assembly::tests`）并在 H 的矩阵里标注实际层次。**不得**三条里只覆盖一条就称能力关闭完整。

**V-06d 的反证**：属主 plan **S-02**（`stage_builder/tools_test.rs` + `builder_v2_test.rs`），H 只登记命令。

**不覆盖**：TUI completion 与 ACP updates 两个关闭面（`TuiToolPresentation::Generic` / `ToolKind`）——TUI 侧的归一断言归 **S-08**（A8：`peri-tui` 在本波范围内，命令见 G §7）；`cargo test --workspace --lib` 已含 `peri-tui` 的 lib 测试，但 `peri-tui` 的集成/e2e 仍**未运行**，故端到端渲染一栏在 acceptance 里记为 **PARTIAL/UNVERIFIED**（口径与 G §9 一致）。
**A18 反例登记**：`{"web": {"disabled": true, "system_mcp": true}}` 必须被**加载期拒绝**（不得落到 `readiness.rs` 的 fatal），反例用例归 **I-02**（`cargo test -p peri-middlewares --lib -- mcp::builtin_apply`）；H 只在矩阵中登记该验收行与命令。
**性能回归**：本波新增两个 in-process 实例，启动路径多出两次握手；本文件**不**做基准测试（不覆盖），只在 acceptance 记录 `run_initialize` 相关用例的耗时变化（若有）。

### V-07：全量回归与门禁

| 门禁 | 命令 | 通过标准 | 备注 |
| --- | --- | --- | --- |
| lib 全量 | `cargo test --workspace --lib` | exit 0；**无** `0 passed` 的测试目标；failed == 0 | part-1 基线 1803 passed / 10 ignored（本波后总数应增加，ignore 不应增加） |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | 含测试目标 |
| 格式 | `cargo fmt --all --check` | exit 0 | 实施前先跑 `cargo fmt --all`，再 `--check` |
| 挂载复核 | `git status --porcelain` + 人工核对每个新 `*_test.rs` 的挂载 | 无孤儿测试文件 | part-1 plan §9 规则 2 |
| 越界复核 | `git status --porcelain` 与主 plan §4 所有权矩阵比对 | 无矩阵外写入 | part-1 plan §9 规则 1；需显式列出「矩阵外但被改动」的条目并说明原因 |

**明确未运行**（沿用 part-1 口径，写进 acceptance）：
- `cargo test --workspace`（含 `peri-tui` 集成测试）；
- `e2e/` 场景；
- `side-projects/local-mcp-server`（独立 workspace）；
- Windows 平台。

**本波新增到门禁的**：`cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` 与 两条既有外部集成测试命令（`--test mcp_isolation_contract` / `--test mcp_host_policy_contract`，均加 `-- --test-threads=1`）必须纳入 W-final 的复跑清单（外部集成测试二进制**不**在 `--lib` 门禁内，漏跑即假绿）。

## 6. 夹具策略与边界

### 6.1 真实 rmcp 对端 vs 假 peer 的分工（冻结）

| 用途 | 用真实 `serve_server` + duplex（spike 形态） | 用手搓 JSON-RPC 假 peer |
| --- | --- | --- |
| 握手成功 + live `tools/list` + ready | ✔ 必须 | ✘ |
| 首个 LLM 请求的 direct 注入 | ✔（host seam 走真实配置） | 允许（假 peer 只负责让连接可用，注入断言的是 model 入参） |
| 工具调用 + 审批 + wire | ✔（对端记录真实收到的请求） | ✘（假 peer 的「计数」不构成 wire 证据） |
| 错误注入（畸形响应 / 延迟 / 断连） | 允许（真实 `serve_server` + 延迟/错误返回） | ✔ 允许，但测试名必须含 `injected` 或文档注释写明「假 peer」 |

**结论一句话**：凡是断言「协议真的发生了」的行，必须用真实对端；假 peer 只用于断言「协议失败时的宿主反应」。

### 6.2 单进程内「wire」的定义

builtin 实例没有独立进程，其 wire 是 in-process duplex 上的 JSON-RPC。**可接受的 wire 证据**只有两种：
1. 对端读半上的线路级 method 序列（`MethodTap` 形态，spike 已给出实现形态）；或
2. server handler **确实是真实 `rmcp::serve_server` 的 handler**，且其 `call_tool` 被调用——但**只有在 handler 由 E 提供、且测试能证明请求确实经 transport 到达**时才构成证据；单纯的 `AtomicUsize` 计数在 handler 被直接函数调用时也会增加，因此**单靠计数器不构成 wire 证据**（这正是 IF-H2 禁止它的理由）。

### 6.3 BLOCKED 缺口的两层设计（明确表态，已按 A10 重写）

- 第 1 层（**必须实现，强证据**）：`system_mcp_tools` 驱动的 direct 提升 + 真实审批 + 真实 wire，实例用 **V-06 的 stdio node fixture**（`mcp_v4_wire_fixture.rs`，**含 wire 日志与 `tools/call` 分支**）。该层足以收口「被提升为 direct 的工具走审批链」这一**机制性**缺口。
- 第 2 层（**条件性**）：同一断言落在 wave 1 的真实 builtin `web` 实例上（V-03b / V-04d）。取决于 U3（E 是否提供 per-instance wire 观测面）。不成立时记 UNVERIFIED。
- **措辞纪律**：acceptance 中该缺口的收口结论必须写成「机制层已收口（fixture 实例）；wave 1 真实 builtin 实例层的 wire 等价性 = UNVERIFIED / 已收口（二选一，按 U3 结论）」，**不得**写成「BLOCKED 已完全收口」。
- **禁止**：用 `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT` 当 wire 夹具（无 wire 日志、`tools/call` 落 `-32601`）；用 server 侧 handler 计数器冒充 wire。

### 6.4 `transport_type` 的处置（⚠ 已被主 plan R16 / IF-D11 推翻，以主 plan 为准）

- **主 plan 结论**：`transport_type` **必须**新增第三分类 `"builtin"`（单一 helper `transport_type_of(source, url)`，三处调用点共用），owner = **E-03**。
- **因此**：本文原先「不改推导、不用它区分 builtin 与 stdio」的处置**作废**。隔离与面板断言**应当**断言 builtin 实例的 `transport_type == "builtin"`：
  `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` 中新增 `builtin_instances_report_builtin_transport_type`，并在 `mcp::builtin_apply` 中断言 config-only 行的同源推导。
- **仍然成立**：`peri-middlewares/tests/mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言**不得修改**（其夹具是 stdio 实例），**只复跑**（主 plan IF-D11）。该文件的**允许改动**仅限 `EnvIsolation`（`:140-160`）增加 `PERI_MCP_BUILTIN=off` 与其 Drop 恢复 + **新增**一条 off 断言（A11 / 主 plan §5 R28）。

## 7. acceptance 记录文件的更新规则（明确建议）

**建议：新建** `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`，**不**追加到 `2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`。

理由（逐条，可核对）：

1. **范围不同**：part-1 记录的第 9 行自述「范围：只记录契约 1–4、7 的落地与契约 5、6 的分级证据；不实现生产代码，不回填设计文档」。wave 1 的对象（Web / Artifact 实例迁移、`ChainSlot` 删除、策略改写、投影同步）**不在**该范围内，追加会让该自述失效。
2. **事实源不同**：part-1 记录的事实源是 `docs/design/mcp-adaptation-v4-part-1.md` + part-1 plan。wave 1 的事实源需要加上 part-2 主计划与本三个子计划；混在一个文件里会让「哪份 plan 冻结了哪条接口」不可追溯。
3. **part-1 记录的 UNVERIFIED 项需要被「闭合」而不是被覆盖**：`§3 五实例落地 = 未落地`、`§4 被提升 direct 工具链路 = BLOCKED` 两项，在 wave 1 之后变成「部分闭合」。正确做法是新记录里以**引用**形式写「part-1 §3 第 3 项 / §4 末行，本波闭合程度 = X」，并在 part-1 记录的这两行加上一行**指回**新记录的超链接（这是**唯一**允许对 part-1 记录做的改动：加指针，不改结论、不改证据、不改裁决）。
4. **三态列的可核性**：「本次运行时证据」列在 part-1 里对应 2026-09-25/26 的一次执行；wave 1 是另一次执行窗口，混排会让「本次」失去指代。
5. **沿用的格式不变**：新记录必须完整沿用 part-1 记录 §1 的口径（三态列 + 裁决规则：0 tests 视为失败、绿色局部单测不升级为整体结论），并额外增加：①「迁移前基线」小节（A12，见 §7 第 3 项）；②「关闭面矩阵（F §6.3 的 **11 面**）」逐面 PASS/PARTIAL/UNVERIFIED；③「未被本波覆盖的 part-1 UNVERIFIED 项」清单（含 A13 的 capability root / 凭据与 A2 的 off 语义，模板见 §7 第 8 项）。

**新记录必须包含的最小结构**（模板，供 V-04 直接执行）：
1. 状态行 + 日期 + 最后核查 + 事实源（part-2 主计划 + 本三个子计划）；
2. §1 口径（沿用 part-1）；
3. §2 **迁移前基线小节（A12，由 V-06 在 W0 写入，V-04 不得改写）**：迁移前 HEAD 上首个 LLM 请求的工具名集合（裸名 `WebSearch`/`WebFetch`/`artifact`，三者迁移前均 direct）与采集命令、夹具文件名、HEAD 提交号；与「迁移后三个 effective name」逐项对照；
4. §3 本波交付判据矩阵（F §1 的四条 + G §1 的三条 + H §4 的 V-01…V-07）；
5. §4 关闭面矩阵（`ARC-CAPABILITY-CLOSURE-001`，**11 面**，逐面给承担测试名）；
6. §5 对 part-1 遗留项的闭合程度（凭据隔离 / capability root / 五实例落地 / 被提升 direct 工具链路 / SubAgent·Workflow·Goal·PTC）；
7. §6 运行命令与现场结果（**每条**含 exit code、`test result:` 原文、0 tests 判定）；
8. §7 **未验证项清单（模板，逐条必须写「为什么不可证伪」与「后续如何证伪」）**，至少含以下四项：
   - `UNVERIFIED`：capability root 隔离（A13；`capability_profile` 是 pool 级 `apps.rs:229-268`，无 per-instance public observable）；
   - `UNVERIFIED`：凭据隔离（A13；`McpClientHandle` 无 credential 字段）；
   - `UNVERIFIED`：builtin 实例自身的 wire 不串（A13/V-04d 条件不成立时）；
   - **已声明语义（非缺陷，但必须写明）**：`PERI_MCP_BUILTIN=off` 时 Web/Artifact 能力**不存在**（A2），middleware 提供面已删，**不存在**「回退到旧实现」这条路径；该 env 只影响实例注入，**不**改变策略判定面（G §12）。
9. §8 非目标 / 未运行的命令；
10. §9 文件所有权与 diff 越界复核（含「矩阵外但被改动」；**必须列出按 §5 R28 允许的夹具 env 改动**：`tests/mcp_isolation_contract.rs`、`peri-acp/src/host/mcp_v4_startup_test.rs` 的守卫 + 各自新增的 off 断言）；
11. §10 整体裁决（逐面 PASS / PARTIAL / UNVERIFIED，**不得**给单一总体「通过」）。

**不接受的做法**：在 part-1 记录里改结论、删证据、把 `PARTIAL`/`BLOCKED` 升级为 `PASS`；或新建记录时省略「本次运行时证据」列。

## 8. 任务表

依赖用 `→` 表示。**所有命令在仓库根目录运行，仅供后续实施，本轮未执行。**

| 批次 | Task | 标题 | owner 产出文件 | 依赖 | 验证命令 |
| --- | --- | --- | --- | --- | --- |
| W0（主 plan） | **V-06** → **主 plan V-06**（**A10/A12**） | 建 host 侧 wire 夹具 + 录**迁移前基线** | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（新，含 wire 日志 + `tools/call` 分支 + 复刻的 model 替身 + ≥1 条自检测试）；`spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（**新建**，「迁移前基线」小节） | — | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture`（≥1 passed，非 0 tests）；基线以命令输出与计数写入 acceptance |
| W1 | **V-00**（本文的前置核实） | 前置核实（U3：E 的 builtin wire 观测面）+ V-03/V-03b 的路径判定 | 无写入（只读复核，产出判定结论） | V-06 → | 读 `peri-middlewares/src/mcp/builtin/**`（E 的实现）与 `PtcScriptedModel` 的实际可见性（**已核实为私有 ⇒ 复刻**，A10）；产出「U3 成立与否 + V-03 走主证据还是兜底 + V-03b 是否进命令表」三行结论 |
| W5（主 plan） | **V-07** → 主 plan **V-04** | 全量回归与门禁复跑 | 无写入 | F、G、E 全部完成 → | `cargo test --workspace --lib`；`cargo clippy --workspace --all-targets -- -D warnings`；`cargo fmt --all --check`；两条 `--test` 命令 |
| W4（主 plan） | **V-01** → 主 plan **V-02** | builtin 真实握手与 ready 三路径（host seam） | `peri-acp/src/host/mcp_v4_builtin_test.rs`（新）、`peri-acp/src/host/mod.rs` | 主 plan I-01、E-03 →；**V-00 →** | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` |
| W4（主 plan） | **V-02** → 主 plan **V-02** | 首个 LLM 请求的三个冻结名 + deferred 排他性 | 同 V-01（同文件内新用例） | V-01（同一 agent 串行） | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` |
| W-final | **V-03** → 主 plan **V-01（crate 内主证据）+ V-02（host 复证）** | BLOCKED 收口：提升 + 审批 + wire | crate 内：`peri-middlewares/src/mcp/builtin_runtime_test.rs`（主证据，**落 V-01 的文件**）；host 复证：`peri-acp/src/host/mcp_v4_builtin_test.rs`（**落 V-02 的文件**） | V-00、V-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime`；`cargo test -p peri-acp --lib -- host::mcp_v4_builtin` |
| W4（主 plan） | **V-04** → 主 plan **V-01** | 两实例隔离契约（crate 内） | `peri-middlewares/src/mcp/builtin_runtime_test.rs`（新，**crate 内**） | 主 plan I-01、E-03 → | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` |
| W4（主 plan） | **V-05** → 主 plan **V-01/S-01** | 策略一致性验收行（引用实现侧测试） | 无写入（登记 + 复跑） | 主 plan S-01 → | `cargo test -p peri-middlewares --lib -- permission::tests`；`cargo test -p peri-middlewares --lib -- subagent::tests` |
| W4（主 plan） | **V-06** → 主 plan **V-01 + I-02** | 能力关闭三路径 + 遗留键 | `peri-middlewares/src/mcp/builtin_runtime_test.rs`（新，**crate 内**） | 主 plan I-02、I-03、S-01、S-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` |
| W5（主 plan） | **V-08** → 主 plan **V-04** | acceptance 记录 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（新） | V-01…V-07 全部 → | 复跑 V-01…V-07 的全部命令并记录终态 |

**并发注意（已按主 plan §7 收敛）**：host 侧断言全部落在**一个**新文件 `peri-acp/src/host/mcp_v4_builtin_test.rs`，由主 plan 的 **V-02 单 owner** 持有 `peri-acp/src/host/mod.rs`；crate 内的启动/审批/关闭/隔离断言全部落在**一个**新文件 `peri-middlewares/src/mcp/builtin_runtime_test.rs`，由主 plan 的 **V-01 单 owner** 持有，其测试模块挂载 `mcp/mod.rs` 归 **I-02**。因此本文件不再产生「多 task 争同一文件」的并发约束。

## 9. 每条命令「不覆盖什么」（诚实声明）

| 命令 | 覆盖 | **不覆盖** |
| --- | --- | --- |
| `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture` | 夹具自检（wire 日志文件生成、`tools/call` 分支可达） | 任何生产行为；它不是验收命令，只是「夹具可用」的门槛 |
| `cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | 真实握手 / ready / 失败 / 超时 / 取消 / 首个请求 tools（**并与 V-06 基线对照**） | 工具调用的成功载荷；真实外网请求；Windows |
| `cargo test -p peri-acp --lib -- host::mcp_v4_builtin`（同一文件、同一命令） | 首个 LLM 请求入参、fatal 路径、以及审批 + wire 的 **host 侧复证** | 多轮/并行工具批次；PTC 内部路径；crate 内 `pub(crate)` seam 的细节（归 `mcp::builtin_runtime`） |
| `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | 启动路径、审批 approve/reject 与 wire 计数、两实例 handle / `transport_type == "builtin"` / 关闭互不影响 / **重连只动一个实例** / 无 orphan task / **大 payload（A16：`BUILTIN_DUPLEX_BUF` 只影响背压，不是帧上限，用例规模为 WebFetch 级正文 数十~数百 KB）** | **凭据隔离与 capability root 隔离**（A13：一律 UNVERIFIED）；跨进程隔离；真实外网与真实上传 |
| `cargo test -p peri-middlewares --lib -- permission::tests` / `-- subagent::tests` / `-- hooks::matcher` | 归一后的判定等价性、反证（未知 `mcp__*` 不变）、保留名反例（A3） | PermissionMode 组合穷举；用户实际配置 |
| `cargo test -p peri-middlewares --lib -- assembly::tests` | 链组成、工具集合、策略键、链序相对顺序、**三个工具面（A6）** | 跨进程；TUI 渲染 |
| `cargo test -p peri-middlewares --lib -- mcp::builtin_apply` | 默认层注入 / 覆盖规则（A17）/ 保留名 typed error（A3）/ 非法关闭片段拒绝（A18）/ direct 一致性（A5） | 运行时连接行为 |
| `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | 关闭面四个路径 | TUI completion / ACP updates 两个面（归 S-02/S-08） |
| `cargo test --workspace --lib` | 无回归（含 `peri-tui`，A8） | 集成测试、`e2e/`、Windows |
| `cargo clippy` / `cargo fmt` | 静态质量 | 任何行为语义 |

**另需在 acceptance 显式声明的未覆盖项**：
- builtin 工具体内部的 **cancel 响应**（`WebFetchTool` 的 `_ctx` 被忽略、`ArtifactTool` 亦然；F §9 已记为 UNVERIFIED）；
- Web / Artifact 的**真实网络与上传**行为（需要凭据与公网，本波不做，沿用既有工具测试的覆盖边界）；
- **capability root 隔离 / 凭据隔离**（A13：pool 级字段，builtin 形态下不可证伪）；
- **`PERI_MCP_BUILTIN=off` 的能力面**（A2）：必须写成「off ⇒ 无 Web/Artifact 能力（显式运维开关）」，**不得**写成「回退到旧实现」；
- ~~声明段消失（Web/Artifact 的 prompt declaration 迁移后为 `None`）~~ **已由 A9 关闭**：声明**仍产出**，断言归 **S-02**（`tool_search::declaration`），本文件登记验收行而非缺口。

## 10. 风险与未知

| # | 风险 | 影响 | 缓解 |
| --- | --- | --- | --- |
| H-R1 | U1/U2 不成立 → V-03 落到 crate 内兜底 | BLOCKED 缺口的证据层次降一级 | **已降险**：U1（broker 注入）可行；U2 按 **A10** 的结论是「形态存在但 `PtcScriptedModel` **私有** ⇒ 在 V-06 的夹具文件内复刻」，复刻失败才退到 crate 内兜底；仅剩 U3 影响 V-03b 这一附加层。兜底路径仍需在 V-00/V-06 中明确写出是否启用 |
| H-R2 | U3 不成立 → V-03b / V-04d UNVERIFIED | 少了「wave 1 builtin 实例的 wire」这一层 | 措辞纪律（§6.3）：不得写成「BLOCKED 完全收口」；不得用计数器冒充 wire |
| **H-R6** | 夹具复用错对象（A10） | `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT` **无 wire 日志、无 `tools/call` 分支**，`:101` 兜底回 `-32601` ⇒ 断言永假却看似在跑（最危险的一类假绿） | 夹具必须落在 `mcp_v4_wire_fixture.rs`（V-06 建）；V-03 的「wire 上 `tools/call` 恰好 1/0 条」断言必须以**日志文件内容**为观察量，并有一条夹具自检用例 |
| **H-R7** | 用「保留实例同名覆盖」做错误注入（A3 冲突） | 加载期 typed error ⇒ 整个 host 夹具的配置被拒绝，测试变成测「配置校验失败」而非目标路径 | V-01 的错误注入一律用**异名** stdio server（`fail_fixture` / `hang_fixture`）；保留名（`web`/`artifact`/`cron`/`lsp`/`workspace`）不得被用户配置接管 |
| **H-R8** | 把 capability root / 凭据写成「已验证」（A13） | 不可证伪项被当成通过，误导后续批次 | §5 V-04 的降级声明 + acceptance 未验证项清单模板（§7 第 8 项） |
| H-R9 | 外部集成测试二进制漏跑（不在 `--lib` 门禁内） | 隔离与关闭面证据假绿 | §7 V-07 把两条 `--test` 命令纳入**强制**复跑清单 |
| H-R10 | 新 `*_test.rs` 未挂载 → `0 tests` | 假绿 | IF-H1 的挂载点表 + §4.1 的挂载复核 |
| H-R11 | `HomeRedirect` / `env::set_var` 与并行测试冲突 | flake | 全部涉及环境变量的命令加 `-- --test-threads=1`；沿用 part-1 的 `HOME_LOCK` 形态；`PERI_MCP_BUILTIN` 的设置与恢复必须在该守卫的 `Drop` 内（A11） |
| H-R12 | 把 `catch_unwind`/`sleep` 当作超时证据 | 不稳定、不可复现 | 超时用例必有有界等待上界与明确的失败信息（spike 的 `SERVER_CONVERGE_TIMEOUT` 形态） |
| H-R13 | 全量 lib 绿色被读成整体验收通过 | 虚假完成 | §7 第 11 项要求逐面 PASS/PARTIAL/UNVERIFIED，禁止单一「通过」 |
| H-R14 | 多个 task 并发改 `host/mod.rs` | 互相覆盖模块挂载 | §8 明确同一 owner 串行（V-06 在 W0 建夹具，V-02 在 W4 复用） |

## 11. 非目标

- **不**为 wave 1 增加 `peri-tui` 集成测试或 `e2e/` 场景（沿用 part-1 口径）；`peri-tui` 的 **lib** 测试（S-08 的归一断言）随 `cargo test --workspace --lib` 运行（A8）。
- **不**验证契约 5 的凭据隔离与 capability root 隔离（A13：pool 级字段，**不可证伪**；本波不改变该事实，且必须按 UNVERIFIED 记录，不得用代码阅读结论替代）。
- **不**重测 SubAgent / Workflow / Goal / PTC 的实现（只测迁出后 Web/Artifact 相关的路径等价）。
- **不**为 builtin 引入 `"builtin"` 的 `transport_type` 取值。
- **不**实现 builtin 工具体内的 cancel 响应。
- **不**做真实外网请求与真实 artifact 上传（无凭据、非确定性）。
- **不**改 `peri-middlewares/tests/mcp_isolation_contract.rs` 与 `mcp_host_policy_contract.rs` 的既有断言（新增独立文件，避免改动 part-1 的证据基线）。
- **不**改 `docs/design/**`、`docs/standards/**`、`docs/code-index/**`、`CLAUDE.md`。
- **不**在 part-1 acceptance 记录中修改任何结论（只允许加一行指向新记录的超链接）。
- **不**运行 Windows / `side-projects/local-mcp-server` / `e2e/` 的门禁。

## 12. 对 IF-D1…IF-D9 与本波要求的验证结论

| 冻结接口 | 验证结论 | 承担行 |
| --- | --- | --- |
| **IF-D1** builtin 与既有 transport 共用 `initialize_config` | **可验证**：V-01a 断言真实握手 + ready 出现在**同一** `run_initialize` 路径上；V-01b/c/d 断言失败/超时/取消都走该函数的既有失败分支（`Failed` + fatal，不旁路） | V-01a…d |
| **IF-D2** `ConfigSource::Builtin` | **可验证（中强）**：属类型层，由主 plan **E-01** 的单测承担；H 不重复断言 | —（引用 F-I-02） |
| **IF-D3** 最低优先级 / 可覆盖 / `disabled` 可关 | **可验证（中强）**：主 plan **I-02** 的配置层单测（`mcp::builtin_apply`）+ V-06c 的运行时关闭断言 | V-06c |
| **IF-D4** 纯数据声明表 | **可验证（中强）**：表内容由主 plan **E-01/E-02** 的单测锁定（`builtin_mcp` / `mcp::builtin`）；H 通过「三条判定结果」与「effective name」端到端引用它 | V-05a/c、V-02 |
| **IF-D5** 三个冻结名 | **可验证（强）**：V-02 在**首个 LLM 请求**上逐字断言；V-05c 在策略层断言同名 | V-02、V-05c |
| **IF-D6** 策略一致性 | **可验证（强）**：V-05a（等价）+ V-05b（未知 `mcp__*` 反证）+ V-05c（冻结结果） | V-05a/b/c |
| **IF-D7** 名单 / 锁定测试 / 剔除谓词同步 | **可验证（中强）**：由主 plan **I-03 + S-02** 的锁定测试承担；H 通过 V-06d 登记误剔反证 | V-06d |
| **IF-D8** 槽位处置 | **可验证（强）**：类型删除是编译期事实；`assembly::tests` 的链序与名单锁定为运行时证据 | V-06a |
| **IF-D9** `system_mcp` + `system_mcp_tools` 可见性等价 + BLOCKED 收口 | **可验证（强）**：V-02（等价性）+ V-03b（crate 内主证据：真实 builtin 实例的审批 + wire，落 `mcp::builtin_runtime`）+ host seam 复证（`host::mcp_v4_builtin`）。⚠ 主 plan R8 把主证据从「stdio fixture」改为「crate 内真实 builtin 实例」，因此本文 §6.3 的措辞纪律随之调整：**主证据不再有 U3 依赖**；U3 只影响「是否能额外做一个独立 wire tap 的对照」 | V-02、V-03、V-03b |
| **IF-D10** 能力关闭 | **可验证（中强）**：四组输入 × **四个面**（V-06b：顶层链 / `parent_tools`（须 direct）/ workflow agent 工具 / deferred bridge）+ G §6.10 的验收行；TUI 面归 S-08（A8），ACP `ToolKind` 面归 S-02 | V-06a/b/c |
| **IF-D13** 直连性声明（**A5**） | **可验证（强）**：V-02 断言首个请求含三个 effective name 且为 direct（crate 内以 `is_direct()` 复核）；V-01 断言 public `build_tool_bridges` 仍全 deferred（锁定测试不改断言） | V-01、V-02 |
| **IF-D14** `call_tool` 结果映射（**A15**） | **可验证（强）**：builtin handler 的 `mcp::builtin::web` / `mcp::builtin::artifact` 断言成功与失败两种 `CallToolResponse` 形态；V-03 的 reject 路径复用失败形态 | `mcp::builtin::web`、`mcp::builtin::artifact`、V-03 |
| **IF-D15** 生效名归一（**A4**） | **可验证（强，纯函数）**：S-01/S-02/S-08 的等价性 + 反证断言（未知 `mcp__*` 不变）+「无硬编码 effective name」grep；H 以精确过滤器复跑 | `permission::tests`、`subagent::tests`、`hooks::matcher`、`tools::invocation`、`event::mapper`、`kit::tool_display`、`truncate` |
| **IF-D11** `transport_type` 三分类 | **可验证（强）**：`builtin_instances_report_builtin_transport_type`（`mcp::builtin_runtime`）+ config-only 行的同源断言（`mcp::builtin_apply`）+ 复跑 `mcp_isolation_contract` 不改断言 | V-04b、§6.4 |
| **IF-D12** reconnect 与 builtin 任务归属 | **可验证（强，归 E-03/V-01）**：close / reconnect 后无 orphan task；**本波不新增 `McpTaskKey` 变体**（如需新增须先登记 R11）。H 只登记该验收行 | V-01（`mcp::builtin_runtime`） |

**对主 plan 覆盖的回应（本文已按 R7/R8/R16/R19 与 A2/A10–A16 修订，实施前请核对）**：
1. **R7/R19**：host 侧断言全部落 `host::mcp_v4_builtin`（单文件单模块）；**A10** 追加：wire 夹具独立成文件 `mcp_v4_wire_fixture.rs`，由 **V-06（W0）** 建立并在 **V-02（W4）** 复用（同一 owner 序列）。
2. **R8**：BLOCKED 缺口的**主证据**在 crate 内 `mcp::builtin_runtime`（真实 builtin 实例 + `pub(crate)` 可达的 `with_direct`）；host 侧为**复证**（用 V-06 的 wire 夹具）。
3. **R16**：§6.4 与 V-04b 已翻转为「必须断言 `transport_type == "builtin"`」；`mcp_isolation_contract.rs` 的 `"stdio"` 断言保持只复跑（夹具 env 改动按 A11/R28）。
4. **A8（取代旧的「peri-tui 排除项」）**：TUI 按名分支**已在范围内**（S-08，走 A4 归一，W3）；本文 §9 与 acceptance 的口径随之改为「TUI lib 测试在 `--workspace --lib` 内，端到端渲染仍 PARTIAL/UNVERIFIED」。
5. **A7（原「未收口冲突」已关闭）**：`MIDDLEWARE_NAMES` 删两键 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS`，`assembly_test.rs:1207-1217` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == `policy_key` 集合」∧「交集为空」；W3 闸门因此可绿（不再需要额外裁决）。
6. **A12**：W0 必须先录迁移前基线（`host::mcp_v4_wire_fixture` + acceptance「迁移前基线」小节），否则 V-02 的「可见性等价」不可证伪。
7. **A3/A18**：保留名不可被用户接管、非法关闭片段必须被加载期拒绝——两者的反例用例归 I-02/S-01，H 在矩阵中登记验收行（§5 V-06 与 §9）。

**对本波三项硬要求的回答**：
1. 「按验收契约 1–7 逐条给出矩阵」 → 本文件 §4 以「本波可观察断言」为单位给出（part-1 的契约 1–4、7 已 PASS，其回归由 V-07 承担；契约 5、6 的闭合程度由 §4 的 V-03/V-04 与本表给出）。
2. 「每条给出模块路径与模块名（过滤能命中、0 tests 视为失败）」 → §4 每行的「承担测试」列 + §4.1 的判定规则；模块名已按主 plan §9 规则 4 收紧。
3. 「夹具策略（真实 rmcp vs 假 peer 的边界）」 → §6.1/§6.2；「不覆盖什么」 → §9。
