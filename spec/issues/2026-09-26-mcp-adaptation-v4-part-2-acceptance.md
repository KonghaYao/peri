# MCP adaptation v4-part-2 — 现场验收记录（wave 1）

**状态**：§1–§11 为 W5 + V-05 末的现场记录（PARTIAL；逐面分级见 §10，本文件不给单一总体「通过」）；**§12 为第二轮收口、§12.7 为第三轮（独立复核后的修复与最终复跑）、§12.8 为第四轮（第三次独立复核的 12 项发现与处置），§12.8 是全文的现行裁决** —— 本波次可证伪面全部闭合；仍 UNVERIFIED 者仅 capability root / 凭据隔离（A13 口径）与 wave 2/3 范围外项，另有三项按「无独立可证伪面」的设计事实登记（Goal 工具面、取消通知抵达、`PERI_MCP_BUILTIN=off` 运维语义），见 §12.5；另登记 1 项 pre-existing 测试隔离缺口（`peri-tui` 全局 atom 用例偶发失败，见 §12.7.3）
**优先级**：高
**类型**：验收记录 / MCP adaptation v4 wave 1（Builtin MCP 运行时 + Web/Artifact MCP）
**创建日期**：2026-09-26
**最后核查**：2026-09-26（§12.8：第三次独立只读复核的 **12 项**发现——0 blocker / 2 major / 10 minor——全部处置；含 §12.3 第 3 行悬空指路与 §12.1 文件计数的更正、Goal 同义反复的撤销、`docs/design/middleware-system.md` 槽位表与 `docs/design/meta-harness.md` 语义段的现行化、三处失效注释清理；命令 35/59–68 复跑取证。此前 §12.7：独立复核后的两处修复 + 文档同步 + 命令 35/41–58 复跑，首跑命中的 2 例 `peri-tui` 偶发失败按 §12.7.3 登记。§1–§11 原文保留不改；§1–§11 的核查时点为 task V-04 的 W5 收口：§1/§2 由 V-06 在 W0 写入、未被改动一字）
**事实源**：`spec/issues/2026-09-26-mcp-adaptation-v4-part-2-plan.md`（主计划，唯一裁决）+ sub-plan E/F/G/H；契约语义仍以 `docs/design/mcp-adaptation-v4-part-1.md` 为准
**owner 传递**：**V-06（W0：§1/§2）→ V-04（W5：§3–§10 终态与三态列）**。§2 由 V-06 在**迁移前 HEAD** 写入，**V-04 只增不改**（主 plan A12 / §7 W5 闸门；sub-plan H §7 第 3 项）；V-06 留在 §3 位的骨架清单已由本节起的各小节逐项落实或显式分级（原始骨架项：交付判据矩阵 / 关闭面矩阵 / part-1 遗留闭合 / 运行命令 / 未验证项 / 文件所有权与整体裁决）。
**证据时点**：HEAD = `0e4fe7cf63f396e9c92e04973bdb2703c027fd89`（与 §2 采集时点同一提交）；本波次全部生产代码改动**未提交**，位于 worktree `/Users/konghayao/code/ai/peri-v4p2`（分支 `feat/mcp-adaptation-v4-part-2`）。本文件所有「本次运行时证据」= 2026-09-26 在该 worktree 上的一组顺序执行（**不并发跑 cargo**），命令与计数见 §6。
**命名消歧**：sub-plan H §4 用 `V-01…V-07` 表示**验收要求**，主 plan §6 用 `V-01…V-06` 表示**任务**；本文件对前者一律加前缀写作 `H-V-xx`，避免与任务号混读（主 plan §12：主计划是唯一裁决）。
**范围**：不实现生产代码；不回填 `docs/design/**`；不追加到 `2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`（part-1 记录的对象与事实源不同，见 sub-plan H §7 理由 1–2）。

## 1. 口径

沿用 part-1 记录 §1 的三态列，任何一格都不得跨态引用：

- **目标归属**：设计文档的 v4 目标划分。**不是**实现状态。
- **当前实现**：代码与契约测试可核对的现状（含未迁移项）。
- **本次运行时证据**：本次现场执行的命令、用例数、exit status 与承担该断言的测试名。无命令则写 `无`，不得用代码阅读替代。

裁决规则（沿用主 plan §8/§9）：`0 tests` 视为失败；绿色局部单测**不**升级为整体迁移结论；`PARTIAL` / `BLOCKED` / `UNVERIFIED` 必须显式标注。错误信息与夹具不含任何真实 secret（主 plan §9 规则 7）：本记录不抄录 env、headers、URL 认证信息、OAuth 值、`PERI_ARTIFACTS_TOKEN`。

## 2. 迁移前基线（A12）—— V-06 在迁移前 HEAD 写入；**V-04 不得改写本小节**

> 主 plan §8 第 1 行：口径为「在**迁移前 HEAD** 上用 V-06 的同一 host 夹具录下首个 LLM 请求的工具名集合，与迁移后（三个冻结 effective name）逐项对照」。**缺此行则「可见性等价」不可证伪。**

### 2.1 现场与观察量

| 项 | 值 |
| --- | --- |
| 采集时点 | **迁移前 HEAD**（本波次任何生产代码改动之前；工作树仅含 V-06 新增的夹具与验收记录，未提交） |
| worktree / 分支 | `/Users/konghayao/code/ai/peri-v4p2` / `feat/mcp-adaptation-v4-part-2` |
| HEAD 提交 | `0e4fe7cf63f396e9c92e04973bdb2703c027fd89` |
| 夹具文件 | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（node 脚本：**每条**收到的 JSON-RPC 行落自己的 wire 日志 `#recv <payload>` + **显式 `tools/call` 分支**；复刻的工具调用 model 替身 `WireScriptedModel`；3 条自检/基线用例） |
| 挂载点 | `peri-acp/src/host/mod.rs`：`#[cfg(test)] #[path = "mcp_v4_wire_fixture.rs"] mod mcp_v4_wire_fixture;` |
| **观察量** | **首个 LLM 请求里模型实际看到的工具名集合**（`ModelRequest.tools` 的名字，按到达顺序） |
| 夹具 MCP server | 异名 `wire_fixture`（真实 stdio node 子进程；`system_mcp: true`、`system_mcp_tools: ["echo"]`、`system_mcp_timeout: 10000`；A3 保留实例名未被占用） |
| 装配路径 | 真实 `.mcp.json` + 真实 `run_initialize`（真实 loader）+ 真实 `McpClientPool` + `ProductionChainAssembler` 链 + 真实 `run_session_loop`；**未**改动任何生产代码 |
| 未复用 | 未复用 `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT`（无 wire 日志、无 `tools/call` 分支，`:101` 落 `-32601`）——A10 明文禁止 |

### 2.2 命令与现场结果

命令（主 plan §6 V-06 行要求的形式；`--nocapture` 用于取出 2.3 的现场名单）：

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture --nocapture --test-threads=1
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.22s
     Running unittests src/lib.rs (target/debug/deps/peri_acp-6091b46f11b74236)

running 3 tests
test host::mcp_v4_wire_fixture::baseline_first_model_request_reports_web_and_artifact_capabilities ... [V-06 迁移前基线] 首个 LLM 请求工具数 = 18；工具名 = ["Agent", "AskUserQuestion", "Bash", "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools", "SkillTool", "TodoWrite", "WebFetch", "WebSearch", "Write", "artifact", "folder_operations", "mcp__wire_fixture__echo"]
ok
test host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call ... ok
test host::mcp_v4_wire_fixture::wire_scripted_model_returns_scripted_tool_call_then_ends_turn ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 704 filtered out; finished in 0.15s
exit status = 0
```

同一基线观察量在不带 `--nocapture` 的主 plan 命令形式下同样绿（证据强度不变，仅少一次现场打印）：

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 704 filtered out
exit status = 0
```

隔离性对照（同一命令组跑全部 `host::mcp_v4*`，证明本夹具与既有 host 夹具互不污染）：

```
$ cargo test -p peri-acp --lib -- host::mcp_v4
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 693 filtered out
exit status = 0
```

（3 次独立复跑基线用例，工具数与名单逐字相同 ⇒ 本观察量在当前环境可复现。）

### 2.3 基线值（迁移前的名字集合）

首个 LLM 请求的工具名集合（**18 项**，字面照抄现场输出）：

```
Agent, AskUserQuestion, Bash, DiscoverSkillsTool, Edit, ExecuteExtraTool, Glob, Grep,
Read, SearchExtraTools, SkillTool, TodoWrite, WebFetch, WebSearch, Write, artifact,
folder_operations, mcp__wire_fixture__echo
```

其中与本波次迁移对象直接相关的四项：

| 工具 | 来源（迁移前） | 模型面名字（迁移前，现场观察） | 代码事实 |
| --- | --- | --- | --- |
| Web 搜索 | `WebMiddleware::build_tools()` 的 direct 工具 | **裸名 `WebSearch`** | `web_search.rs:73`（name 字面量）/`:76`（`is_direct() == true`） |
| Web 抓取 | 同上 | **裸名 `WebFetch`** | `web_fetch.rs:92`（name 字面量）/`:95`（`is_direct() == true`） |
| Artifact 上传 | `ArtifactMiddleware` 的 direct 工具 | **裸名 `artifact`** | `artifact/tool.rs:106`（name 字面量）/`:109`（`is_direct() == true`） |
| 夹具 MCP 实例的 direct 提升 | `.mcp.json` 的 `system_mcp_tools: ["echo"]` 经真实 `prepare_system_tools` 提升 | `mcp__wire_fixture__echo`（**wire 侧仍是裸名 `echo`**） | `system_tools.rs` 提升路径；wire 事实见夹具 `#recv` 日志 |

迁移前侧的两个**否定**事实（同一现场输出）：名单中**不含** `mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`；也**不含**同一夹具 server 的 `mcp__wire_fixture__glob`（未列入 `system_mcp_tools` ⇒ 保持 deferred）。

### 2.4 逐项对照表（V-02 / V-04 的对照目标）

| capability | 迁移前（本行现场证据） | 迁移后应为（IF-D5 冻结 effective name） | 对照判定口径 |
| --- | --- | --- | --- |
| Web 搜索 | 裸名 `WebSearch` 在首个请求中（不含 effective name） | `mcp__web__WebSearch` | 同一夹具、同一观察量、同一命令复跑；**恰有一者**出现 |
| Web 抓取 | 裸名 `WebFetch` 在首个请求中（不含 effective name） | `mcp__web__WebFetch` | 同上 |
| Artifact | 裸名 `artifact` 在首个请求中（不含 effective name） | `mcp__artifact__artifact` | 同上 |
| 夹具实例（对照锚点，不受迁移影响） | `mcp__wire_fixture__echo` 在首个请求中；`mcp__wire_fixture__glob` 不在 | 同左（应逐字不变） | 迁移前后均须成立；变化即「非目标面被误动」 |
| 工具总数 | 18 | 待 V-04 记录（迁移后应仍为 18：三项由裸名换成 effective name，不增不减） | 计数仅供对照，**不**单独作为判据 |

夹具断言写成「两种命名**恰有其一**」（`bare ^ effective`），因此同时排除：① 两者都在（重复暴露）；② 两者都不在（能力净丢失）。这正是「可见性等价」要证伪的两种形态。

### 2.5 本小节证明与不证明什么

- **证明**：迁移前 HEAD 上，模型在**首个 LLM 请求**中确实**直接看到**裸名 `WebSearch` / `WebFetch` / `artifact`（不是经 ToolSearch 摘要），且**没有** `mcp__web__*` / `mcp__artifact__*`；观察量取自真实装配 + 真实 loader + 真实 stdio wire 的同一条生产路径。
- **不证明**：模型是否会真正调用这些工具（V-02/V-03 覆盖调用与审批链，本夹具只准备能力面）；迁移后的等价性（本小节只给迁移前一侧，对照由 V-02/V-04 用同一命令重跑产生）。
- **不覆盖**：TUI / ACP 事件投影面；`PERI_MCP_BUILTIN`（该 env 由 W1 引入，迁移前不存在）。

## 3. 本波交付判据矩阵（三态列；**不得跨态引用**）

口径提醒：**目标归属**（设计文档要什么）≠ **当前实现**（代码事实）≠ **本次运行时证据**（本文件 §6 的命令 + 用例数 + exit status）。本列**不**把代码阅读当作证据；每条「证据」格都写命令号（§6 表）+ 用例名 + 计数。

### 3.1 sub-plan F §1 的四条交付判据

| # | 交付判据 | 目标归属 | 当前实现（代码事实，非证据） | 本次运行时证据（命令 / 用例 / 计数） | 分级 |
| --- | --- | --- | --- | --- | --- |
| F-1 | 三个冻结 effective name 出现在**首个 LLM 请求**且为 direct（IF-D9） | `docs/design/mcp-adaptation-v4-part-1.md` 的目标：Web/Artifact 由内置实例以一等（direct）工具提供 | 声明表 `peri-acp-types/src/builtin_mcp.rs`（逐工具 `direct`）；生效点 `build_typed_tool_bridges`（`tool_bridge.rs:418`）；`system_mcp_tools == declared direct 集合` 由 `mcp::builtin_apply::system_mcp_tools_equals_declared_direct_set_for_every_instance` 锁定 | 命令 **26** `host::mcp_v4_builtin::builtin_instances_expose_frozen_effective_names_on_first_model_request`；现场名单（`--nocapture`，命令 36）＝ 18 项，含 `mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`，且**恰有一者**（裸名不出现）；命令 **16** = 17 passed | **PASS** |
| F-2 | `WebMiddleware` / `ArtifactMiddleware` 及全部四处挂载点消失，两个 `ChainSlot` 处置完成（IF-D8） | IF-D8 / IF-F4：删两槽位，能力改由 builtin 实例提供 | `middleware/mod.rs` 不再声明 `pub mod web`（文件头注释记录删除）；`ChainSlot`（`factory.rs:27` 起）无 `Web` / `Artifact` 变体；`MIDDLEWARE_NAMES` 只含链槽位名、`BUILTIN_INSTANCE_POLICY_KEYS` 承载两键（`meta_harness.rs:143`） | 命令 **17**（`assembly::tests` 36 passed，含两表不变式用例）＋ 命令 **05**（23 passed）＋ 命令 **24**（16 passed）；静态复核：`grep -rn 'WebMiddleware\|ArtifactMiddleware'` 在非测试代码中的命中仅剩注释、声明表 `policy_key` 字符串，以及 §9.3 登记的**未编译死文件** `middleware/web.rs`（不在模块树 ⇒ 不进编译产物） | **PASS**（附 1 项卫生观察，见 §9.3） |
| F-3 | 单独关闭 Web 或 Artifact 的能力在 ARC-CAPABILITY-CLOSURE-001 的**全部关闭面**上可观察 | 关闭语义不降级（IF-D10） | 关闭集的唯一映射源 = 注册表 `policy_key`（`closed_instances`）；四个工具面的过滤落点分别在闸门候选、`static_tool_bridges`、`preparation.rs`、`workflow.rs::build_tools` | 逐面见 **§4**（11 面）；命令 **16 / 17 / 25 / 26** | **PASS**（11 面中 9 面 PASS、2 面 PARTIAL，逐面见 §4） |
| F-4 | 遗留 `"WebMiddleware": false` / `"ArtifactMiddleware": false` **不静默失效** | IF-D10 硬要求：任何「键仍存在但不再生效」的中间态判失败 | 两键进入 `BUILTIN_INSTANCE_POLICY_KEYS`；`build_meta_harness_state`（`frozen.rs`）取两表并集；`closed_instances` 只认 `policy_key` | 命令 **16** `disabled_instance_keeps_builtin_transport_but_not_system_dependency`、`builtin_default_layer_inserts_complete_entries`；命令 **05** `closed_instances_maps_policy_keys_only`；命令 **17**（关闭面①/②/③/④ 随策略键收缩）；命令 **26** `builtin_instance_closure_changes_only_the_first_model_request_surface` | **PASS** |

### 3.2 sub-plan G §1 的三条交付判据

| # | 交付判据 | 目标归属 | 当前实现（代码事实，非证据） | 本次运行时证据 | 分级 |
| --- | --- | --- | --- | --- | --- |
| G-1 | 三个 effective name 的 `default_requires_approval` / `is_edit_tool` / `is_mutation_tool` 判定**等于**原始名判定；并有未知 `mcp__*` 反证 + 等价性测试（IF-D6） | A4/A8：判定型归一 = 替换；匹配型 = 原样优先 | 判定型三个消费点（`permission/mod.rs`、`subagent/mod.rs`）调用 `original_tool_name_of_effective`；未知/外部 `mcp__*` 未命中 ⇒ 沿用既有保守语义 | 命令 **12**（`permission::tests` 32 passed）、命令 **13**（`subagent::tests` 32 passed）；含 `builtin_reserved_name_is_not_parity_hijackable`、未知 `mcp__workspace__Read` ⇒ `requires_approval == true` 的反证 | **PASS** |
| G-2 | `sensitive_tool_entries()` 与 `10_hitl` 段落同步，一致性锁定测试仍绿 | A19 / IF-G3：条目名改 effective name，description 不再宣称「所有 `mcp__*` 一律敏感」 | `permission/mod.rs` 条目名与 description 改写；`prompt_test.rs` 既有断言未改 | 命令 **12** `sensitive_entries_use_effective_names_and_parity_description`；命令 **06**（`prompt::tests` 111 passed，既有 `test_hitl_section_rendered_by_holder` 未改断言） | **PASS** |
| G-3 | 每个受工具名影响的投影面有明确处置 + 「不改会怎样」+ 承担断言 | A4 的 7 个消费点全集（IF-D6/IF-D15） | 7 处消费点全部走单一 helper（静态复核见 §9.1） | 命令 **21**（`event::mapper` 36）、**20**（`tools::invocation` 13）、**24**（`session::tool_catalog` 16）、**14**（`hooks::matcher` 15）、**22**（`kit::tool_display` 14）、**23**（`truncate` 56）、**19**（`tool_search::declaration` 10） | **PASS**（7 处消费点全部有命令覆盖；「关闭实例后 hook / 过滤器」的行为属否定式结构性推论，未单独断言，见 §7 第 9 条） |

### 3.3 sub-plan H §4 的验收要求（H-V-01…H-V-07）

| 要求 | 要点 | 本次运行时证据（命令 / 用例 / 计数） | 分级 |
| --- | --- | --- | --- |
| H-V-01a | 真实协议握手后才 ready（首帧 `server/discover`、全程无 `initialize`） | 命令 **25** `web_instance_wire_uses_modern_discover_and_lists_tools`（wire method 序列 + `tools/list` 成功提交证据）；命令 **26**（两实例被 `await_connected` 接受且 `transport_type == "builtin"`） | **PASS** |
| H-V-01b | 握手/发现失败 ⇒ 不发布 ready、首个 prompt fatal、模型 0 次 | 命令 **26** `system_dependency_failure_is_fatal_even_with_builtin_instances_live`（fatal 投影：非 `Interrupted`、`TurnEnded(Error)`）；发现失败分支的 crate 内形态见命令 **27** `system_mcp_tool_discovery_failure_is_not_an_empty_tool_list` | **PASS**（注入点与 sub-plan H 命名的实现细节不同，语义等价） |
| H-V-01c | 超时**不是** cancel | 命令 **27** `system_mcp_timeout_is_fatal_not_cancelled`（12 passed 中的一员） | **PASS** |
| H-V-01d | 启动期取消 ⇒ `Interrupted` | **无命令**：`host::mcp_v4_builtin` / `mcp::builtin_runtime` / `mcp::initialize` 中不存在取消用例（`grep -rn 'Interrupted\|Cancelled' peri-middlewares/src/mcp/builtin_runtime_test.rs` = 0 命中；`builtin_instance_cancellation_maps_to_interrupted` 不存在）。主 plan §6 V-01 行未要求该项 | **PARTIAL**（缺口登记于 §7 第 8 条；本波不宣称启动期取消语义） |
| H-V-02 | 首个 LLM 请求含三个冻结 effective name，且同 server 未列入 `system_mcp_tools` 的工具不在其中 | 命令 **26**（18 项；`mcp__wire_fixture__echo` 在、`mcp__wire_fixture__glob` 不在；deferred 摘要不含三个 direct 名且条目非空） | **PASS** |
| H-V-03 | BLOCKED 收口：approve ⇒ wire `tools/call` **恰 1 次**；reject ⇒ **0 次** | 命令 **26** `promoted_direct_tool_approval_calls_wire_exactly_once` / `promoted_direct_tool_rejection_never_reaches_wire`（夹具 = stdio node fixture，wire 日志是天然 observable） | **PASS** |
| H-V-03b | 同一断言在 wave 1 的**真实 builtin 实例**上（条件性） | **条件成立**：E 提供了 per-instance wire 观测面（`runtime.rs` 的 `#[cfg(test)]` tap + `BuiltinWireLog`）⇒ 命令 **25** `direct_builtin_tool_call_passes_approval_then_touches_wire_once` / `rejected_direct_tool_call_never_reaches_the_wire` | **PASS** |
| H-V-04a | 两实例独立 transport（关 web 不影响 artifact） | 命令 **25** `closing_web_keeps_artifact_capability_and_real_call`（capability 面 + bridge 仍 direct + 一次真实 `tools/call` 往返：artifact handler 经 wire 返回 IF-D14 固定规则文本） | **PASS** |
| H-V-04b | 独立 handle / pool entry（`!Arc::ptr_eq`、connection key 不同、`all_server_infos()` 逐实例一条） | 命令 **25** `builtin_instances_are_distinct_and_reconnect_touches_one_generation` + `builtin_instances_report_builtin_transport_type` | **PASS** |
| H-V-04c | 重连只动一个实例（web generation 递增、artifact 不变且同 `Arc`） | 命令 **25** 同上用例 | **PASS** |
| H-V-04d | per-instance wire 不串（条件性） | 命令 **25** `per_instance_wire_does_not_cross_between_instances`（发往 web 的请求在 web tap 上计 1、artifact tap 计 0，反向亦然） | **PASS**（**强度附注**：两条链路各自构造独立 pool，故「不串」的可证伪面主要是 **bridge↔实例绑定** 与 handler 路由计数；严格意义的「同一 pool 内两实例 wire 序列互不污染」由 H-V-04a 的同 pool 单向证据补齐，未做同 pool 双向计数） |
| H-V-04e | 关闭互不影响（矩阵形式 `(T,T)/(F,T)/(T,F)`，无 `(F,F)` 连带） | 命令 **25** `closure_matrix_four_faces_on_real_builtin_pool`（三组策略键 + `McpMiddleware=false` 的四面收缩）；命令 **26** 的关闭用例；命令 **17**（`closed_instances` 交集为空） | **PASS**（以等价形态覆盖；未以该命名单独断言二元组） |
| H-V-04z | capability root 隔离 + 凭据隔离 | **无命令**（不可证伪） | **UNVERIFIED**（§7 第 1、2 条） |
| H-V-05a/b/c | 策略等价性 / 未知 `mcp__*` 反证 / 三条判定被钉住 | 命令 **12**、命令 **13** | **PASS** |
| H-V-06a | 禁用实例后工具从 session-local 视图消失 | 命令 **17**（36 passed，含 `assemble_tool_names` 形态断言） | **PASS** |
| H-V-06b | 关闭面：链工具集合 / `parent_tools` / workflow 工具集**同时**不含关闭实例 | 命令 **25** `closure_matrix_four_faces_on_real_builtin_pool`（面①②③④ 逐个断言，含裸名不得出现） | **PASS** |
| H-V-06c | 遗留关闭键不静默失效 | 命令 **16**、命令 **17**、命令 **26** | **PASS** |
| H-V-06d | `MIDDLEWARE_TOOL_NAMES` 不再误剔同名非 middleware 工具 | 命令 **18**（`session::exec::stage_builder` 6 passed） | **PASS** |
| H-V-07a | 全量 lib 回归：无失败、无 ignore 增量、无 `0 tests` | 命令 **30**（`cargo test --workspace --lib --no-fail-fast`）：16 target 中 **14 ok / 2 FAILED**（§6.5）；ignore 无本波次增量（§6.6）；**无 `0 tests`** | **FAIL**（红点归因见 §6.6：与本波次无可达因果路径） |
| H-V-07b/c | lint / 格式门禁 | 命令 **32**（clippy）exit 0、0 warning = **PASS**；命令 **31**（`cargo fmt --all --check`）exit 1、30 处 diff / 4 文件 = **FAIL** | 见 **§6.5 / §10** |

## 4. 关闭面矩阵（`ARC-CAPABILITY-CLOSURE-001`，F §6.3 的 **11 面**，逐面分级）

口径：三条合法关闭路径（策略键 `false` / 用户 `{"web": {"disabled": true}}` / 全局 `PERI_MCP_BUILTIN=off`）下，下列各面**必须同时消失**；「任一面仍可见」即关闭不完整。表中「证据」= §6 命令号 + 用例名 + 计数。

| # | 关闭面 | 承担机制（代码事实） | 本次运行时证据 | 分级 |
| ---: | --- | --- | --- | --- |
| 1 | direct tools（首个 LLM 请求的 `tools`） | 关闭集过滤闸门候选与 `direct` 集合 | 命令 **26** `builtin_instance_closure_changes_only_the_first_model_request_surface`（关 web/关 artifact/两键）＋ `mcp_middleware_closure_removes_every_mcp_tool_from_first_model_request`（`McpMiddleware=false` ⇒ 14 项，无任何 `mcp__*`）；命令 **25** 面① | **PASS** |
| 2 | deferred index / `SearchExtraTools` 描述 | 类型化 `static_tool_bridges` + 关闭集过滤（A6 面①） | 命令 **25** 面②（链工具集合随关闭集收缩、裸名不得出现）；命令 **25** `closed_builtin_instance_disappears_from_collected_tools`；命令 **26** 断言 deferred 条目非空（避免空断言） | **PASS** |
| 3 | ACP updates（工具卡片 / `ToolKind`） | `infer_tool_kind` 走 A4 归一（S-02） | 命令 **21**（`event::mapper` 36 passed，含 `test_tool_start_infer_tool_kind_variants`）——**归一后分类正确**已证；「关闭后无调用 ⇒ 无事件」**未单独断言**（结构性推论：无 bridge ⇒ 无调用） | **PARTIAL** |
| 4 | TUI completion（按名参数摘要 / 输出折叠） | 走 A4 归一，不得硬编码 effective name（A8） | 命令 **22** `kit::tool_display::tests::tui_web_tools_still_summarize_after_migration`、`tui_has_no_hardcoded_effective_name`、`tui_unknown_mcp_names_fall_back_to_generic`；命令 **23**（`truncate` 56） | **PASS**（关闭态无卡片同样是「无调用 ⇒ 无事件」的结构性推论，不单独断言） |
| 5 | 静态 prompt / examples / 声明段 | 声明段仍产出（A9）+ prompt 段落同步（S-06/S-04） | 命令 **19**（`tool_search::declaration` 10 passed，含渲染名为 effective name、裸名不得作为条目、文本来自声明表模板三项断言）；命令 **06**（`prompt::tests` 111） | **PASS**（**中强**：文本等价由「仍产出 + 渲染名字正确 + 模板片段逐字出现」保证，非逐字 diff） |
| 6 | subagent 继承（`parent_tools`） | `preparation.rs` 改类型化构造 ⇒ 声明 direct 生效 | 命令 **25** 面③（`open_builtin_bridges` 与 `direct` 集合一致）；命令 **17**（`assembly::tests`） | **PASS** |
| 7 | workflow agent 工具 | `workflow.rs::build_tools` 补 builtin bridge 提供面 + 关闭集过滤 + `assemble.rs` 传池 | 命令 **25** 面④（生产 workflow 工厂 `default_workflow_middleware_factory_with_pool`）；命令 **17** | **PASS** |
| 8 | 策略键不静默失效 | `BUILTIN_INSTANCE_POLICY_KEYS` + `provider/config.rs` known 集合改两表并集 | 命令 **17**（两表不变式：槽位名 == `MIDDLEWARE_NAMES` 去策略键、策略键 == 声明表 `policy_key` 集合、交集为空）；命令 **33**（`provider::config::tests` 35 passed）；命令 **05** `closed_instances_maps_policy_keys_only` | **PASS** |
| 9 | `MIDDLEWARE_TOOL_NAMES` 不再误剔同名非 middleware 工具 | IF-F5：三裸名已从该表删除 | 命令 **18**（6 passed）；命令 **17**（`meta_harness_disables_each_known_middleware` 等）；静态复核：该表不再含 `WebSearch` / `WebFetch` / `artifact` | **PASS** |
| 10 | 保留名不可被外部 server 接管 | A3：加载期 typed error（`web`/`artifact`/`cron`/`lsp`/`workspace`） | 命令 **16** `reserved_instance_names_cannot_be_taken_over_by_command`、`..._by_url`、`reserved_name_takeover_is_rejected_even_when_injection_is_off`；命令 **12** `builtin_reserved_name_is_not_parity_hijackable`（归一表只以保留名为键） | **PASS** |
| 11 | 非法关闭片段不致命 | A18：`{"web": {"disabled": true, "system_mcp": true}}` 被加载期拒绝 | 命令 **16** `disabled_with_system_mcp_is_rejected_at_load_time`、`illegal_closure_fragment_is_rejected_even_when_injection_is_off`、`system_mcp_timeout_without_system_mcp_is_rejected_before_overlay`；命令 **05** `overlay_rejects_disabled_with_system_declaration` | **PASS** |

不在本表范围：`/artifacts` slash 命令（F §6.3 末段的待核实项，本波未涉）。

## 5. 对 part-1 遗留项的闭合程度

引用对象 = `2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`（**本文件不改它一字**；按 sub-plan H §7 第 3 项，part-1 记录的「加指针」属 V-05 的文档面，不在 V-04 范围）。

| part-1 遗留项（原文位置） | part-1 结论 | 本波闭合程度 | 本次运行时证据 |
| --- | --- | --- | --- |
| §3 表「凭据隔离」 | UNVERIFIED | **仍 UNVERIFIED（闭合 0）** | 无命令。形态未变：`McpClientHandle`（`client/types.rs:85`）13 个字段中无 credential；builtin 形态下两实例共用同一个 pool 内的 `FileCredentialStore`，没有可安全读取/比较的 per-instance 凭据身份 ⇒ 不可证伪（§7 第 2 条） |
| §3 表「capability root 隔离」 | UNVERIFIED | **仍 UNVERIFIED（闭合 0）** | 无命令。形态未变且**更强**：`McpClientPool::capability_profile` 是 pool 级（`client.rs:118`，`pub(crate)`）、`execution_cwd` 是 pool 级 `OnceLock`（`client.rs:64`，`bind_execution_cwd` 于 `:177`）；同一 pool 内的 `web` / `artifact` 共享这两个量 ⇒ 本波**不宣称** capability root 隔离（§7 第 1 条） |
| §3 表「五个目标 MCP 的实例落地」 | 未落地 | **PARTIAL 2/5**：`web` / `artifact` 已落地（含真实 handler、真实 wire、真实审批链）；`cron` / `lsp` / `workspace` 仍只有**保留名**（`BUILTIN_RESERVED_INSTANCE_NAMES = ["web","artifact","cron","lsp","workspace"]`），`find()` 对未实现名返回 `None` | 命令 **03**（11 passed，含 `find_hits_only_implemented_instances`、`reserved_names_superset_of_implemented_instances`）；命令 **12**（`original_tool_name_of_effective("mcp__workspace__Read") == None` 且 `requires_approval == true`）；命令 **16** `reserved_instance_without_transport_is_not_injected_when_unimplemented` |
| §4 表「被提升为 direct 的真实 MCP bridge 走审批链」 | **BLOCKED** | **闭合（本波最重要的一行）**：两条独立证据链，各自同时观察审批与 wire 计数 | 命令 **26** `promoted_direct_tool_approval_calls_wire_exactly_once`（approve ⇒ wire `tools/call` 恰 1 次）、`promoted_direct_tool_rejection_never_reaches_wire`（reject ⇒ 0 次，结果为拒绝语义）；命令 **25** `direct_builtin_tool_call_passes_approval_then_touches_wire_once`、`rejected_direct_tool_call_never_reaches_the_wire`（真实 builtin 实例） |
| §4 表「SubAgent / Workflow / Goal / PTC」 | UNVERIFIED | **PARTIAL**：`parent_tools`（SubAgent 继承面）与 workflow agent 工具面已由 A6 三个工具面覆盖；**Goal / PTC 本波未覆盖** | 命令 **25** `closure_matrix_four_faces_on_real_builtin_pool`（面③ `open_builtin_bridges`、面④ 生产 workflow 工厂）；命令 **17**（`assembly::tests`）。Goal / PTC 无命令 ⇒ 见 §7 第 5 条 |

## 6. 运行命令与现场结果（终态闸门复跑）

**执行环境**：worktree `/Users/konghayao/code/ai/peri-v4p2`，HEAD `0e4fe7cf63f396e9c92e04973bdb2703c027fd89`，2026-09-26 12:22–12:35，**顺序执行（不并发跑 cargo）**。
**判定规则**：`0 tests` = 失败（主 plan §9 规则 3）；「0 tests 判定」列逐行给出结论，无一行命中。
**过滤器纪律**：全部使用主 plan §9 规则 4 的精确清单，未使用任何禁用过滤器（`mcp::builtin` 裸前缀 / `permission` / `subagent` / `-- session::factory`）。

### 6.1 W0–W4 全部闸门命令（29 条，逐条复跑）

| # | 命令（`cargo test -p <crate> --lib -- <filter>`，除标注外） | 现场 `test result:` 原文 | exit | 0 tests 判定 |
| ---: | --- | --- | ---: | --- |
| 1 | `-p peri-acp` `host::mcp_v4_wire_fixture`（V-06 基线命令，对照重跑见 §6.2） | `ok. 3 passed; 0 failed; 0 ignored; 0 measured; 714 filtered out` | 0 | 否 |
| 2 | `-p peri-middlewares` `mcp::builtin_spike`（E-00：W0 基线，应 6 passed） | `ok. 6 passed; 0 failed; 0 ignored; 0 measured; 1921 filtered out` | 0 | 否 |
| 3 | `-p peri-acp-types` `builtin_mcp` | `ok. 11 passed; 0 failed; 0 ignored; 0 measured; 444 filtered out` | 0 | 否 |
| 4 | `-p peri-middlewares` `mcp::transport` | `ok. 13 passed; 0 failed; 0 ignored; 0 measured; 1914 filtered out` | 0 | 否 |
| 5 | `-p peri-middlewares` `mcp::builtin::tests` | `ok. 23 passed; 0 failed; 0 ignored; 0 measured; 1904 filtered out` | 0 | 否 |
| 6 | `-p peri-acp` `prompt::tests` | `ok. 111 passed; 0 failed; 0 ignored; 0 measured; 606 filtered out` | 0 | 否 |
| 7 | `-p peri-middlewares` `mcp::builtin::runtime` | `ok. 16 passed; 0 failed; 0 ignored; 0 measured; 1911 filtered out` | 0 | 否 |
| 8 | `-p peri-middlewares` `mcp::initialize` | `ok. 15 passed; 0 failed; 0 ignored; 0 measured; 1912 filtered out` | 0 | 否 |
| 9 | `-p peri-middlewares` `mcp::tool_bridge` | `ok. 16 passed; 0 failed; 0 ignored; 0 measured; 1911 filtered out` | 0 | 否 |
| 10 | `-p peri-middlewares` `mcp::builtin::web` | `ok. 10 passed; 0 failed; 0 ignored; 0 measured; 1917 filtered out` | 0 | 否 |
| 11 | `-p peri-middlewares` `mcp::builtin::artifact` | `ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1920 filtered out` | 0 | 否 |
| 12 | `-p peri-middlewares` `permission::tests` | `ok. 32 passed; 0 failed; 0 ignored; 0 measured; 1895 filtered out` | 0 | 否 |
| 13 | `-p peri-middlewares` `subagent::tests` | `ok. 32 passed; 0 failed; 0 ignored; 0 measured; 1895 filtered out` | 0 | 否 |
| 14 | `-p peri-middlewares` `hooks::matcher` | `ok. 15 passed; 0 failed; 0 ignored; 0 measured; 1912 filtered out` | 0 | 否 |
| 15 | `-p peri-middlewares` `mcp::config::tests` | `ok. 45 passed; 0 failed; 0 ignored; 0 measured; 1882 filtered out` | 0 | 否 |
| 16 | `-p peri-middlewares` `mcp::builtin_apply` | `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1910 filtered out` | 0 | 否 |
| 17 | `-p peri-middlewares` `assembly::tests` | `ok. 36 passed; 0 failed; 0 ignored; 0 measured; 1891 filtered out` | 0 | 否 |
| 18 | `-p peri-agent` `session::exec::stage_builder` | `ok. 6 passed; 0 failed; 0 ignored; 0 measured; 869 filtered out` | 0 | 否 |
| 19 | `-p peri-middlewares` `tool_search::declaration` | `ok. 10 passed; 0 failed; 0 ignored; 0 measured; 1917 filtered out` | 0 | 否 |
| 20 | `-p peri-agent` `tools::invocation` | `ok. 13 passed; 0 failed; 0 ignored; 0 measured; 862 filtered out` | 0 | 否 |
| 21 | `-p peri-acp` `event::mapper` | `ok. 36 passed; 0 failed; 0 ignored; 0 measured; 681 filtered out` | 0 | 否 |
| 22 | `-p peri-tui` `kit::tool_display` | `ok. 14 passed; 0 failed; 0 ignored; 0 measured; 1670 filtered out` | 0 | 否 |
| 23 | `-p peri-tui` `truncate` | `ok. 56 passed; 0 failed; 0 ignored; 0 measured; 1628 filtered out` | 0 | 否 |
| 24 | `-p peri-agent` `session::tool_catalog` | `ok. 16 passed; 0 failed; 0 ignored; 0 measured; 859 filtered out` | 0 | 否 |
| 25 | `-p peri-middlewares` `mcp::builtin_runtime` | `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1910 filtered out` | 0 | 否 |
| 26 | `-p peri-acp` `host::mcp_v4_builtin` | `ok. 7 passed; 0 failed; 0 ignored; 0 measured; 710 filtered out` | 0 | 否 |
| 27 | `-p peri-acp` `host::mcp_v4_startup` | `ok. 12 passed; 0 failed; 0 ignored; 0 measured; 705 filtered out` | 0 | 否 |
| 28 | `-p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` | `ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` | 0 | 否 |
| 29 | `-p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` | `ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` | 0 | 否 |

**合计**：29 条命令全部 exit 0；**604 个用例通过、0 failed、0 ignored、0 条命中 `0 tests`**。

> 用例名逐条可复核（示例，非全集）：命令 25 含 `production_startup_path_connects_both_builtin_instances`、`web_instance_wire_uses_modern_discover_and_lists_tools`、`direct_builtin_tool_call_passes_approval_then_touches_wire_once`、`rejected_direct_tool_call_never_reaches_the_wire`、`closing_web_keeps_artifact_capability_and_real_call`、`builtin_instances_are_distinct_and_reconnect_touches_one_generation`、`per_instance_wire_does_not_cross_between_instances`、`effective_name_is_not_a_wire_name_on_real_handler`、`large_payload_crosses_builtin_instance_intact`、`shutdown_drains_builtin_tasks_without_orphan`、`closure_matrix_four_faces_on_real_builtin_pool`；命令 26 含 `builtin_instances_expose_frozen_effective_names_on_first_model_request`、`promoted_direct_tool_approval_calls_wire_exactly_once`、`promoted_direct_tool_rejection_never_reaches_wire`、`builtin_injection_off_removes_capabilities_without_fallback`、`system_dependency_failure_is_fatal_even_with_builtin_instances_live`。

### 6.2 A12 对照：V-06 基线命令的**同一夹具、同一观察量**重跑

命令（迁移后，`--nocapture` 仅用于取出 6.3 的现场名单；主 plan 形式的无 `--nocapture` 复跑见 §6.1 第 1 行）：

```
$ cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture --nocapture --test-threads=1
running 3 tests
test host::mcp_v4_wire_fixture::baseline_first_model_request_reports_web_and_artifact_capabilities ...
    [V-06 迁移前基线] 首个 LLM 请求工具数 = 18；工具名 = ["Agent", "AskUserQuestion", "Bash",
    "DiscoverSkillsTool", "Edit", "ExecuteExtraTool", "Glob", "Grep", "Read", "SearchExtraTools",
    "SkillTool", "TodoWrite", "Write", "folder_operations", "mcp__artifact__artifact",
    "mcp__web__WebFetch", "mcp__web__WebSearch", "mcp__wire_fixture__echo"]
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 714 filtered out
exit status = 0
```

> 该用例的打印标签仍是 V-06 写入时的字面量（`[V-06 迁移前基线]`），**不是**本次结论；它的断言是「裸名与 effective name **恰有其一**」，因此同一用例在迁移前/后各自成立。本行为 A12 要求的「两个时点、同一夹具、同一观察量」。

### 6.3 迁移后基线值（与 §2.3 逐项对照）

| capability | §2.3 迁移前（V-06 现场） | 迁移后（本次现场，命令 36 / §6.2） | 对照判定 |
| --- | --- | --- | --- |
| Web 搜索 | 裸名 `WebSearch` | `mcp__web__WebSearch` | 恰有一者出现 ✓（同一命令、同一夹具） |
| Web 抓取 | 裸名 `WebFetch` | `mcp__web__WebFetch` | 恰有一者出现 ✓ |
| Artifact | 裸名 `artifact` | `mcp__artifact__artifact` | 恰有一者出现 ✓ |
| 夹具实例（锚点） | `mcp__wire_fixture__echo` 在、`mcp__wire_fixture__glob` 不在 | 同左，逐字不变 | ✓ 非目标面未被误动 |
| 工具总数 | 18 | 18（命令 26 断言 `names.len() == 18`） | ✓ 不增不减 |
| 裸名残留 | —（迁移前只有裸名） | 三个裸名**均不在**首个请求中（命令 26 逐项断言） | ✓ 无重复暴露、无能力净丢失 |

额外的关闭态现场（命令 36，同一夹具）：

- `PERI_MCP_BUILTIN=off` ⇒ 首个请求 **15 项**，不含任何 Web/Artifact 名字，`mcp__wire_fixture__echo` 仍在（用户 MCP 不受影响），**没有**回退到 middleware 实现的形态。
- `McpMiddleware=false` ⇒ 首个请求 **14 项**，不含任何 `mcp__*`。

### 6.4 复跑 W1–W4 全部闸门命令的终态

- 命令 1–29（§6.1）= W1（3/4/5）、W2（7/8/9/10/11/12/13/14）、W3（15/16/17/18/19/20/21/22/23/24）、W4（25/26/27/28/29）+ W0（1/2）**全部复跑**；逐条 exit 0、无 `0 tests`。
- 编译闸门：`cargo check --workspace --all-targets` → `Finished dev profile ... in 13.62s`，**exit 0**（命令 34）。
- 文档面：`git diff --check` → **exit 0**（命令 35，无空白/冲突标记）。
- **两个补充门禁为红**（不属于 W1–W4 的命令清单，故单独记录）：`cargo test --workspace --lib --no-fail-fast`（命令 30）2 个 target 红、`cargo fmt --all --check`（命令 31）红 —— 见 §6.5，归因见 §6.6 与 §10。**本 §6.4 的「全部命令 exit 0」只覆盖命令 1–29。**
- 设计文档未被回填：`git status --porcelain docs/design/` 为空、`git diff --stat docs/design/` 为空、`docs/design/mcp-adaptation-v4-part-1.md` mtime = `Sep 26 09:48:43 2026`（早于本波次全部源码改动时间戳 10:13–12:07）✓。

## 7. 未验证项清单（逐条给出「为什么不可证伪」与「后续如何证伪」）

> 主 plan §9 规则 9 的强制项：① capability root / 凭据隔离 **UNVERIFIED**；② `PERI_MCP_BUILTIN=off` 的退回态语义是**运维语义而非缺陷**。两项均在本表显式列出。本表**不**用「构造参数不同」这类代码阅读结论替代运行时证据。

| # | 未验证项 | 分级 | 为什么不可证伪（代码事实） | 后续如何证伪（具体动作） |
| ---: | --- | --- | --- | --- |
| 1 | **capability root 隔离**（A13） | **UNVERIFIED** | `McpClientPool::capability_profile` 是 **pool 级**字段（`client.rs:118`，`pub(crate)`）；`execution_cwd` 也是 pool 级 `OnceLock`（`client.rs:64`，`bind_execution_cwd` 于 `:177`）。`web` / `artifact` 同属一个 pool ⇒ 二者**共享**这两个量，无 per-instance public observable 可断言「不共享」。`McpConnectionKey` 亦为 `pub(crate)`（外部集成测试读不到） | 引入 per-instance capability profile（或暴露 `#[cfg(test)]` 的 per-instance 读取面）后，断言两实例的 profile 与 execution cwd 的**不同**与不互相影响；在此之前任何断言都是同义反复 |
| 2 | **凭据隔离**（A13） | **UNVERIFIED** | `McpClientHandle`（`client/types.rs:85`）的 13 个字段中**无** credential；凭据由 pool 级 credential store 建立，没有可安全读取或比较的 per-instance 凭据身份 public API。**本记录与夹具不打印、不比较任何凭据值**（§1 口径 / 主 plan §9 规则 7） | 为 handle 增加可比较的凭据**身份**（如 store key 指纹，非凭据本体）后，断言两实例的 fingerprint 不同且关闭其一不影响另一 |
| 3 | **builtin 实例自身的 wire 不串**（A13 ④） | **本轮条件成立 ⇒ 已断言**（H-V-04d PASS），但**强度受限** | E 提供了 per-instance wire 观测面（`mcp/builtin/runtime.rs` 的 `#[cfg(test)]` tap + `BuiltinWireLog`）；命令 25 `per_instance_wire_does_not_cross_between_instances` 双向计数（1 / 0）已绿。**受限处**：两条链路各自构造独立 pool，故严格意义的「**同一 pool 内**两实例 wire 序列互不污染」未被双向断言 | 在**同一 pool**（`StartupFixture` 形态）内对两实例同时挂 tap，做双向计数与 method 序列比对；或断言关闭/重连其一之后另一实例的 wire method 序列逐字不变 |
| 4 | **`PERI_MCP_BUILTIN=off` 的运维语义**（A2） | **能力面 PASS ＋ 运维语义 UNVERIFIED（已声明语义，非缺陷）** | 能力面已由运行时证据证伪性覆盖：命令 26 `builtin_injection_off_removes_capabilities_without_fallback`（15 项、无 Web/Artifact 名字）、命令 16 `builtin_overlay_is_skipped_entirely_when_policy_is_none`、命令 28 `builtin_injection_off_leaves_pool_with_exactly_the_fixture_servers`、命令 27 `builtin_injection_off_leaves_only_fixture_servers`。**未验证的是运维语义**：生产环境把该 env 置 off 之后的实际退避行为（用户升级后 Web/Artifact 能力**不存在**，且 middleware 提供面已删 ⇒ **不存在「回退到旧实现」这条路径**）、以及 off **不**改变策略判定面的边界（判定面仍按原始名 parity，见 §3 G-1）。这类「运维承诺」没有可执行的运行时断言面 | ① 在 `docs/reference/mcp-ecosystem.md`（V-05）写成显式运维开关并给出升级须知；② 若要运行时证伪，需新增一条 host seam 用例：off 下**不**存在任何 Web/Artifact 名字**且** `default_requires_approval("mcp__web__WebSearch")` 等判定**不变**（当前只断言了前者） |
| 5 | **builtin 工具体内的 cancel 响应** | **UNVERIFIED** | `mcp/builtin/web.rs` / `artifact.rs` 的设计是「取消 / 超时与 `Err` 同路」（进 IF-D14 的 error 结果文本），但**没有任何用例**在 builtin handler 执行工具体（如 `reqwest` 请求在途）时投递取消并观察响应。命令 27 只有**启动期「超时 ≠ 取消」**一条（`system_mcp_timeout_is_fatal_not_cancelled`），命令 25 / 26 的拒绝路径是审批语义而非取消语义 ⇒ 全波**无任何取消用例**，工具体内更未覆盖 | 在 `mcp::builtin_runtime` 内新增用例：让 builtin handler 的工具体等待一个可控门闩，投递 `AgentCancellationToken`，断言返回 IF-D14 的 error 结果文本、不 panic、wire 上无第二次调用、pool 仍可继续服务 |
| 6 | **真实网络与上传** | **UNVERIFIED** | 两个实例的工具体在测试中全部走**注入的本地 base url 与假 token**（`artifact/client.rs:27` 的 `new(base_url, token)`；web 侧同样不发起真实外网请求）⇒ 真实网络路径、真实上传端点的响应形态、限流/超时/证书路径均未被运行时证据覆盖 | 在受控环境用真实端点做一次 smoke（或用本地 mock server 覆盖 HTML 正文、非 2xx、超时三种形态），并把 base url / token 经环境注入，**不**把任何值写进仓库或本记录 |
| 7 | **Goal / PTC 两个工具面**（part-1 §4 遗留） | **UNVERIFIED** | A6 只要求并只覆盖三个工具面（主链 / subagent 继承 / workflow agent）；Goal 与 PTC 的工具列表是否随 builtin 关闭集同步收缩，本波没有任何断言 | 按 A6 的形态为 Goal / PTC 各加一条「关闭 web/artifact 后其工具列表对应收缩」的断言（落 `assembly::tests` 或各自承担测试） |
| 8 | **启动期取消 ⇒ `Interrupted`**（H-V-01d） | **未落地（PARTIAL）** | `host::mcp_v4_builtin` / `mcp::builtin_runtime` / `mcp::initialize` 中无取消用例；sub-plan H 命名的 `builtin_instance_cancellation_maps_to_interrupted` 在代码中不存在。主 plan §6 V-01 行未要求该项 ⇒ 不构成本波验收缺口，但**不得**读作「已覆盖」 | 按 part-1 既有口径补一条启动期取消用例（断言 `LoopResult::Interrupted` 或 `TurnEnded` 的 interrupted 形态且模型 0 次） |
| 9 | **关闭态的 ACP 事件面 / TUI 卡片面** | **结构性推论，未单独断言**（§4 第 3、4 面记 PARTIAL） | 「关闭 ⇒ 无 bridge ⇒ 无调用 ⇒ 无事件 / 无卡片」是链路上的否定式推论；两条命令（21 / 22）证明的是**归一后分类与摘要仍正确**，不是「关闭后一定没有卡片」 | 关闭 web 后跑一次含 web 调用的脚本化 turn，断言 ACP 事件流中无该 effective name 的 `ToolStart`；TUI 侧对同一事件流断言无卡片渲染 |
| 10 | **`cron` / `lsp` / `workspace` 三个实例落地** | **未落地（目标归属 ≠ 已落地）** | 三者只在保留名表与预留语义中存在；`find()` 返回 `None`、不进归一表 | 归 wave 2/3（主 plan §1.2 非本波目标） |

## 8. 非目标 / 未运行的命令（诚实声明「不覆盖什么」）

### 8.1 本波非目标（主 plan §1.2/§1.3）

- wave 2 / wave 3 内容：`cron` / `lsp` / `workspace` 实例迁移、`side-projects/local-mcp-server/**`；主 plan §4 明确「不属于本批次任何 task」。
- `system_mcp` 的 session 中途声明；builtin 实例的**凭据隔离**（§7 第 2 条）。
- 设计文档回填（`docs/design/**` 一字未改，见 §6.4）。
- `/artifacts` slash 命令（F §6.3 末段的待核实项，本波未涉）。

### 8.2 刻意**未运行**的命令与理由

| 未运行的命令 | 理由（若不写理由就会被误读为已覆盖） |
| --- | --- |
| `cargo test -p peri-agent --lib -- session::factory` | **主 plan §9 规则 4 明令禁止**：`peri-agent/src/session/factory.rs` 无任何测试模块 ⇒ 必然 `0 tests` 而按规则 3 判失败。该文件的槽位断言改挂 `assembly::tests`（命令 17）与 `session::exec::stage_builder`（命令 18） |
| `cargo test -p peri-acp --lib -- session::frozen` | 同因：`frozen.rs` 无测试模块 ⇒ 必然 `0 tests`。其两表并集行为由 `provider::config::tests`（命令 33）与 `assembly::tests`（命令 17）承担 |
| `cargo test -- mcp::builtin` / `-- permission` / `-- subagent`（宽前缀） | 主 plan §9 规则 4 禁用清单：会命中 `builtin_spike` / `permission::auto_classifier` / `subagent::fork` 等非目标模块，计数不可解释 |
| 真实网络命令（外网抓取 / 真实上传） | §7 第 6 条：无真实端点，且不得把任何 URL 认证信息或 token 写进记录 |
| `cargo test --workspace`（全量，含 E2E 与需要真实 provider 的测试） | E2E 见 `e2e/CLAUDE.md`，需要真实 provider 与凭据，不在 wave 1 范围；本波只跑 `--lib`（命令 30，**红点已如实登记**）＋ 两个集成契约文件（命令 28/29） |
| `cargo test --workspace --doc` | doc comment 未在本波新增公开 API（主 plan §9 规则 6：除 §3 冻结者外一律 `pub(crate)`）；未运行即**不宣称** doc tests 覆盖 |
| `docs/**`、`CLAUDE.md` 的同步检查项 | 归 **V-05**（本 task 的下一棒）；V-04 只对「设计文档未被回填」作证据（§6.4） |

## 9. 文件所有权与 diff 越界复核

### 9.1 允许模式的静态复核（主 plan §9 规则 11 / A4 / A8）

| 复核项 | 命令 | 结果 |
| --- | --- | --- |
| 归一 helper 的唯一性 | `grep -rn 'fn original_tool_name_of_effective'` | 仅 `peri-acp-types/src/builtin_mcp.rs:129` 一处定义；消费点 16 个文件命中（含各自测试） |
| 消费点未自建第二张反查表 | `grep -rn '"mcp__web__WebSearch" =>\|"mcp__artifact__artifact" =>'`（非测试） | **0 命中** |
| 消费点未硬编码前缀分支 | `grep -rn 'starts_with("mcp__web\|starts_with("mcp__artifact'` | 仅 2 处，**均在测试文件**（`mcp/builtin_runtime_test.rs:155`、`assembly_test.rs:1510`，用途是断言「不得出现」） |
| 归一未泄漏到事件 / transcript / wire | 命令 25 `effective_name_is_not_a_wire_name_on_real_handler`（wire 侧仍是原始工具名）＋ `tool_bridge.rs:200` 的既有约束「净化不可逆、不得反拆」 | 通过 |
| `mcp__web__` / `mcp__artifact__` 字面量出现位置 | `grep -rn`（排除测试与注册表） | 其余命中**全部是 doc 注释**，无分支逻辑；注册表字面量集中在 `peri-acp-types/src/builtin_mcp.rs`（IF-D5 允许的唯一声明处） |

### 9.2 按 §5 R28（A11）**允许**的夹具 env 改动（唯一被许可的既有测试改动）

| 文件 | 改动内容 | 既有断言 |
| --- | --- | --- |
| `peri-middlewares/tests/mcp_isolation_contract.rs` | `EnvIsolation` 由单键 `HOME` 扩为 `["HOME", "PERI_MCP_BUILTIN"]` 两键并设 `PERI_MCP_BUILTIN=off`；新增 1 条 `builtin_injection_off_leaves_pool_with_exactly_the_fixture_servers`（含「注册表非空」守卫，避免空断言） | **未改**（命令 28：4 passed，其中 3 条为既有用例） |
| `peri-acp/src/host/mcp_v4_startup_test.rs` | `HomeRedirect` 增加 `PERI_MCP_BUILTIN=off`；新增 1 条 `builtin_injection_off_leaves_only_fixture_servers` | **未改**（命令 27：12 passed = 既有 11 + 新增 1） |
| W0 基线证据 `peri-middlewares/src/mcp/builtin_spike_test.rs` | **未改**（文件 mtime 10:06 早于本波次全部源码改动时间戳 10:13–12:07；命令 2 = 6 passed 与 W0 基线一致） | **未改** |

### 9.3 矩阵外但被改动（逐条给出判定）

| 文件 | 改动规模 | 判定 |
| --- | --- | --- |
| `peri-acp/src/session/frozen.rs` | +15/-2（仅 `build_meta_harness_state` 的 `false` 键判定面） | **可追溯**：A7 的两表并集（`MIDDLEWARE_NAMES` ∪ `BUILTIN_INSTANCE_POLICY_KEYS`）。该文件在 §4 未登记 owner（§2 只作为事实行引用），严格意义属「矩阵外」；实际归属与 A7 直接相关，**不是**计划外重构。已在 §5/§4 面⑧ 以行为证据登记 |
| `peri-middlewares/src/assembly/mcp.rs` | +8/-1（`add_mcp` 追加 `.with_builtin_closures(...)`） | **可追溯**：IF-D10 面①/② 的关闭集注入（S-01 提供判定 / I-03 提供落点）。同样未在 §4 逐文件登记；属 §4「`assembly.rs`、`assembly/preparation.rs`、`assembly/workflow.rs`」同目录的装配落点，**不是**越界 |

**卫生项（非正确性失败，如实记录，V-04 不修改他 owner 文件）**：`peri-middlewares/src/middleware/web.rs` 仍在磁盘上且被 git 跟踪为 modified，但**已不在模块树**（`grep -rn 'mod web;' peri-middlewares/src/middleware/` = 0 命中）⇒ 不被编译、`WebMiddleware` 不再存在于任何编译产物；其 `is_direct` 出现次数也已为 0。对照 F §1 判据 2「`WebMiddleware` 提供面消失」= **已达成**（提供面从编译单元消失），遗留的是**死文件**本身；建议后续由 I-01/I-03 的 owner 删除该文件（本 task 不做）。

### 9.4 归属清单（本波次全部改动 = §4 矩阵内 + 上两行矩阵外 + R28 两项）

改动文件共 **72 项**（`git status --porcelain` 实测：modified 63 + untracked 9，其中 `peri-middlewares/src/mcp/builtin/` 为新增目录、含 8 个文件），逐项与 §4 owner 对齐；**V-04 本次只新增/修改 1 个文件** = 本文件 `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（§2 原样保留、未改动）。未触碰 `.claude/`、CI 配置、`docs/design/**`、part-1 的验收记录。

### 6.5 补充命令（跨波次门禁与现场取证，编号 30–40）

| # | 命令 | 现场结果 | exit | 说明 |
| ---: | --- | --- | ---: | --- |
| 30 | `cargo test --workspace --lib --no-fail-fast` | 16 个 lib target：**14 ok / 2 FAILED**；红点 = `peri-js-runtime --lib`（`31 passed; 19 failed`）与 `peri-middlewares --lib`（`1914 passed; 8 failed; 5 ignored`）；其余 14 个 target 全绿（含 `peri-acp` 717、`peri-agent` 875、`peri-tui` 1677、`peri-acp-types` 455） | **非 0（2 targets failed）** | sub-plan H §4 的 H-V-07a 门禁 = **FAIL**；归因见 §6.6。**注**：不带 `--no-fail-fast` 的首跑会在 `peri-js-runtime` 失败后 fail-fast，只报告 6 个 target ⇒ 该 flag 是拿到完整终态的必要条件 |
| 31 | `cargo fmt --all --check` | 30 处 diff、涉及 **4 个文件**：`assembly/workflow.rs`(1)、`assembly_test.rs`(9)、`mcp/builtin_runtime_test.rs`(16)、`mcp/config_test.rs`(4) | **1** | H-V-07c 门禁 = **FAIL**；4 个文件**全部是本波次改动/新增文件**（分属 I-03 / V-01 / I-02），**不是**预存格式债；V-04 不修改他 owner 文件（主 plan §9 规则 1） |
| 32 | `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile ... in 18.34s`；`warning` 计数 0 | 0 | H-V-07b 门禁 = **PASS** |
| 33 | `cargo test -p peri-acp --lib -- provider::config::tests` | `ok. 35 passed; 0 failed; 0 ignored; 0 measured; 682 filtered out` | 0 | 补强 A7 的键集合回归（§4 面⑧） |
| 34 | `cargo check --workspace --all-targets` | `Finished dev profile ... in 13.62s` | 0 | W0/W1/W3 波次闸门 |
| 35 | `git diff --check` | 无输出 | 0 | 主 plan §4「doc 检查项」之一（另一半归 V-05） |
| 36 | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin --nocapture --test-threads=1` | `ok. 7 passed`；现场打印首个请求 18 项名单 + deferred 条目 + off/关闭态名单 | 0 | §6.3 的名单来源 |
| 37 | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture --nocapture --test-threads=1` | `ok. 3 passed` | 0 | A12 对照重跑（§6.2） |
| 38 | `cargo test -p peri-middlewares --lib -- middleware::web_search` | `ok. 16 passed; 0 failed; 0 ignored; 0 measured; 1911 filtered out` | 0 | F §6.4 要求：`web_test.rs` 的 **16 用例**在删除 middleware 提供面后**必须重挂载**——现挂于 `web_search.rs`，实测 16 passed（证明「已挂载且被编译」而非假绿） |
| 39 | `cargo test -p peri-middlewares --lib -- middleware::web_fetch` | `ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1923 filtered out` | 0 | 另一挂载点 |
| 40 | `cargo test -p peri-middlewares --lib`（全量，用于定位 8 个失败） | `FAILED. 1914 passed; 8 failed; 5 ignored` | 非 0 | 8 个失败**全部**为 `ptc::tests::*`；归因见 §6.6 |

### 6.6 全量 lib 回归的两个红点（归因：**不含本波次可归因路径**）

| 红点 | 现场失败面 | 归因证据 | 分级 |
| --- | --- | --- | --- |
| `peri-js-runtime --lib` 19 failed | `executor::tests::*`（`ArtifactUnavailable`）、`executor::tests::lifecycle::*`（node 子进程协议断言，`executor_lifecycle_test.rs:275`） | **结构性**：该 crate 的依赖闭包**不含本波次修改的任何文件**（依赖 = tokio/serde/tempfile/… + `peri-process`；`peri-process` 未改动；本波次**未改任何 `Cargo.toml`**；`git status --porcelain -- peri-js-runtime peri-process` 为空）⇒ 无可达因果路径 | **FAIL 但非本波回归**（归因依据是依赖闭包的结构性论证；「迁移前 HEAD 上是否同样为红」**未**直接复跑——`git stash`/`checkout` 属主 plan §9 禁止操作，建议由 CI 在基线提交上复跑该命令确认） |
| `peri-middlewares --lib` 8 failed | 全部为 `ptc::tests::*`，panic 点一律在 `peri-middlewares/src/ptc/ptc_test.rs:42` | **环境性**：该 panic 是 fixture 复制 `npm-packages/@peri-ptc/dist/{peri-ptc.js,index.js}` 失败（`Os { code: 2, NotFound }`）；`dist/` 是**未跟踪的构建产物**（`git ls-files npm-packages/@peri-ptc` 无 `dist`），本环境未构建 ⇒ 与代码改动无关；`git status --porcelain -- peri-middlewares/src/ptc` 为空 | **FAIL 但非本波回归** |
| ignore 增量 | — | `grep -rn '#\[ignore'` 在本波次全部新增/改动测试文件中 = **0 命中**；`peri-middlewares` 的 5 个 `ignored` 全部是既有环境门控用例（`attribution::*`、`hooks::loader::*`、`terminal` 的 120s 用例、`plugin::marketplace::*` ×2） | 无本波次 ignore 增量 |

## 10. 整体裁决（逐面，**不给单一总体「通过」**）

| 面 | 裁决 | 依据与限制 |
| --- | --- | --- |
| F §1 的四条交付判据 | **PASS**（4/4） | §3.1；F-2 附 1 项卫生观察（死文件 `middleware/web.rs`，见 §9.3） |
| G §1 的三条交付判据 | **PASS**（3/3） | §3.2 |
| `ARC-CAPABILITY-CLOSURE-001` 的 11 个关闭面 | **PARTIAL**（9 PASS + 2 PARTIAL） | §4；面③（ACP 事件）与面④（TUI）的「关闭态」为否定式结构性推论，未单独断言 |
| sub-plan H §4 的验收要求 | **PARTIAL**（17 PASS + 1 PARTIAL + 2 FAIL + 1 UNVERIFIED；20 行要求中 07b/07c 同行分列） | §3.3；`H-V-01d` = PARTIAL（启动期取消无命令）；`H-V-07a` = FAIL（全量 lib 红，非本波回归，§6.6）；`H-V-07c` = FAIL（fmt，红在本波次文件）；`H-V-07b` = PASS（clippy 0 warning） |
| 可见性等价（A12，迁移前后对照） | **PASS（强）** | §6.2/§6.3：同一夹具、同一观察量、同一命令的两个时点；18 项不增不减、三项 capability 恰有一者 |
| 契约 5 扩展（builtin 实例隔离，A13 口径） | **中强 PASS**（①–⑤ 全部有命令） | §3.3 H-V-04a…e；④ 附强度附注与后续强化方法（§7 第 3 条） |
| 契约 6 缺口闭合（被提升 direct 工具走完整审批链） | **PASS（强）** | 两条独立证据链（host seam + crate 内真实 builtin 实例），approve=1 / reject=0 的 wire 计数 |
| part-1 遗留项的闭合 | **PARTIAL** | BLOCKED 项**已闭合**（PASS）；五实例 **2/5**；capability root 与凭据隔离 **UNVERIFIED**；SubAgent/Workflow 面 PASS、Goal/PTC **UNVERIFIED**（§5、§7） |
| 文档纪律（契约 7） | **PARTIAL** | 本文件的「三态列 + 未验证项清单」已齐备；`docs/reference/mcp-ecosystem.md` 与 `docs/code-index/**` 的同步**归 V-05，尚未执行**；设计文档未被回填（已证，§6.4） |
| 代码门禁（本波次新增验收） | **FAIL** | §6.5 命令 31：`cargo fmt --all --check` 红（30 处 diff / 4 文件），4 个文件全部属本波次改动（owner = I-02 / I-03 / V-01）。**这是本文件唯一明确要求「先修后交」的失败项**；V-04 依主 plan §9 规则 1 不代改 |
| 全量回归 | **FAIL（非本波回归）** | §6.5 命令 30 / §6.6：2 个 target 红，均与本波次无可达因果路径（缺 JS/npm 构建产物） |

### 10.1 本次**明确不宣称**（与 §7 一一对应）

capability root 隔离；凭据隔离；`PERI_MCP_BUILTIN=off` 的运维语义（只宣称能力面为零）；builtin 工具体内的取消响应；真实网络抓取与真实上传；Goal / PTC 工具面；启动期取消 ⇒ `Interrupted`；同一 pool 内两实例 wire 的双向不串；`/artifacts` slash 命令。**绿色局部单测不升级为整体迁移结论**：本波只证 wave 1 的两个实例（Web / Artifact），不证 cron / lsp / workspace（§1.2 非目标）。

### 10.2 交棒与后续动作

1. **必须先修**：`cargo fmt` 的 4 个文件（owner = I-02 / I-03 / V-01；命令 31 的 diff 可逐处复现）。
2. **建议补证**：在安装/构建 JS 侧产物（`npm-packages/@peri-ptc` 的 `dist`、node 运行时 artifact）后复跑命令 30，以把 §6.6 的「非本波回归」结构性归因升级为基线对照证据。
3. **V-05**：`docs/reference/mcp-ecosystem.md`、`docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 路由表的同步（含 A2 的 off 运维语义写法：**显式运维开关，不是静默降级，也不存在回退到旧实现的路径**）。
4. **part-1 记录的指针**：按 sub-plan H §7 第 3 项，可在 part-1 记录的两行加**指回本文件**的超链接（唯一被允许的改动：加指针，不改结论）；归 V-05。

## 11. 交付后独立复核（编排者收口，2026-09-26）

本节由批次编排者在 V-05 之后独立执行，只处理 §10.2 的三项（先修 / 建议补证 / 死文件），**不改写 §1–§10 的任何结论**。

| # | 动作 | 现场结果（命令 / 计数 / exit） |
| ---: | --- | --- |
| 1 | `cargo fmt --all`（worktree，仅本波次文件） | 4 个文件格式化完毕；`cargo fmt --all -- --check` → 无输出（PASS）。§10「代码门禁 FAIL」的唯一「先修」项**闭合** |
| 2 | 构建 JS 侧产物：`npm-packages/@peri-ptc` 的 `bun run build` | `dist/{peri-ptc.js,index.js,…}` 已在位；`dist` 被 `.gitignore:200` 忽略，**不进提交**。build 内的 `bunx tsc` typecheck 因 worktree 无 `node_modules` 退出码 2（emit 已完成）——环境限制，非代码缺陷 |
| 3 | `cargo test --workspace --lib --no-fail-fast`（全量复跑，dist 就位后） | **16 个 target：6506 passed / 0 failed / 15 ignored，exit 0**。§6.6 两个红点全部消失：`peri-middlewares --lib` 1922 passed（原 8 个 `ptc::tests` 失败 = 缺 `dist`，环境性归因**得到复跑证实**）；`peri-js-runtime --lib` 由红转绿 |
| 4 | `peri-resources` 抖动复核（第 3 条的对照） | 第 1 次全量（与另一条并发工作流同时跑测试）出现 2 个失败：`sessions::sqlite_store::workspace::tests::test_worktree_slow_git_wait_is_measured_per_call_outside_the_write_lock`（panic 文本「慢 Git 不得让准入失败」，`workspace_test.rs:1742`）与子用例 `test_worktree_registration_admission_child`。隔离复跑 `cargo test -p peri-resources --lib` = **156 passed / 0 failed**；第 2 次全量 = **156 passed / 0 failed** ⇒ 归因**并行负载下的时延不稳**，非本波次回归（本波次未改 `peri-resources` 任何文件） |
| 5 | `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`，warning **0**，exit 0（H-V-07b 复跑） |
| 6 | `cargo check --workspace --all-targets`（终态） | `Finished dev profile ... in 7.64s`，exit 0 |
| 7 | 死文件清理（主 plan §5 R36） | 删除 `peri-middlewares/src/middleware/web.rs`（已不在模块树、不参与编译）；删除后 check / clippy / fmt 仍全绿。sub-plan F §1 判据 2「`WebMiddleware` 提供面消失」在文件系统层面亦成立 |
| 8 | 归一硬编码审计（A4 / A8 复跑） | `grep -rn "mcp__web__\|mcp__artifact__" --include=*.rs peri-*/src` ⇒ **生产消费点 0 命中**；命中项仅为文档注释、冻结声明表与 V-06 夹具的对照表 |
| 9 | 主仓库污染审计 | `/Users/konghayao/code/ai/peri` 的 `git status` 中本批次产物 **0 命中**（该仓库另有并发工作流的独立改动与其提交 `f9793811`，与本批次无关） |
| 10 | layer-imports 门禁（`git commit` 的 `pre-commit` 首轮 REJECT 后修复） | 首轮 `bash scripts/check-layer-imports.sh` 报 2 条越层 import（`ACP-biz-use` / `ACP-model-use`），位置是当时名为 `mcp_v4_wire_fixture.rs` 的 host 夹具，其 import 与**已通过**的 `mcp_v4_startup_test.rs:35-36`、`executor_flow_test.rs:45-47` 逐字同形。豁免按**路径子串**判定（`TEST_EXEMPTS="_test.rs _test/ tests/"`，脚本 `:30`/`:62`）⇒ 夹具重命名为 **`peri-acp/src/host/mcp_v4_wire_fixture_test.rs`**，`#[path]` 保持模块名 `host::mcp_v4_wire_fixture`（`host/mod.rs:68-69`）⇒ 复跑门禁 = **18 条规则 / 违规边 0 / exit 0**；`cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture` = **3 passed / 0 failed**，三条用例名与 §2 基线记录逐字不变。§2.1 的「夹具文件」「挂载点」两格是迁移前记录原貌（受「V-04 只增不改」冻结约束，未改写），现行路径以本行为准；观察量、命令形式、计数与结论不受影响。登记为主 plan §5 **R37** |

**仍未闭合（本批次不做，如实登记）**：

- `web_fetch.rs:95` / `web_search.rs:76` / `artifact/tool.rs:116` 的 `is_direct() -> true` 保留（I-01 按 sub-plan F §8 延后、I-03 未接手）。其唯一活构造点是 builtin MCP server 内部与测试，**无 provider 注册** ⇒ 「提供面」事实已消失（由命令 26 的首个请求名单作证）；删除属卫生项。
- §7 的 10 项未验证清单**逐条不变**。
- `apply_builtin_overlay` 的签名细化与两处矩阵外文件已登记为主 plan §5 **R34 / R35**，死文件清理登记为 **R36**，wire 夹具文件名后缀（本表第 10 行）登记为 **R37**。

## 12. 第二轮收口：§7 可证伪项补齐（编排者，2026-09-26 续）

> **只增不改**：§1–§11 原文（含其分级与裁决）保留为历史记录，本节的写入**不修改**其任何一字。凡本节与 §3.3 / §4 / §7 / §8.2 / §10 / §11 冲突处，**以本节为准**，被超越的行在 §12.4 逐行登记。
> 触发：§11 末尾「仍未闭合」的两条 —— ①`is_direct() -> true` 保留；②§7 十项未验证清单逐条不变。

### 12.1 现场与环境

| 项 | 值 |
| --- | --- |
| worktree / 分支 | `/Users/konghayao/code/ai/peri-v4p2` / `feat/mcp-adaptation-v4-part-2` |
| HEAD（本轮改动之前） | `354705e5 feat(mcp): v4-part-2 wave 1 —— Web/Artifact 实迁为 builtin MCP 实例` |
| 本轮改动面 | **9 个文件**：生产 3（`middleware/web_fetch.rs`、`middleware/web_search.rs`、`artifact/tool.rs`）+ 测试 6；改动性质与「不新增公开 API」的论证见 §12.6 |
| 新增用例 | **11 条**（peri-acp 2 / peri-middlewares 9）＋ **2 处既有用例追加断言**（`builtin_injection_off_removes_capabilities_without_fallback` 的判定面、`test_collect_tools_returns_goal_tool` 的 Goal 反证） |
| 执行纪律 | 顺序执行、**不并发跑 cargo**；过滤器沿用主 plan §9 规则 4 的精确清单；命令编号接续 §6.5（30–40），本轮为 **41–58** |
| 证据时点 | 本节全部「现场结果」= 2026-09-26 在该 worktree 上的一组顺序执行；本轮改动在本节写入后与本节一并提交 |

### 12.2 命令与现场结果（41–58）

| # | 命令 | 现场结果 | exit |
| ---: | --- | --- | ---: |
| 41 | `cargo test --workspace --lib --no-fail-fast` | **16 个 lib target 全 `ok`**；**6517 passed / 0 failed / 15 ignored**（= §11 的 6506 + 本轮 11 条新用例） | 0 |
| 42 | `cargo fmt --all --check` | 无输出 | 0 |
| 43 | `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile ... in 13.32s`；warning **0** | 0 |
| 44 | `bash scripts/check-layer-imports.sh` | 18 条规则、**违规边 0** | 0 |
| 45 | `-p peri-middlewares --lib -- mcp::builtin_runtime` | `ok. 20 passed`（原 17，**+3**） | 0 |
| 46 | `-p peri-acp --lib -- host::mcp_v4_builtin` | `ok. 9 passed`（原 7，**+2**） | 0 |
| 47 | `-p peri-middlewares --lib -- middleware::web_search` | `ok. 19 passed`（原 16，**+3**） | 0 |
| 48 | `-p peri-middlewares --lib -- middleware::web_fetch` | `ok. 6 passed`（原 4，**+2**） | 0 |
| 49 | `-p peri-middlewares --lib -- mcp::builtin::web` | `ok. 11 passed`（原 10，**+1**） | 0 |
| 50 | `-p peri-middlewares --lib -- mcp::builtin::runtime` | `ok. 16 passed`（计数不变：回归确认） | 0 |
| 51 | `-p peri-middlewares --lib -- goal_middleware` | `ok. 8 passed`（计数不变：追加的是既有用例内的反证断言） | 0 |
| 52 | `-p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` | `ok. 4 passed` | 0 |
| 53 | `-p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` | `ok. 5 passed` | 0 |
| 54 | `-p peri-acp --lib -- provider::config::tests` | `ok. 35 passed` | 0 |
| 55 | `-p peri-acp --lib -- host::mcp_v4_wire_fixture` | `ok. 3 passed`（A12 夹具未受本轮影响） | 0 |
| 56 | `-p peri-acp --lib -- host::mcp_v4_startup` | `ok. 12 passed` | 0 |
| 57 | `-p peri-js-runtime --lib`（隔离复跑） | `ok. 50 passed; 0 failed` | 0 |
| 58 | `cargo test --workspace --doc --no-fail-fast` | 16 个 Doc-tests target 全 `ok`；**11 passed / 0 failed / 5 ignored**；无 `FAILED` 命中 | 0 |

**无一条命中 `0 tests`**（主 plan §9 规则 3）：上表 18 条中 **15 条为测试命令**，其 `passed` 计数全部 ≥ 3；42 / 43 / 44 三条是门禁命令（fmt / clippy / layer-imports），不产出 `passed` 计数，不适用本判据。

### 12.3 §7 未验证项清单的逐条重判

| # | 未验证项 | 旧分级（§7） | 本轮分级 | 依据（命令 + 用例 + 边界） |
| ---: | --- | --- | --- | --- |
| 1 | capability root 隔离 | UNVERIFIED | **UNVERIFIED（不变）** | **未引入** per-instance capability profile（或 `#[cfg(test)]` 读取面）：`McpClientPool::capability_profile` 仍是 pool 级（`client.rs:118`）、`execution_cwd` 仍是 pool 级 `OnceLock`，同一 pool 内两实例共享这两量且无 per-instance observable。主 plan §1.3 明文禁止为 wave 2/3 预建抽象，A13 明文要求按 UNVERIFIED 登记 ⇒ 本轮**不**为消除该行而改生产结构。证伪动作仍为 §7 原表第 4 列 |
| 2 | 凭据隔离 | UNVERIFIED | **UNVERIFIED（不变）** | `McpClientHandle`（`client/types.rs:85`）字段中无 credential；无 per-instance 凭据**身份**可比较，且本记录与夹具不读取、不比较、不打印任何凭据值（§1 口径 / 主 plan §9 规则 7） |
| 3 | **同一 pool 内**两实例 wire 不串（H-V-04d 的强度缺口） | PARTIAL（强度受限） | **PASS（按原表给出的动作闭合）** | 命令 45 `same_pool_instances_never_cross_wires_and_reconnect_touches_one_link`：在**同一个** `McpClientPool` 容器内建两条 `TappedLink` ⇒ ① web 调用后 web 计 1 / artifact 计 0；② 调 artifact 后 web 仍 1 / artifact 1；③ method 序列**逐字对照**（artifact 快照逐字不变、web 只追加一帧 `tools/call` 且为前缀），反向同理；④ `pool.reconnect("web")` 后 artifact 序列逐字不变、其 server 侧计数不变、artifact bridge 仍能完成完整往返；⑤ 收尾两条夹具链路收敛 `Quit`、重连新建 task 收敛后 `builtin_task_count() == 0`。**边界（用例内自述）**：server 半边是 `FixtureBuiltinHandler` 替身 ⇒ 证的是「同一 pool 容器内两条独立 wire 互不写入 + 观测面按实例隔离 + 重连只动被点名链路」，**不**等于「生产 handler 在真 loader 下同 pool 不串」——该命题**无证据面**（生产 transport 没有 per-instance tap，`StartupFixture` 系用例只观察握手 / 目录 / 真实往返、**不看线路帧序列**），本记录**不宣称**（§12.5，与用例文档同口径） |
| 4 | `PERI_MCP_BUILTIN=off` 的运维语义 | 能力面 PASS ＋ 运维语义 UNVERIFIED | **判定面 PASS（新）；运维语义 UNVERIFIED（不变，已声明语义）** | 命令 46 `builtin_injection_off_removes_capabilities_without_fallback` 追加判定面：在 env 生效**之前**取 `(default_requires_approval, is_edit_tool)` 基线、生效**期间**对三个冻结 effective name 复算并**逐位比较** ⇒ 一旦有人让这两个判定读 env（或让 off 参与判定）即变红。`is_mutation_tool` 是 `peri-middlewares` 私有 fn、本 crate 不可命名，其同名 parity 由 `subagent/mod_test.rs:509`（`mutation_tool_matches_original_name_policy_for_builtin_names`；507 是其文档注释行）覆盖。**运维语义**（升级后能力不存在、无回退路径）仍是文档承诺：已落 `docs/reference/mcp-ecosystem.md`（提交 `354705e5`），没有可执行的运行时断言面 |
| 5 | builtin 工具体内的 cancel 响应 | UNVERIFIED | **PARTIAL：4 项子要求中 3 项已断言，第 4 项经证据判定「现状无因果」** | 命令 45 `builtin_handler_in_flight_cancel_has_no_replay_and_keeps_pool_serving`：等 server 侧已进入 `tools/call`（可控门闩）后取消 ⇒ ① 取消分支命中（`Cancelled` ＋ `interrupted by user` 同文案，race 形状与 `execution.rs:327-334` 同形）；② **无重放**（wire `tools/call` 恰 1、server 侧恰 1、工具体恰进入 1）；③ 放行被弃置的在飞 handler 后**同一条** bridge 仍完成一次往返（pool 未坏）；④ 收尾 `Quit` ＋ task 排空。**未断言的子要求**：§7 原表写的「断言返回 IF-D14 的 error 结果文本」——取消通知**抵达工具体内**在现状下无因果（handler 丢弃 `RequestContext`；rmcp 不在 future drop 时自动发 `notifications/cancelled`）⇒ 本轮把它登记为**设计事实**而非待补缺口；若要真传取消，后续动作是 handler 侧接取消信号或显式发 `notifications/cancelled` |
| 6 | 真实网络与上传 | UNVERIFIED | **PASS（本地真 HTTP 面 —— §7 原表两条路径之一）；真实外网端点/真实上传仍 UNVERIFIED** | 命令 47 的 `test_websearch_invoke_{round_trips_real_http_200_body,reports_non_2xx_status_and_body,request_timeout_terminates_request}`、命令 48 的 `test_webfetch_invoke_{round_trips_real_http_200_body,reports_non_2xx_status_and_body}`：**本地回环 stub**（真实 TCP 连接，非 mock trait）覆盖 200 正文 / 非 2xx 状态与正文 / 请求超时终止三形态；命令 49 `web_handler_tools_call_reaches_real_http_stub_over_wire`：builtin 实例经**真实 wire**（`tools/call` 往返）打到真 HTTP stub。生产路径行为等价（`new()` 仍用原常量；`trim_end_matches('/')` 对无尾斜杠常量恒等）且**不新增配置项**。真实端点 smoke **未做**：无端点，且主 plan §9 规则 7 禁止把 URL / token 值写进记录 |
| 7 | Goal / PTC 两个工具面（part-1 §4 遗留） | UNVERIFIED | **PASS（PTC 按原形态、按证据；Goal 面按「无独立可证伪面」的设计事实登记）** | PTC：命令 46 `ptc_catalog_section_follows_builtin_instance_closure` 的观察量是**首个模型请求系统文本**里的 `RPC-callable tool catalog` 段（不是 `ModelRequest.tools`）⇒ 段存在性守卫（可解析、非空）＋ 对照锚点（`mcp__wire_fixture__echo` 四个 case 恒在）＋ **差分收缩**（关 web −2 / 关 artifact −1 / 同关 −3）。Goal：§7 原要求的形态（「关闭后 Goal 工具表收缩」）经核实为**同义反复**——`GoalMiddleware::collect_tools` 恒返回 `[GoalTool]`、不消费 `shared_tools` / bridge / 关闭集 ⇒ Goal 面与关闭集**按构造无关，没有独立的可证伪面**，本记录按**设计事实**登记，**不**声称该面随关闭集收缩。命令 51 `test_collect_tools_returns_goal_tool` 断言该面恒为 `[goal]`（承担可证伪力的是既有的 `len == 1` 与 `name() == "goal"` 两条）；本轮追加的「不含 `mcp__` 前缀名」是**非独立守卫**（前两条成立时恒真），只在前两条被放宽时才起约束作用 ⇒ **不**登记为「可证伪替代」（第四轮复核修正，见 §12.8） |
| 8 | 启动期取消 ⇒ `Interrupted`（H-V-01d） | PARTIAL（未落地，无命令） | **PASS（middleware 层；host 端到端**不**宣称）** | 命令 45 `builtin_instance_cancellation_maps_to_interrupted`：`system_mcp = true` 的 builtin 实例 + 清单已发布（闸门真的有依赖可等）但**不**提交任何连接 / discovery evidence + `system_mcp_timeout = 60s` 远大于用例时长 + **预取消** token ⇒ 断言 `Err(AgentError::Interrupted)`（**不是** `MiddlewareError`）、不暂存候选、`elapsed` 远小于 timeout（排除 timeout 路径）。**边界（用例内自述）**：驱动的是闸门内取消（`readiness.rs` 循环入口与 `wait_for_readiness_change` 两条等价分支，用例不区分）；host 层「prompt 启动后、闸门等待中被取消」当前**无确定性门闩**（`stages/mod.rs:691-695` 先早退）⇒ 不等于 host 已端到端验证 |
| 9 | 关闭态的 ACP 事件面 / TUI 卡片面 | 结构性推论，未单独断言 | **PASS（口径修正，取代 §4 面③/面④ 的 PARTIAL）** | 命令 46 `closed_web_tool_call_never_reaches_approval_or_wire`：同一条 turn 内先调**可用**工具（正控制 ⇒ 审批恰 1 次、wire 恰 1 条、审批面只见到该可用工具），随后调模型**编造**的 `mcp__web__WebSearch`（`WebMiddleware=false`）⇒ 审批 0 次、wire 0 条、恰一条 error 结算且文案为 `Tool not found: mcp__web__WebSearch`、该名不在首个 LLM 请求。**口径修正**：§4 面③/面④ 原表述「关闭后无事件 / 无卡片」**不可能成立**（模型编造的调用经 model bridge 直接发 `ToolStarted`，tool dispatch 还会补发成对 Started/Ended），且**不应**成立（失败必须在 UI 可见）；关闭的**可证伪观察量**是「调用面不可路由 + 以未知工具结算」，本节按该形态判定 PASS |
| 10 | `cron` / `lsp` / `workspace` 三个实例落地 | 未落地 | **不变（wave 2 / wave 3，主 plan §1.2/§1.3）** | 无变化：三者在保留名表与预留语义中存在，`find()` 对未实现名返回 `None`、不进归一表 |

### 12.4 被本节超越的既有裁决行（逐行登记；原文不删）

| 被超越的行 | 原分级 / 原文要点 | 现行裁决 | 依据 |
| --- | --- | --- | --- |
| §3.3 `H-V-01d` | **PARTIAL**（启动期取消无命令） | **PASS**（middleware 层；host 端到端不宣称） | §12.3 第 8 行 |
| §3.3 `H-V-04d` 的强度附注 | 「**同一 pool 内**两实例 wire 序列互不污染未被双向断言」 | 该缺口**闭合** | §12.3 第 3 行 |
| §3.3 `H-V-07a` | **FAIL**（全量 lib 红：2 target） | **PASS** | 命令 41（16/16 target 全 ok、6517 passed / 0 failed）＋命令 57（§6.6 红点①的隔离复跑直接证据，取代其结构性归因） |
| §3.3 `H-V-07b` | PASS（clippy 0 warning） | **PASS（复跑）** | 命令 43 |
| §3.3 `H-V-07c` | **FAIL**（fmt 30 处 diff / 4 文件） | **PASS** | 命令 42 |
| §4 面③（ACP updates） | **PARTIAL**（「关闭后无事件」未断言） | **PASS（口径修正）** | §12.3 第 9 行 |
| §4 面④（TUI completion） | **PASS（附结构性推论附注）** | **PASS**（附注由 §12.3 第 9 行的证据形态取代） | 同上 |
| §8.2 「`cargo test --workspace --doc` 未运行」 | 未运行即不宣称 doc tests 覆盖 | **已运行：16 target / 11 passed / 0 failed** | 命令 58 |
| §10「代码门禁（本波次新增验收）」 | **FAIL** | **PASS**（fmt / clippy / layer-imports 三门全绿） | 命令 42 / 43 / 44 |
| §10「全量回归」 | **FAIL（非本波回归）** | **PASS** | 命令 41 / 57 / 58 |
| §10「sub-plan H §4 的验收要求」 | PARTIAL（17 PASS + 1 PARTIAL + 2 FAIL + 1 UNVERIFIED） | **仅 `H-V-04z` 一项 UNVERIFIED，其余各项 PASS** | §12.4 上述各行 |
| §10「`ARC-CAPABILITY-CLOSURE-001` 的 11 个关闭面」 | PARTIAL（9 PASS + 2 PARTIAL） | **11 面 PASS**（面③/④ 按修正口径） | §12.3 第 9 行；面①/②/⑤–⑪ 原证据不变 |
| §10「part-1 遗留项的闭合」 | PARTIAL（Goal / PTC **UNVERIFIED**） | PTC 已闭合（按证据）；Goal 面改为按「**无独立可证伪面**」的设计事实登记（非按证据闭合）；该行**仍为 PARTIAL**，缺口收敛为「3 项无独立可证伪面 + 实例落地 2/5」 | §12.3 第 1、2、7 行 |
| §10.1「本次明确不宣称」清单 | **9 项**（＋末句「绿色局部单测不升级」caveat） | **收敛为 §12.5** | — |
| §11 末尾「仍未闭合」两条 | ①`is_direct()` 保留；②§7 十项不变 | **两条均已处理**：①三个 `is_direct()` 覆盖已删除（§12.6）；②逐条重判见 §12.3 | §12.6 / §12.3 |

### 12.5 本节**仍不宣称**（与 §12.3 逐条对应；末尾两项属 §8.1 非目标，无 §12.3 对应行）

capability root 隔离；凭据隔离；真实外网抓取与真实上传端点；builtin 工具体内的取消**通知抵达**（现状无因果，登记为设计事实）；`PERI_MCP_BUILTIN=off` 的**运维语义**（只有能力面与判定面证据）；host 层的启动期取消端到端；`cron` / `lsp` / `workspace` 三实例落地；**「生产 handler 在真 loader 下同 pool 的 wire 序列隔离」**（生产 transport 无 per-instance tap，`StartupFixture` 系用例只看握手 / 目录 / 真实往返、不看线路帧序列 ⇒ **无证据面**，§12.3 第 3 行的边界括注与用例内自述同此口径）；`/artifacts` slash 命令；跨进程 / 跨机器 MCP 隔离与 workspace root 沙箱（§8.1 非目标）。
**绿色局部单测不升级为整体迁移结论**：本波只证 wave 1 的两个实例（Web / Artifact）。

### 12.6 本轮生产文件改动的性质（不新增公开 API 的论证）

| 文件 | 改动 | 性质与等价性论证 |
| --- | --- | --- |
| `middleware/web_search.rs` | 删除 `is_direct()` 覆盖；`WebSearchTool` 字段化（`base_url` / `timeout`）＋两个 `#[cfg(test)] pub(crate)` 构造器 | `new()` 仍用原常量 ⇒ 生产行为逐位不变；URL 拼接 `trim_end_matches('/')` 对无尾斜杠常量恒等；新构造器仅测试可见（`#[cfg(test)]`），**不新增公开 API、不新增配置项** |
| `middleware/web_fetch.rs` | 同上（一个 `#[cfg(test)]` 构造器） | 同上 |
| `artifact/tool.rs` | 删除 `is_direct()` 覆盖 | 卫生项：该结构体的活构造点只有 builtin `artifact` server 内部与测试；「提供面」事实由命令 46 的首个请求名单作证（`mcp__artifact__artifact` 在、裸名不在 ⇒ 由声明表的逐工具 `direct` 决定，不再由 middleware 工具结构决定） |

**改动纪律复核**：命令 42 / 43 / 44 三门在本轮改动之后全部为绿；命令 52 / 53 两个集成契约文件在本轮改动之后复跑仍为绿；`git status` 无未跟踪产物（`npm-packages/@peri-ptc/dist` 由 `.gitignore:200` 忽略、不进提交）。

### 12.7 第三轮：独立复核后的修复与最终复跑（编排者，2026-09-26 续二）

> **只增不改**：§12.1–§12.6 保留为第一遍收口的记录。本节**取代** §12.1 的「本轮改动面 / 新增用例」两行、§12.6 的生产文件清单与 §12.2 的现场结果（新计数见 12.7.3）；凡冲突以本节为准。
> 触发：§12 写入后的一次**独立只读复核**发现两处「声称与实现不一致」，二者都落在 §12 自身声明的可证伪面内 ⇒ 修掉，而不是在记录里保留。

#### 12.7.1 两处修复

| # | 发现 | 性质 | 修复 | 证据 |
| --- | --- | --- | --- | --- |
| F1 | 同池隔离用例 `same_pool_instances_never_cross_wires_and_reconnect_touches_one_link` 的两条夹具链路**没有把自己的 client 半边登记进 `pool.services`** ⇒ `reconnect` 的「关闭被点名实例的旧 service」在该用例内是**空操作**，「重连只动被点名链路」因此**没有失败模式**（把无关实例一并关掉的实现也能绿） | 断言强度缺陷（主 plan §9 规则 8/9：不得把无失败模式的断言登记为 PASS） | ① `TappedLink` 新增 `hand_service_to_pool()`：夹具自持的 client 半边登记进 `pool.services`（表内已有同名项即断言失败），此后重连**真的**会关掉它；② 重连段新增**因果断言**：被点名链路的 server task 由 `reconnect` 触发收敛（`converge_task → Quit`），未点名链路的 service 仍在表内、server task 未结束、method 序列逐字不变、且仍能完成一次完整往返；③ 用例文档补「证据边界」段，明示前三条交叉断言（计数 / 逐字序列）**不是**路由隔离的证明，实质证据在移交后的 ④⑤⑥ 三条 | 命令 45（`ok. 20 passed`）；该用例现有 **50 处断言宏调用**（12 `assert!` + 38 `assert_eq!`），夹具新增方法 2 个（`hand_service_to_pool` / `converge_task`） |
| F2 | `peri-middlewares/src/artifact/mod.rs` 仍持有**活体** `pub struct ArtifactMiddleware`（`Middleware` impl + `Default` + `collect_tools`），与「Web / Artifact 提供面已删除」的既有声明（`docs/code-index/peri-middlewares.md:37`、§11 末、§12.6）**矛盾**；`ArtifactTool` 的 doc 也仍自称「由独立 `ArtifactMiddleware` 注册」且已删的 `is_direct()` 说明留在原位 | doc/代码矛盾（**提供面**事实错误，非风格问题） | 删除该结构体与其 `Middleware` / `BaseTool` 导入；`ArtifactTool` 的 doc 改为「模型面名字与直连性由 builtin `artifact` 实例的声明表决定（`peri-acp-types/src/builtin_mcp.rs`），本结构体只提供工具核心」 | `cargo check --workspace --all-targets` 0 error；命令 41 / 45 / 46 全绿；全仓 `ArtifactMiddleware` 命中**无活体类型引用**：定义侧只有 `builtin_mcp.rs:100`（`policy_key`）与 `meta_harness.rs:143`（`BUILTIN_INSTANCE_POLICY_KEYS`），其余为配置键字符串（`example/minimal/.peri/settings.json:37`、`example/minimal/README.md:39/68`、各测试夹具）与注释（粗查约 31 行 / 10 文件）——第四轮复核修正，见 §12.8 |

#### 12.7.2 文档同步（DOC-UPDATE-001）

wave 1 提交（`354705e5`）删除了 `WebMiddleware` 链槽位与提供面，但四处**现行时态**文档仍按 middleware 描述 Web / Artifact。本轮逐处改正（均为文档，不动生产契约）：

| 文件 | 改动 |
| --- | --- |
| `docs/design/meta-harness.md` | key 类型表改为「middleware 名 / builtin 实例策略键」两类；场景 2 的期望改为「builtin `web` 实例的关闭键 ⇒ 四个面一并消失」，并区分「关闭链槽位名」的语义；装配示意代码把 Web 例子换成链槽位例子 + `closed_instances` 一句；「关闭面 = middleware 实例」改为「关闭面 = 能力提供者」 |
| `docs/meta-harness.md` | 「middleware 名清单」拆为两张表（`MIDDLEWARE_NAMES` / `BUILTIN_INSTANCE_POLICY_KEYS`，**并集 = 已知键**）；示例说明改为四关闭面；「未知 key」判定补第三类；「关闭 = middleware 不进链」补实例键语义 |
| `docs/design/tool-system.md` | `ArtifactTool` 段落改为「builtin `artifact` 实例的工具核心 + Direct 身份来自注册表逐工具 `direct`」 |
| `docs/code-index/peri-middlewares.md` | 头注记第二轮改动；条目 72 / 105 的「若文件仍在磁盘上即死文件」改为「已删除」并补「`is_direct()` 覆写已删除」 |
| `example/minimal/README.md` | 「Middleware controls」表头下补一句说明：`WebMiddleware` / `ArtifactMiddleware` 不再是 middleware 槽位，而是 builtin 实例关闭键（两行效果与旧配置兼容性不变，故表格本身未改） |

#### 12.7.3 最终复跑（命令 35、41–58；顺序执行，不并发）

| # | 现场结果 | exit |
| ---: | --- | ---: |
| 35 | `git diff --check` 无输出 | 0 |
| 42 | `cargo fmt --all --check` 无输出（**首跑 FAILED**：`builtin_runtime_test.rs:941` 一处 `assert!` 未按 rustfmt 折行；`cargo fmt --all` 修复后复跑 0） | 0 |
| 43 | `cargo clippy --workspace --all-targets -- -D warnings`：`Finished dev profile ... in 4.08s`，warning **0** | 0 |
| 44 | 18 条规则、**违规边 0** | 0 |
| 41 | **第二次全量复跑**：16 个 lib target 全 `ok`；**6517 passed / 0 failed / 15 ignored**（与 §12.2 逐位一致） | 0 |
| 45–57 | 13 条过滤器**逐条计数与 §12.2 一致**：20 / 9 / 19 / 6 / 11 / 16 / 8 / 4 / 5 / 35 / 3 / 12 / 50 | 0 |
| 58 | 16 个 Doc-tests target 全 ok；**11 passed / 0 failed / 5 ignored** | 0 |

**首次全量复跑不是全绿（诚实登记，§9 规则 8）**：首跑为 **6515 passed / 2 failed / 15 ignored**，两个红点都在 `peri-tui`：

- `kit::acp_bridge::tests::test_bridge_reset_rehydrates_pending_compact_note_for_same_session`（`peri-tui/src/kit/acp_bridge_test.rs:391`，断言读进程级全局 atom `ACP_STATE`）
- `kit::acp_bridge::tests::test_hitl_bridge_drops_unowned_events_before_all_side_effects`（同文件 `:762`，断言读全局 `VIEW_MODELS`）

判定与依据：

1. **不在本波次改动面**：本波 **17** 个改动文件中**无** `peri-tui/` 任何文件（`git status`；§12.7 写入时为 16 个，此后 `example/minimal/README.md` 进入本批），两个红点所在文件未被本波次触碰。
2. **非确定性**：隔离复跑 `cargo test -p peri-tui --lib -- kit::acp_bridge --test-threads=1` = `ok. 17 passed`；单独 `cargo test -p peri-tui --lib`（并行）= `ok. 1677 passed`；紧接着的全量复跑（命令 41 第二次）16 target 全绿。即同一测试二进制在同一环境下**三次并行全量里命中一次**。
3. **机理（有代码依据）**：两条断言读的是**进程级共享 atom**（同一 lib target 内全部用例共享）；`#[serial]` 只在 serial 用例之间互斥，**不排斥非 serial 用例**，该文件 17 条用例中 12 条标了 `#[serial]`、5 条未标，`peri-tui` 内另有多个测试文件写同一批 atom ⇒ 并发交错在构造上可行。**未做**：精确定位交错伙伴（需插桩，属 `peri-tui` owner 面）。
4. **标准**：`docs/standards/testing.md:271` 要求依赖全局 atom 的测试隔离并串行化 ⇒ 该 crate 存在 pre-existing 测试隔离缺口。
5. **纪律**：不把该 crate 的偶发失败归因于本波次，也不声称「全量回归恒绿」；本波次的判定证据是「隔离复跑为绿 + 第二次全量复跑 16/16 为绿且计数与 §12.2 逐位一致」。该 pre-existing 缺口**不在本波次修复**（属 `peri-tui` owner 面，主 plan §9 规则 1），仅登记为本节发现项。

#### 12.7.4 本节不改变的结论

§12.3 / §12.4 / §12.5 的逐条分级与「仍不宣称」清单**不变**：F1 / F2 只让既有声称站得住（强度补齐 + 消矛盾），没有新增或扩大任何声称。§12.6 对 `web_search.rs` / `web_fetch.rs` 的「生产行为逐位不变」论证仍成立；`artifact/mod.rs` 的改动性质见 12.7.1 F2。命令 41 的 pre-existing 偶发失败不改变 §12.4「全量回归 PASS」的判定口径——但该行的措辞按本节 12.7.3 第 5 条理解（**判定用隔离复跑 + 全量复跑双证据**，非「每次全量必绿」）。

### 12.8 第四轮：独立复核（第三次）的 12 项发现与逐项处置（编排者，2026-09-26 续三）

> **只增不改**：§12.1–§12.7 保留为历次记录。本节**取代** §12.1 的「本轮改动面」行与 §12.6 的生产文件清单（更正计数见 12.8.1），并**在本节内逐条登记**对 §12.3 第 3 / 4 / 7 行、§12.4、§12.5、§12.7.1、§12.7.3 的就地修正；凡冲突以本节为准。
> 复核形态：**独立只读复核**（`code-reviewer` subagent；只读、不写文件、不跑 cargo），快照时点 2026-09-26 17:02，且**已知目标树在复核期间被本次收口并发写入**（§12.7 与 `example/minimal/README.md` 在 17:00–17:01 落盘）——复核报告据此把「审计前后快照差异」显式分开，本节沿用其口径。
> 结果：**12 项（0 blocker / 2 major / 10 minor）**；未发现生产行为回归、秘密泄漏或遗留标记。**全部 12 项在本轮处置完毕**（11 项修复 + 1 项接受现状），无一项挂账。

#### 12.8.1 本批改动面（更正 §12.1「9 个文件 / 生产 3」）

| 轮次 | 改动面 | 构成 |
| --- | --- | --- |
| §12.1–§12.7（第二 / 三轮，17:02 复核快照） | **17 个文件** | 代码 **10** = 生产 **4**（`middleware/web_search.rs`、`middleware/web_fetch.rs`、`artifact/tool.rs`、**`artifact/mod.rs`**）+ 测试 **6**（`host/mcp_v4_builtin_test.rs`、`mcp/builtin/web_test.rs`、`mcp/builtin_runtime_test.rs`、`middleware/web_test.rs`、`middleware/web_fetch_test.rs`、`goal_middleware_test.rs`）＋ 文档 **5** ＋记录 **2** |
| §12.8（第四轮，本节） | **+3 个文件** | **文档 1**：`docs/design/middleware-system.md`（m6）；**注释 2**：`mcp/builtin/web.rs`、`middleware/mod.rs`（m7 / m8，**纯注释 / 文档注释，无可执行语句变化**） |
| 合计 (`git status --porcelain`) | **20 个文件** | 代码 **12**（含注释-only 2）＋ 文档 **6** ＋ 记录 **2** |

**更正原因**：§12.1 原文「9 个文件：生产 3 + 测试 6」**漏计 `artifact/mod.rs`**（§12.7.1 F2 的删除落点）；§12.7 前言声称取代该行，却**只重述了命令计数、未重述文件计数** ⇒ 由本节补齐并给出轮次分解（第四轮的三项由第三次独立复核触发，见 12.8.2 / 12.8.3）。

#### 12.8.2 两项 major

| # | 发现 | 性质 | 处置 |
| --- | --- | --- | --- |
| M1 | §12.3 第 3 行的边界括注称「生产 handler 在真 loader 下同 pool 不串」**由 `StartupFixture` 系用例承担**，而 `mcp/builtin_runtime_test.rs` 的用例自述明写该命题**无证据面**（生产 transport 没有 per-instance tap，`StartupFixture` 系用例只看握手 / 目录 / 真实往返、不看线路帧序列）；且该括注反指 §12.5，而 §12.5 当时**没有**这一条非声称 ⇒ 双向悬空 | 声称与代码互相否定（记录侧）+ 反向悬空引用 | ① §12.3 第 3 行括注改为「该命题**无证据面** ⇒ 本记录**不宣称**」，与用例自述同口径；② §12.5 补入该条非声称（并注明其属 §8.1 非目标、无 §12.3 对应行）；③ **不改用例文本**（其自述本就是正确口径）⇒ 记录与代码三处一致 |
| M2 | `docs/design/meta-harness.md`（§2.5 语义段）仍写「Artifact 上传由独立 `ArtifactMiddleware` 承载；关闭它仅移除 `artifact`」——与本批删除该结构体、同文件 17 行与 220–222 行、以及 §12.7.2「该文件已同步」的声称**自相矛盾** | 现行文档事实错误（DOC-UPDATE-001 漏网行，不属 12.7.2 列举的四处） | 改为「由 builtin `artifact` MCP 实例（`mcp/builtin/artifact.rs`）承载；策略键 `ArtifactMiddleware: false` 关闭该实例工具面（模型面名字 `mcp__artifact__artifact`），不影响 `ToolSearch` 的元工具」 |

#### 12.8.3 十项 minor（逐项处置）

| # | 发现 | 处置 |
| --- | --- | --- |
| m1 | §12.1 文件计数与实测不符（详见 12.8.1） | **已更正**：12.8.1 给出 17 / 10 / 4 / 6 / 5 / 2 的完整分解 |
| m2 | §12.2 末句「上表 18 条命令的 `passed` 计数全部 ≥ 3」对 42（fmt）/ 43（clippy）/ 44（layer-imports）三条门禁命令不成立 | **已改为**「18 条中 15 条为测试命令，其 `passed` 全部 ≥ 3；42 / 43 / 44 为门禁命令，不产出 `passed` 计数，不适用本判据」 |
| m3 | §12.3 第 7 行把「Goal 工具集合不含 `mcp__` 前缀名」登记为**可证伪替代**，但该断言被同一用例既有的 `len == 1` 与 `name() == "goal"` **逻辑蕴含** ⇒ 独立失败模式为 0（同义反复残留：本轮已识破原要求是同义反复，替换物又落回同一形态） | **已更正**：§12.3 第 7 行改为「Goal 面与关闭集按构造无关 ⇒ **无独立可证伪面**，按**设计事实**登记，不声称该面随关闭集收缩；追加断言为**非独立守卫**，只在前两条被放宽时才起约束作用」；用例内文档注释同步改写（原「本条立刻变红」的措辞撤销）；§12.4 对应行由「Goal / PTC 已闭合」改为「PTC 按证据闭合；Goal 按设计事实登记」，缺口计数改为 **3 项无独立可证伪面** |
| m4 | §12.4「§10.1 清单 \| **10 项**」与 §10.1 实际清单（9 项 + 末句 caveat）不符；§12.5 标题「与 §12.3 **一一对应**」对末尾两项（`/artifacts` slash 命令、跨进程沙箱）不成立 | **已改为**「9 项（＋末句 caveat）」；§12.5 标题改为「与 §12.3 **逐条对应**；末尾两项属 §8.1 非目标，无 §12.3 对应行」 |
| m5 | §12.7.1 F2「证据」列称全仓 `ArtifactMiddleware` 命中「**只剩**两处策略键字符串 + 文档表格」，实测另有 example 配置、README、测试夹具与注释共约 31 行 / 10 文件 | **已改为**「命中**无活体类型引用**：定义侧 2 处（`builtin_mcp.rs:100` / `meta_harness.rs:143`），其余为配置键字符串与注释（约 31 行 / 10 文件）」——实质结论（无活体类型引用）成立，穷尽性声称撤销 |
| m6 | `docs/design/middleware-system.md` 仍是过期现行描述：自称「生产蓝本包含 **26** 个槽位」，且 §3 表仍列 `#13 web`、`#24 artifact` 两槽位（本批已删 `ChainSlot::Web` / `ChainSlot::Artifact`） | **已修正**：槽位数改 **25**（= `ChainSlot` 25 变体，含 `git_watch`）；删去两行并按链蓝本顺序**重编号 1–24**；同步全部编号引用（脚注 #17/#18→#16/#17、#19→#18、#22/#23→#21/#22、#26→#24；§5 贡献表 ptc → #21、tool_search → #22；§6 的 `HookMiddleware` #16→#15；§4 顺序约束「web」→「mcp」）；新增脚注说明 Web / Artifact 由 `mcp` 槽位（#19）客户端侧的 builtin 实例提供、关闭键为策略键 |
| m7 | 两处注释称「Web 两个工具的后端地址是编译期常量，**无法在单测内指向本地桩**，成功/失败只能由替身产生」——本批已加 `#[cfg(test)] with_endpoint_for_test` 与 `web_handler_tools_call_reaches_real_http_stub_over_wire`（真工具体 + 本地回环桩 + 真 wire）⇒ 说明失效 | **已改写** `mcp/builtin/web.rs` 的 `with_tools` 文档与 `mcp/builtin_runtime_test.rs` 文件头：生产地址仍恒为编译期常量；测试两条路径并存（真工具体 + 本地桩 / 替身工具），并指名承担用例 |
| m8 | `middleware/mod.rs` 残留「⚠ 已知跨 task 依赖（S-02）：`tool_search/declaration_test.rs` 仍引用 `crate::middleware::WebMiddleware` ⇒ lib 测试目标会报 E0432」——该文件已改走 `peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES`，全仓 `WebMiddleware` 命中 0 ⇒ 注释所述故障不存在 | **已删除该告警**，改为一行事实说明（依赖已解除） |
| m9 | `docs/design/meta-harness.md` 装配示意里 `let disabled: HashSet<&str>` 与真实签名 `closed_instances(&HashSet<String>)` 不符（按字面不可编译） | **已改为** `HashSet<String>`（`.map(\|(k, _)\| k.clone())`），示例与 `assembly.rs:419` 的实参类型一致 |
| m10 | `docs/code-index/peri-middlewares.md` 头注把「`middleware/web.rs` 文件删除」归入「收口第二轮」，实际该删除发生在 wave 1 提交 `354705e5`（`git log --diff-filter=D` + 提交信息 + §11 第 7 行三处互证） | **已改**：该短语移入「此前（Builtin MCP 实例…）」段并标注提交号 |
| m11 | §12.3 第 4 行引 `subagent/mod_test.rs:507` 实为该用例的**文档注释**行（`#[test]` 在 508、`fn` 在 509） | **已改为** `:509` 并注明 507 是文档注释行 |
| m12 | `example/minimal/README.md` 表头仍为「Middleware controls」，而表中两行现属 builtin 实例策略键 | **接受现状**：表头分组是名义问题；17:01 新增的 Note 已准确声明两键「不再是 middleware 槽位，而是 builtin 实例关闭键（`BUILTIN_INSTANCE_POLICY_KEYS`），旧配置仍被识别」，且 `example/minimal/.peri/settings.json` 的两键行为不变 ⇒ 不触发 DOC-UPDATE-001 的事实错误，本轮不改（避免与本波次无关的示例改动）。**（→ 收口提交 `6eb0ca45` 之后按用户口径在 §12.8.7 改为「已更新表头」）** |

#### 12.8.4 复核未覆盖面（与本节声称边界一致，不升级）

复核为**只读**形态，以下五项在其报告中列为「未能核实」，本节沿用其口径、**不**把它们读作已证或已误：

1. 命令计数与 exit 由本轮复跑（12.8.5）承担；复核侧只做静态自洽核对（§12.2 的 15 条测试计数与各文件测试属性数逐一相等、16 个 `src/lib.rs`、doc-tests 11 passed / 5 ignored、layer-imports 18 条规则）。
2. §12.7.3 的 `peri-tui` 偶发失败**不可复现**：其**代码机理**（17 条用例中 12 条 `#[serial]`、失败点 `:391` 读 `ACP_STATE`、`:762` 读 `VIEW_MODELS`）经复核确认为真；「三次并行全量命中一次」的频次仍是单次现场观察。
3. rmcp「不在 future drop 时自动发 `notifications/cancelled`」只有静态佐证（rmcp 3.1.4 的 `RequestHandle` 无 `Drop`、peri 侧侧内无发送点），**无运行时证据** ⇒ §12.3 第 5 行「设计事实」的定性不变。
4. 真实外网 Tavily / 真实上传端点、capability root 隔离、凭据隔离 —— 与 §12.3 第 1 / 2 / 6 行的 UNVERIFIED 自称一致（复核确认其「不可证伪」的代码依据存在）。
5. §12.7.3 中「`builtin_runtime_test.rs:941` 首跑 fmt 失败」的**修复前状态**不可回读（修复已落地）。

#### 12.8.5 第四轮复跑（命令 35、59–68；顺序执行，不并发 cargo）

| # | 命令 | 现场结果 | exit |
| ---: | --- | --- | ---: |
| 35 | `git diff --check` | 无输出 | 0 |
| 59 | `cargo fmt --all --check` | 无输出 | 0 |
| 60 | `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile ... in 14.92s`；warning **0** | 0 |
| 61 | `bash scripts/check-layer-imports.sh` | 18 条规则、**违规边 0** | 0 |
| 62 | `cargo test --workspace --lib --no-fail-fast` | **首跑失败**（`error: 1 target failed: -p peri-tui --lib`）⇒ **复跑 16/16 target 全 `ok`；6517 passed / 0 failed / 15 ignored**（与 §12.2 / §12.7.3 逐位一致） | 首跑 101 / 复跑 0 |
| 63 | `cargo test --workspace --doc --no-fail-fast` | 16 个 Doc-tests target 全 `ok`；**11 passed / 0 failed / 5 ignored** | 0 |
| 65 | `-p peri-middlewares --lib -- goal_middleware` | `ok. 8 passed; 0 failed; 1928 filtered out` | 0 |
| 66 | `-p peri-middlewares --lib -- mcp::builtin::web` | `ok. 11 passed; 0 failed; 1925 filtered out` | 0 |
| 67 | `-p peri-middlewares --lib -- mcp::builtin::runtime` | `ok. 16 passed; 0 failed; 1920 filtered out` | 0 |
| 68 | `-p peri-middlewares --lib -- mcp::builtin` | `ok. 100 passed; 0 failed; 1836 filtered out` | 0 |

65–68 四条过滤器覆盖本轮被改的 `.rs` 文件所对应的测试面（`goal_middleware_test.rs` / `mcp/builtin/web.rs` + `web_test.rs` / `mcp/builtin_runtime_test.rs` / 整个 `mcp::builtin` 子树）；计数与 §12.2 的对应命令逐位一致。

**本轮改动的性质**：四处 `.rs` 改动**全部是注释 / 文档注释**（`goal_middleware_test.rs` 用例文档、`builtin_runtime_test.rs` 文件头、`mcp/builtin/web.rs` 的 `with_tools` 文档、`middleware/mod.rs` 模块注释），**无任何可执行语句、断言、类型或签名变化**；其余改动为文档（6 个文件）与记录（2 个）。因此本轮复跑的判定口径是「注释变更后重新取证」，**不**构成行为回归的修复——上一轮 6517 passed 的语义结论不由本轮复跑扩大（沿用 §12.7.4）。

**末次复检（命令 69，本节写入之后）**：`git diff --check` 复跑 —— 首跑命中一条 whitespace 报错（本文件 EOF 多出两个空行，由本节追加时引入），删去后复跑**无输出**（exit 0）。命令 35 的 exit 0 覆盖的是 12.8.1–12.8.4 落盘时的树，本节追加后由命令 69 覆盖末态；两者各自对应当次时点的文件树，**末态以命令 69 为准**（本波次全部检查中唯一一次「检查发现 → 修复 → 复检」发生在该空白行上，其余检查均为首跑即绿或按 12.8.6 登记）。

#### 12.8.6 第四轮再次命中 `peri-tui` 偶发失败（诚实登记 + 新增现场数据）

命令 62 的**首跑**再次出现 `-p peri-tui --lib` 失败（exit 101）。本次登记与 §12.7.3 的差异与补充：

1. **用例名缺失（本记录的方法学缺陷，如实写出）**：命令 62 的输出经脚本 `tail -60` 截断，**只留下目标级证据**（`error: 1 target failed: -p peri-tui --lib`），失败用例名**不可回读**。§12.7.3 里那两例的名字来自当时未截断的输出；本轮无法把两处现象归到同一对用例上，只能记为「同一目标、同类现象」。
2. **新增数据（隔离连跑 3 次全绿）**：`cargo test -p peri-tui --lib --no-fail-fast` 连跑 **3 次**，每次 `ok. 1677 passed; 0 failed; 7 ignored`（≈15.4s），**3/3 为绿**。
3. **现象统计（本次会话内，均为同一台机、同一代码树）**：`--workspace` 级全量 lib 共 4 次——§12 第一次**绿**、§12.7 第一次 **2 例红**、§12.7 第二次**绿**、本轮第一次 **红（≥1 例）**、本轮第二次**绿**（即 5 次中 2 红 3 绿）；`-p peri-tui` 单 crate 级共 4 次（§12.7 隔离复跑 1 次 + 本轮 3 次）**全绿**。
4. **机理收窄（相对 §12.7.3 的新结论）**：失败**只在 workspace 级全量里出现**，单 crate 连跑 3 次复现不出 ⇒ 该 crate 的全局 atom 交错需要 workspace 级运行的上下文（构建 / 运行期差异），单纯「同一 lib target 内 serial 与非 serial 用例交错」不足以解释。§12.7.3 第 3 条已注明「未做：精确定位交错伙伴」——本轮**仍未做**，此处只把现象范围收窄并如实登记，**不作**因果断言。
5. **判定不变**：本波 20 个改动文件中**无** `peri-tui/` 任何文件；peri-tui 的失败与 wave 1 的改动面无因果路径；§12.4「全量回归 PASS」的判定口径仍按 §12.7.3 第 5 条（**隔离复跑 + 全量复跑双证据**，非「每次全量必绿」）。该缺口属 `peri-tui` owner 面，按 §9 规则 1 不在本波次修复，仅登记。

#### 12.8.7 第五轮：文档面追加更新（编排者，2026-09-26 续四）

> 触发：收口提交 `6eb0ca45` 之后的「更新文档」口径。**只改文档**——不改代码、不改测试、不改任何分级或声称；**文件集合不变**（仍是 §12.8.1 的 **20** 个文件，本节改动落在其中 2 个文件内），随收口批次的**文档追加提交**落盘。

| # | 位置 | 原文 | 改为 | 依据 / 证据 |
| --- | --- | --- | --- | --- |
| 1 | `example/minimal/README.md:37` | `### Middleware controls` | `### Middleware and builtin instance controls` | §12.8.3 **m12** 由「接受现状」升级为「已更新表头」：该表内两行（`WebMiddleware: false` / `ArtifactMiddleware: false`）现属 builtin 实例关闭键（`BUILTIN_INSTANCE_POLICY_KEYS`），表头分组与内容不符；17:01 的 Note 与两行含义均不变，示例行为（`example/minimal/.peri/settings.json:26` / `:37`）不变 |
| 2 | `docs/code-index/peri-middlewares.md:3` | 「更新：2026-09-26（**收口第二轮**：`artifact/mod.rs` 的 `ArtifactMiddleware` 提供面与 … 三处 `is_direct()` 覆写删除，…」 | 「更新：2026-09-26（**收口第二～四轮**：…（提交 `6eb0ca45`），…」 | 与 §12.8.3 **m10** 同型的**归属错误**：`git show 354705e5:peri-middlewares/src/artifact/mod.rs` 仍含 **5** 处 `ArtifactMiddleware`、`354705e5` 的 `middleware/web_search.rs` 仍含 `fn is_direct` ⇒ 两项删除**不在** wave 1 提交内，而发生在收口提交 `6eb0ca45`（= 第二～四轮的产出）。原文只写「收口第二轮」**低估轮次** |

**本节明确排除**：`docs/design/mcp-adaptation-v4-part-1.md` **不动** —— 该文件自述「已批准目标设计」（第 3 行），并在第 234 行明文「本文件只定义必须满足的行为契约，**不保存**某一次执行的勾选状态、耗时或提交号」⇒ v4-part-2 的落地状态由本记录与主计划承担，不写入该文件；其迁移清单表的「迁移状态」列（第 180 行起，含第 190–191 行的 `ArtifactMiddleware` / `WebMiddleware` 两行）是**目标归属**，不是当前实现描述。

**复检（命令 70）**：`git diff --check` 无输出（exit 0）；表头旧名在 `example/**` 内**无残留**（该串在 `example/` 的唯一命中即被改的那一行），其余命中全部落在 `spec/` 内（§12.7.2、§12.8.3 m12、本节与主计划 R41 —— 都是对本次改名的**记述**）⇒ 无悬空指向；本节不改任何 `.rs`，故不触发 `cargo fmt` / `clippy` / 测试的复跑义务（判定口径同 §12.8.5：命令 59–68 的证据对应 12.8.5 落盘时的 `.rs` 内容，本节未触碰）。
