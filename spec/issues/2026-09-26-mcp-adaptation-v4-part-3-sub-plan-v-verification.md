# MCP adaptation v4-part-3 — sub-plan V：验证与收口

> 当前代码导航（2026-09-29）：Cron/LSP plugin handler 与纯工具测试已拆入独立 crates；当前入口及非零 package 验证命令见 [MCP packages 代码索引](../../docs/code-index/mcp-packages.md)。宿主 tick、pool 与运行时生命周期回归仍由 `peri-middlewares` 承载。下方命令清单记录 wave 2 计划时的落点，作为历史计划保留。

## 覆盖范围表

| 范围 | 责任 / 本文落点 | 收口要求 |
| --- | --- | --- |
| V-01 / W0 | 基线保全与 A/B，§2 | 同一夹具、同一命令；直连、摘要、搜索 XOR 原始记录 |
| V-02 / W1–W3 | overlay、ready、行为与门控，§3–§6 | 新契约有新增/修改断言，不以旧测试非零通过闭合 |
| V-03 / W3 | host 三面、cron/LSP E2E、关闭、shutdown，§3–§5 | 真实生产链路与计数；fake 边界明确 |
| V-04 / W3 | transport/task/state 隔离，§4.4 | builtin 两实例独立；off 零注入；集成测试单独执行 |
| V-05 / W4 | S-05 文档/索引/skill 复核，§7 | `DOC-UPDATE-001` 逐文件清单；不代替 S-05 写实现 |
| V-06 / W4 | acceptance 与门禁，§6–§8 | §8 的 24 行逐行判定、命令、exit、计数、未闭合项 |
| A20 / A22 / A24 / A25 / A30 / A32 / A33 | §2 / §4.1–§5 | 搜索面等价；multi-cwd 退化；关闭五维；LSP 同步契约；supervisor tick join 与单驱动；宿主早于 initialize 注入 |
| F2 / F5 / F6 / F8 / F10 | §3–§6、§8 | 外层超时取消；TUI 回归；builtin 隔离；四类关闭分列；仅验收 ACP 共享 scheduler tick |
| A29 / R23–R28 | §3、§7 | 五维、门控、生命周期、overlay、文档、证据完整性全部落表 |

> 日期：2026-09-26。只规划验证，不表示实现或运行通过。本次仅写本文，不改代码、不跑 cargo、不 commit。
> 裁决事实源：`spec/issues/2026-09-26-mcp-adaptation-v4-part-3-plan.md` §5–§10；现场事实源：同目录 `2026-09-26-mcp-adaptation-v4-part-3-acceptance.md`。wave 1 的 `2026-09-26-mcp-adaptation-v4-part-2-sub-plan-h-verification.md` 只作证据风格参考，不继承其 V 编号。
> 初读主计划为 v2；撰写期间他方将工作树主计划改为 v3，已复核差异（§5–§10 未变）。实际分支 `feat/mcp-adaptation-v4-part-3`、HEAD `a81e0ba6`，不同于任务给定 `b1651aee`；不切换、不重置。歧义与实施前置裁决见文末。

## 1. 证据边界与测试落点

| 记号 | 含义 / 落点 |
| --- | --- |
| 【既有】 | 当前源码有函数；仅作为原有断言/基线证据，不自动覆盖新增契约 |
| 【修改】 | 当前有函数，本波必须扩展到 cron/lsp；acceptance 要列新增断言与修改提交 |
| 【新增】 | 计划新增，当前未落地；不是已通过测试。全名是本 sub-plan 的施工名称，owner 改名须同步本文与台账 |
| host 文件 | 既有 `peri-acp/src/host/mcp_v4_wave2_baseline_test.rs`；新增 `peri-acp/src/host/mcp_v4_wave2_test.rs` → `host::mcp_v4_wave2`，由 V-03 挂载，不另复制 wire 夹具 |
| 行为文件 | 新增 `peri-middlewares/src/mcp/builtin/{cron,lsp,dispatch}_test.rs` → `mcp::builtin::{cron,lsp,dispatch}::tests`；由 C/L/H owner 写，V 复核 |
| 既有测试文件 | `peri-middlewares/src/mcp/builtin_apply_test.rs` → `mcp::builtin_apply_tests`；`builtin_runtime_test.rs` → `mcp::builtin_runtime_tests`；`builtin/runtime_test.rs` → `mcp::builtin::runtime::tests` |
| 同步/声明/装配 | `peri-middlewares/src/lsp/middleware_test.rs` → `lsp::middleware::tests`；`tool_search/declaration_test.rs` → `tool_search::declaration::tests`；`assembly_test.rs` → `assembly::tests` |
| 集成测试 | `peri-middlewares/tests/mcp_isolation_contract.rs`；现为顶层函数，无同名内层模块。计划新加 `mod mcp_isolation_contract` 承载指定全名；不得把 binary 名误当模块名 |
| 协议边界 | builtin 真实 `ServerHandler` + in-process transport/tap；Node stdio `wire_fixture` 只作外部 MCP 对照。不能用两个 stdio peer 替代两个 builtin 实例的隔离结论 |
| 复用先例 | `host::mcp_v4_wire_fixture` 提供 `WireFixtureHarness`、`WireScriptedModel`、`run_wire_prompt`；`host::mcp_v4_builtin` 提供 broker/首请求模式；startup 的实际模块是 `host::mcp_v4_startup_tests` |

所有新文件必须经 `#[cfg(test)]`、`#[path = "…_test.rs"]` 挂载；文件名保留 `_test.rs` 以符合层导入脚本。不能为验证把私有生产状态整体公开；需要观测时优先既有端口、协议日志、cfg(test) 计数 seam，跨 owner 先登记。

## 2. W0 保全与同夹具 A/B 重跑程序（V-01、A20）

| 步骤 | 可复现操作 / 判定 |
| --- | --- |
| 1 冻结 A | acceptance §2 原样保留，生产基线为其记录的 `226fa2bc`；夹具落地 `b1651aee` 不冒充采集 HEAD。保存夹具 blob 标识、两个函数名、临时 HOME 与配置前提；不补造历史输出 |
| 2 前置检查 | 当前 LSP 夹具在 initialize 后写 settings 并注入 `SessionContext.lsp_pool`；H-04 删除该字段后无法字节不变重跑。先解决文末冲突 C2，再进入 B；不得复制成另一套“等价基线” |
| 3 同命令 B | 在指定 worktree 单独执行 `cargo test -p peri-acp --lib -- host::mcp_v4_wave2_baseline --nocapture`。命令与 acceptance §2.2 一致；不要加 filter、改 query 或关闭 builtin |
| 4 同函数/探针 | 必须恰命中 `host::mcp_v4_wave2_baseline::wave2_baseline_first_request_and_deferred_summary` 与 `host::mcp_v4_wave2_baseline::wave2_baseline_lsp_tool_visible_when_server_configured`。各无工具 prompt 模型调用=1；各搜索探针调用=2；wire 自检含 initialize/tools-list；search query=`cron`/`lsp`、`max_results=50` |
| 5 时点不变 | 四个裸名/effective pair 在搜索结果 XOR 为真；wire glob 的摘要条目为空、搜索命中为真。XOR 只断言恰有其一，不能证明迁移完成；B 还必须由终态测试断言裸名=false、effective=true |
| 6 逐字对照 | A/B 原始块并排保留命令、exit、完整 `test result:`、直连表、deferred 条目/载体行、搜索名单与四条 XOR 打印。另列解释列：只比较目标工具成员，搜索排序/前五名不是等价判据 |
| 7 只增不改 | 在 acceptance §4 追加“W0 同夹具迁移后重跑”与时间/HEAD/夹具变更清单；§2 不改、旧失败不删。若 A 原文有截断、占位或缺计数，登记历史证据限制，不事后重建 A |

冻结映射：`cron_register → mcp__cron__cron_register`、`cron_list → mcp__cron__cron_list`、`cron_remove → mcp__cron__cron_remove`、`LSP → mcp__lsp__LSP`。
直连首请求 W0 为 18 项；终态同配置预期仍为 18，四工具裸名/effective 均不在直连表。摘要只比较 `- <名字>:` 条目，不能把 PTC JSON/散文子串当条目。四个 effective 名被 `mcp__` 过滤是已知摘要变化；能力等价以搜索改名映射判，不要求摘要或搜索完整列表字面相等。若 deferred 总数达到 50 导致截断，登记夹具前提破坏，不静默调 limit。

## 3. 主计划 §8 第 1–24 行 → 测试函数映射

下列打印标签是**计划格式**，不是现场证据。每个计数必须先被 assert，再打印；acceptance 抄录实际输出。命令编号见 §6。第 14/17/18 行是登记/人工/门禁行，无单一 Rust 测试函数，不能伪造承载。

| 行 | 承担任务 | 具名测试函数（模块路径 + 函数名） | 断言的可观察量 | 现场证据形态 / 命令 |
| ---: | --- | --- | --- | --- |
| 1 | V-02 / R4/R26 | 【新增】`mcp::builtin_apply_tests::wave2_overlay_contract` | cron/lsp 各 `{}`、缺省、disabled、command/url 接管、非法关闭片段；按 typed variant 断言，off 不放行非法片段 | `[W2 overlay] case/instance/variant/count`，每格一次；M01 |
| 2 | V-02/H-02/A33 | 【修改】`mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances`；【新增】`host::mcp_v4_wave2::wave2_ready_gate_matrix`、`host::mcp_v4_wave2::context_injected_before_initialize_and_rejects_duplicate` | cron/lsp 各 transport+协议初始化+tools 能力+tools-list 在 1R 前；实例上下文（cwd / `Arc<CronScheduler>` / LSP pool / `tick_enabled` / 关闭集）由 `peri-acp/src/host/assemble.rs` 注入早于 `run_initialize`，重复注入 typed `Err`；错误/超时模型=0、无 ready 发布；缺失上下文失败且不 fallback | `[W2 ready] instance/stages/model/ready/error`、`[W2 context] inject_seq/initialize_seq/duplicate_err`；M02、H01、H13 |
| 3 | V-01/V-03 | 【既有】`host::mcp_v4_wave2_baseline::wave2_baseline_first_request_and_deferred_summary`、`host::mcp_v4_wave2_baseline::wave2_baseline_lsp_tool_visible_when_server_configured`；【新增】`host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary`（终态函数名以代码为准；早先写作 `wave2_baseline_*` 系笔误） | 直连四对均缺席；摘要裸名条目消失且 effective 条目为空；搜索四对 XOR 且终态 effective 分支 | 两次原始工具表、摘要、命中名单/XOR；B01、H02 |
| 4 | V-02 | 【修改】`mcp::builtin_runtime_tests::startup_gate_stages_declared_direct_tools_and_required_set` | cron/lsp 的 required entry 存在且 value=[]；仍须 ready；direct 提升=0、必需工具名校验=0 | `[W2 empty-required] entries=2 direct=0 validated_names=0`；M03 |
| 5 | L-01/H-04/V-02 | 【新增】`mcp::builtin::lsp::tests::list_tools_follows_configured_server_set`、`host::mcp_v4_wave2::lsp_handler_constructed_after_config_merge` | 空配置工具=0；非空工具=1、server 启动=0 也可 ready；merge 序号 < construct < initialize；变配置不改变本代快照 | `[W2 lsp-gate] config/tools/processes/order`；L01、H03 |
| 6 | V-03 | 【新增】`host::mcp_v4_wave2::cron_register_tick_approval_continuation` | effective 注册→真实 tick→订阅事件→触发审批→CronTrigger 队列→真实 continuation；批准/拒绝分别判 | `[W2 cron-e2e] register_approval/trigger_approval/queue/dispatch`；H04 |
| 7 | C-03/V-02/V-03/A32 | 【新增】`mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers`、`mcp::builtin::cron::tests::tick_reconnect_has_single_driver_per_interval`、`host::mcp_v4_wave2::reconnect_has_single_tick_driver` | `BuiltinInstanceSupervisor` 持有 `TickGuard{cancel, join}`；`CronMcpServer` 不持有 tick；`close_builtin_task(s)` 经 `supervisor.close(...).await`，先 tick cancel+join 再收敛 server task；关闭后 >2×interval 无新触发且 `tick_is_finished()` 为真；reconnect 后每 interval 恰好 1 次 tick（计数） | `[W2 tick] generation/window/tick/trigger/join/active/finished`；C02、H05 |
| 8 | L-02/L-03/V-03/A30/F2 | 【新增】`lsp::middleware::tests::write_sync_orders_change_then_save`、`host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result`、`peri-lsp::pool::tests::port_did_change_and_did_save_route_paths`、`host::requests::tests::lsp_sync_mock_counts`、`host::stdio::run_server_integration_tests::lsp_sync_stdio_counts`、`host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges` | 唯一 `LspPoolPort` 新增无默认实现的同步 `ready_for(path) -> bool`（读文件前必问）、`async did_change(path, &str) -> Result<(), LspSyncError>`、`async did_save(path) -> Result<(), LspSyncError>`；`LspSyncError` 在 `peri-acp-types`；同一调用严格 didChange→didSave，前者失败仍尝试后者；薄中间件使用装配期 cwd（`after_tool` 无 `ToolContext`）；实现者矩阵覆盖 `peri-lsp/src/pool.rs`、`peri-lsp/src/pool_test.rs`、`peri-acp/src/host/requests_test.rs`（mock）、`peri-acp/src/host/stdio/run_server_integration_test.rs`；替身必须断言调用计数/顺序（read=1、change=1、save=1），不得用“未报错”代替。MCP bridge `TOOL_CALL_TIMEOUT=120s` 包裹 `LspTool(timeout() == None)`：延迟夹具必须断言桥层超时、取消传播且 transport/server task 在收敛期限内为 0；具名函数不得只断言返回错误 | fake server JSON-RPC 方法序列/内容摘要、替身 read/change/save 计数与顺序、tool result、`LspSyncError`；`[W2 timeout] elapsed≈120s/timeout=true/cancelled=true/converged=true`；L03、H06、H14 |
| 9 | S-02/V-02 | 【修改】`meta_harness::tests::builtin_instance_policy_keys_match_declaration_table`、`meta_harness::tests::middleware_names_and_builtin_policy_keys_are_disjoint`、`meta_harness::tests::known_key_union_covers_builtin_policy_keys`（crate=`peri-acp-types`） | 两表迁键、互斥、known union 三不变式；明确包含 cron/lsp 与 LspSync | 每函数 `1 passed` + 空差集/键名单；T02–T04 |
| 10 | V-02/V-03/R23 | 【新增】`mcp::builtin_runtime_tests::policy_close_five_dimensions`、`host::mcp_v4_wave2::lsp_sync_close_cross_matrix` | §5 每格五维；LspMiddleware/LspSync 4 组合、Cron 2 组合；关闭同步不读文件 | `[W2 close] case/visible/read/change/save/tick/ready/handler/pool`；M04、H07 |
| 11 | S-01/V-02 | 【修改】`permission::tests::builtin_effective_names_match_original_name_policy`、`subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names` | 四工具裸/effective policy parity；register 审批且 mutation，其余否；事件载荷不归一 | 四对判定布尔与审批原始载荷，passed；S01、S02、H04 |
| 12 | C-01/V-02 | 【修改】`builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance`（crate=`peri-acp-types`）、`mcp::builtin::tests::builtin_prompt_declaration_matches_registry`；【新增】`tool_search::declaration::tests::wave2_has_no_prompt_declaration` | 四个 None、四工具新声明贡献=0；web/artifact 模板及渲染段逐字不变 | `[W2 declaration] none=4 added=0 wave1_equal=true`；T01、M05、M06 |
| 13 | H-04/V-03/S-05 | 【新增】`host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown` | 单 cwd root_uri 等价；两 session 不同 cwd 共享 host root；session/delete 不关 pool；全部子进程限时回收 | `[W2 cwd] uri/session/close/pid/reaped/orphan` + A22 登记；H08 |
| 14 | V-06/R22/F5/F10 | 无单一函数；静态具名登记 `peri-acp/src/host/assemble.rs`、`peri-middlewares/src/lsp/middleware.rs` 中实际符号；TUI 回归使用 `peri-tui/src/kit/tool_display.rs`、`peri-tui/src/truncate.rs` 的既有归一入口 | TUI 私有 scheduler/tick（`peri-tui/src/app/cron_state.rs`、`peri-tui/src/app/mod.rs`）不在范围；print/stdio 无 tick、LSP 同步覆盖缺口不修；新增 Cron/LSP 注册表条目后，仅断言既有 TUI 归一入口回归，不作全仓 grep 零 tick 判定 | acceptance 缺陷/回归行各符号、位置、现场证据或 UNVERIFIED；D01、S03/S04，不用旧 exit 0 闭合 |
| 15 | V-04/V-03/R25/F6 | 【新增】`mcp_isolation_contract::instances_have_independent_transport_task_and_state`、`host::mcp_v4_wave2::lifecycle_state_matrix`、`host::mcp_v4_wave2::off_has_zero_builtin_injection` | 既有 stdio 回归仍保留，但它关闭 builtin 注入、仅用两台 stdio fixture，不能证明 cron/lsp builtin 的 handler/tick/state 隔离；新增真实 builtin handler + in-process transport/tap 路径，断言两实例 transport/task/state 独立、close/reconnect/shutdown 对象计数；off 四 builtin 零注入 | `[W2 isolation/lifecycle/off] link/task/state/generation/count`；I01、H09、H10；真实 builtin 与 stdio 回归分列 |
| 16 | V-01/V-03/R28 | 第 3 行三个完整函数 +【新增】`host::mcp_v4_wave2::search_execution_uses_effective_names` | A/B 三面分别判；终态四工具可由搜索→ExecuteExtraTool 实际调用，裸名不再可路由 | 原始 A/B 输出 + 四工具结果/命中/handler 调用计数；B01、H02、H11 |
| 17 | S-05/V-05/R27 | 无 Rust 函数；人工 `DOC-UPDATE-001` | 调用文本无旧裸名；索引/槽位/关闭/multi-cwd/wave3 说明齐全；历史证据与归一映射允许裸名，不能全仓零字符串命中 | 文件/符号或段落/残留分类/审阅结论；D02 |
| 18 | V-06 | 无单一函数；`cargo build` / clippy / workspace lib / layer gate | 四门禁各 exit=0，测试确实执行；所有行级新断言另有证据 | 四条原文、各 binary `test result:`、检查数；G01–G04 |
| 19 | V-02/V-03/R10 | 【新增】`host::mcp_v4_wave2::wave1_compatibility_with_wave2`；【既有复跑】`host::mcp_v4_builtin::builtin_instances_expose_frozen_effective_names_on_first_model_request` | 四实例共存时 web/artifact direct/声明/transport 不变；wave1 ready、审批与 wire 口径仍成立 | `[W2 wave1] names/direct/transport/capability` + wave1 具名结果；H12、R01–R05 |
| 20 | H-05/V-02/R12/R13 | 【修改】`mcp::builtin::tests::effective_tool_names_covers_registry`；【新增】`mcp::builtin::dispatch::tests::all_registered_instances_have_handler`、`mcp::builtin::dispatch::tests::builtin_source_propagates` | 四实例 registry→dispatch 一一映射；cron/lsp `ConfigSource::Builtin` 经 discover/status 保留；transport=builtin | `[W2 dispatch/source] instance/handler/source/transport`；M07、D03、D04 |
| 21 | H-03/S-02/R15 | 【新增】`assembly::tests::production_chain_has_only_lsp_sync_slot`、`meta_harness::tests::known_builtin_keys_are_exhaustive`；第 20 行 source 函数 | Cron slot=0；Lsp slot 恰1 且为 sync、原次序不变；四 builtin key 穷举；discover/status | slot/key/source 清单及计数；A03、T05、D04 |
| 22 | H-05/V-02/R17 | 【新增】`mcp::builtin::dispatch::tests::call_tool_uses_shared_result_mapping` | 真 handler 经 effective bridge 成功/Err/未知工具三形态；仍调用 `web::invoke_tool_call` 唯一助手 | success/error/invalid_params 计数、固定文本；D05 + 共享助手代码引用复核 |
| 23 | V-06/S-05/R20 | 【新增】`mcp::builtin::dispatch::tests::call_tool_error_text_is_fixed_and_redacted` | Cron 两类、LSP 六类实际错误均映射固定文本；业务细节不进输出，未知工具另测 | 8 行 variant→固定文本、matched=8、泄露计数=0；D06 + §7 退化表 |
| 24 | S-02/V-02/R21 | 【新增】`meta_harness::tests::cron_tools_are_not_middleware_static_tools`、`host::mcp_v4_wave2::search_execution_uses_effective_names` | cron 三工具静态表命中=0，builtin registry 命中=3；四 effective 搜索/执行均可达 | static=0/registry=3/search=4/execute=4；T06、H11 |

A29 补齐定位：R23→10；R24→2/4/5；R25→7/13/15；R26→1/15；R27→17；R28→3/16/18 及逐条台账。R1–R22 的“不改”项也须给实际测试/代码引用，不能因覆盖登记写“不改”便跳过。

## 4. 端到端夹具与可判定程序

### 4.1 cron、tick join 与单驱动（行 6/7）

| 用例 / 文件 | 依赖与动作 | 观察量 / 阈值 |
| --- | --- | --- |
| `host::mcp_v4_wave2::cron_register_tick_approval_continuation`；host wave2 文件 | 复用 wire/model，接真实 cron builtin、同一 scheduler/`CronSchedulerPort::subscribe`、`SessionCronBridge`、`run_cron_continuation_scheduler`、`approve_scheduled_trigger`、队列及真实 `dispatch_prompt_turn`；broker 替身只决定批准/拒绝 | 注册审批与触发审批分开计数。注册 approve=1，随后触发 approve=1；任务 ID 一致；queue push=1、source=`MessageSource::CronTrigger`、kind=`Defer`；continuation=true 派发=1，模型消费提醒=1 |
| 同函数负例 | 两个独立场景：拒绝注册；注册成功后拒绝触发。关闭时清理所有 fixture task | 拒绝注册：register handler=0、task=0；拒绝触发：事件=1、触发审批=1、queue=0、dispatch=0。不能直接调用 enqueue/helper 冒充整链路 |
| `mcp::builtin::cron::tests::cron_server_maps_three_tools_without_tick` / `tick_shutdown_joins_task_and_stops_triggers` / `tick_reconnect_has_single_driver_per_interval`；cron_test 文件 | 真 handler；tick guard 不在 `CronMcpServer`，由 `BuiltinInstanceSupervisor` 持有；`tick_enabled=true/false` 两态；生产 interval=1s，不改为测试短 interval。注入可控 scheduler 时钟以使一个任务到期，驱动仍走真实 tick loop | true 活跃 supervisor driver=1，false=0；跨过首次立即 tick 后按边界统计 3 个完整 interval，各 tick=1。关闭后 >2×interval 无新 trigger 且 `tick_is_finished()==true`；不能以“只触发一个任务”证明无第二 driver，TUI 私有 scheduler/tick 不计入本断言 |
| `host::mcp_v4_wave2::reconnect_has_single_tick_driver`；host wave2 文件 | host 真实 reconnect；计数 `BuiltinInstanceSupervisor` 的 driver spawn/exit、tick、触发、join 完成顺序 | old_join_seq < new_spawn_seq；同 scheduler max_active=1；关闭后观察 3×1s（严格 >2×interval），tick/trigger 增量=0、`tick_is_finished()==true` 且 join 完成；新代每个 interval 恰好 1 tick；只计 ACP 共享 scheduler 驱动任务，不以全仓 grep 零 tick 判定 |

异步证据用 channel/barrier 与有界 await，不用 sleep 猜成功。fixture 默认单步期限 5s；资源关闭期限记为 `BUILTIN_CONVERGE_TIMEOUT` 或 owner 冻结的 shutdown deadline，超时失败并打印 phase；真实 cron 表达式是分钟粒度，需受控时钟 seam，不可把 tick interval 当任务周期。若没有该 seam，先报 BLOCKED，不绕过 tick。TUI 私有 scheduler 不在“同一 scheduler 单驱动”结论内（A15）。

### 4.2 Write/Edit → language server（行 8）

| 文件 / 函数 | 夹具与程序 | 阈值与证据 |
| --- | --- | --- |
| `peri-middlewares/src/lsp/middleware_test.rs` / `lsp::middleware::tests::write_sync_orders_change_then_save` | 新薄中间件+计数 `LspPoolPort` 替身，文件为临时合成内容；`LspSyncMiddleware` 使用装配期注入 cwd；先调用 `ready_for(path)`，仅为 true 时读文件；按 Write/Edit 分组。 | 每调用 `ready_for`=1、read=1、didChange=1、didSave=1；同 path、正确最终内容与顺序；change typed Err 后仍尝试 save；save typed Err 也降级；两种错误工具原结果均不变；替身必须以计数/顺序断言，不能用“未报错”代替 |
| host wave2 文件 / `host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result` | 真 Write/Edit、真 `LspSyncMiddleware`、真 pool/protocol；fake language server 先 initialize/didOpen，然后记录 JSON-RPC 通知；stdout 只走协议，日志另存临时文件 | Write 与 Edit 各跑一个独立文件：ready_for=1、read=1、change=1→save=1；接收内容匹配磁盘；原 success 文本逐字相同。错误降级用端口替身计数旁证，不谎称无响应的 LSP notification 会返回业务错误 |

fake server 不可用时端口替身只能给端口边界 PASS，language-server 到达子项记 PARTIAL；关闭同步时需 read=0、change/save=0，而 LSP 工具仍可调用。不同文件并发/同文件顺序若未冻结，按文末 C6 处理，不自行扩充业务语义。

### 4.3 readiness / LSP 门控（行 2/4/5）

host wave2 的 `wave2_ready_gate_matrix` 与 `lsp_handler_constructed_after_config_merge` 复用真实 loader/assembly/builtin transport；协议失败注入在 cron/lsp 对应实例，不拿外部 stdio fatal 替代。分别对两实例跑成功、spawn/context 失败、协议失败、tools-list 失败/超时；未完成时阻塞 1R（模型=0），失败后 fatal、ACP internal 投影、ready 发布=0，成功后第一模型调用才发生。
`mcp::builtin::lsp::tests::list_tools_follows_configured_server_set` 用不存在的 server executable 证明“不需进程 ready”：空配置 tools=0、非空 tools=1，进程数均0，两个 enabled 实例仍 ready。合并配置先于构造，构造先于 initialize；同代修改配置 tools 不变。上下文重复注入 typed Err、首次对象不被替换；缺上下文失败不 fallback。

### 4.4 transport/task/state 隔离与 off（行 15）

| 文件 / 函数 | 夹具与动作 | 判定阈值 |
| --- | --- | --- |
| `peri-middlewares/tests/mcp_isolation_contract.rs` / `mcp_isolation_contract::instances_have_independent_transport_task_and_state` | 既有两台 stdio fixture 回归仍执行，但 builtin 注入关闭，不能作为 builtin 隔离证据；另走真实 cron/lsp builtin handler、独立 in-process transport/tap、公开 assembly 注入计数 scheduler/pool；注册 cron、调用 fake LSP、分别物理 close/reconnect | builtin 路径断言两 transport identity 不同；两个 handler task 生命周期独立；cron call 不增 LSP handler call；cron 状态增1 不改 LSP server 状态；LSP 操作不改 cron task 集合；A close 后 B 真调用成功。stdio 路径只判 wire 回归，不能替代上述断言；先忽略握手/后台已排定通知，只统计关联请求 ID 的调用 |
| host wave2 / `lifecycle_state_matrix` | 观测 §5 生命周期表：重连前后 scheduler/pool 身份、task 代际、join、server 存活；显式 shutdown | 除被关闭实例外计数不变；reconnect 状态保留、代际变化、旧 join 完成；host shutdown task/server/orphan=0 |
| host wave2 / `context_injected_before_initialize_and_rejects_duplicate` | `peri-acp/src/host/assemble.rs` 装配真实实例上下文：cwd、`Arc<CronScheduler>`、LSP pool、`tick_enabled`、关闭集；在 `run_initialize` 前记录序号，再尝试相同对象重复注入 | `inject_seq < initialize_seq`；首次注入成功；重复注入返回 typed `Err`，首个上下文身份保留；`assembly/mcp.rs` 不计入注入调用 |
| host wave2 / `off_has_zero_builtin_injection` | 临时 HOME + env 守卫/串行锁；off 测试独立于启用场景；保留 `wire_fixture` 用户 MCP | `PERI_MCP_BUILTIN=off`：web/artifact/cron/lsp 注入=0、builtin handler/tick=0；直连/搜索/执行目录无 builtin，旧 middleware 不回退；外部 MCP 可调用=1；name-policy parity 不因 off 改变 |

Arc 不同只能辅助，不能单独证明 task/state/transport 隔离；公开 API 不足时 crate 内 tap 只能补证，集成行仍 PARTIAL/BLOCKED，先登记 seam，不私自扩大生产可见性。capability root/凭据隔离沿主计划 §10.6 保持 UNVERIFIED。

### 4.5 multi-cwd 与 shutdown（行 13，A22）

`host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown`（host wave2 文件，新增）依赖真 host、两 session cwd A/B、两个有可等待退出句柄的 fake language server。单 cwd 将迁移前 `create_session_lsp_pool` 根 URI 规则与迁移后 initialize.rootUri 逐字对照；多 cwd 实测两 session 均为 host rootUri，第二 session 不得被标“等价”。删除 session 后 server 仍存活且 pool 身份不变；host shutdown 后在冻结 deadline 内收到 shutdown/exit、wait/reap 完成，存活子进程=0、未 join task=0、orphan=0。只看 Drop/Arc 引用计数或 PID 不存在不够；保留启动 PID 与 wait 结果关联。同步/协议超时也须验证有界强制收敛；没有宿主关闭 deadline 则 BLOCKED，不自行写“有界”。

## 5. A24 关闭五维与生命周期真值表

下表均假定两实例正常启动、LSP 已配置且 ready、tick_enabled=true。四类关闭必须分别观察：MCP 配置 `disabled:true` 看连接/ready/实例 handler；MetaHarness `policy_key=false` 看本 turn 投影但保留 handler/pool/readiness；`PERI_MCP_BUILTIN=off` 看零注入；物理 close 看 supervisor/task/server 收敛。只有策略关闭适用“保留 handler/pool/readiness”。工具关闭检查 required/deferred/subagent parent_tools/workflow 四面，并用编造的 effective 调用证明 handler 不被触达。

| 场景 | 工具可见性 | 同步 read/change/save | tick | readiness | 物理生命周期 / 观察面 |
| --- | --- | --- | --- | --- | --- |
| 全开 | cron=3、lsp=1，均 deferred | 每次 1/1/1 | 1 driver | 两实例 ready | handler/tick/pool 保留 |
| MetaHarness `CronMiddleware=false` | cron=0，lsp=1 | 1/1/1 | 仍1 | 不变 | **策略关闭专属**：handler/pool/readiness 保留；既有注册任务仍 tick |
| MetaHarness `LspMiddleware=false`、`LspSyncMiddleware=true` | cron=3，lsp=0 | 0/0/0 | 仍1 | 不变 | **策略关闭专属**：handler/pool/readiness 保留，不读文件 |
| MetaHarness `LspMiddleware=true`、`LspSyncMiddleware=false` | cron=3，lsp=1，调用成功 | 0/0/0 | 仍1 | 不变 | **策略关闭专属**：handler/pool/readiness 保留 |
| MetaHarness 两个 LSP 策略均 false | 两者0；外部工具不变 | 0/0/0 | 仍1 | 不变 | **策略关闭专属**：handler/pool/readiness 保留，不物理销毁 |
| MCP 配置 `disabled:true`（cron 或 lsp） | 对应工具0 | 只观察实际同步目标：目标不存在则 0/0/0 | 对应实例0 | 对应实例不连接、不加入其 system ready 依赖 | 连接/handler 不构造；单列组合根 scheduler/pool 是否保留，不能套用策略关闭结论 |
| `PERI_MCP_BUILTIN=off` | 四 builtin 均0 | 同步调用 0/0/0 | builtin driver=0 | 无 builtin ready 依赖 | **零注入**：builtin transport/handler=0；外部 stdio 仍可调用；不推断组合根对象不存在 |
| 物理 `instance close` | 对应工具0 | 对应同步目标 0/0/0 | 对应 tick=0 | 对应实例不再 ready | 观察 `supervisor.close` 的 tick cancel+join、server task 收敛；B 实例不受影响，scheduler/pool 不因 Arc 释放关闭 |

LSP 两键 4 格分别再乘 Cron 开/关，执行 8 个策略组合；每格打印五维及 read 计数。上述单项/全关、disabled、off、物理 close 观察不能互相替代。

| 物理操作 | 应保留 | 应关闭 / 阈值 |
| --- | --- | --- |
| instance close | 组合根 scheduler/pool 与注册状态 | `close_builtin_task(s)` 经对应 `BuiltinInstanceSupervisor::close(...).await`，先 tick cancel+join 再收敛 transport/server task；B 实例不受影响；不能把策略关闭套进这一行 |
| reconnect | 同一 scheduler/pool/context 与任务数据 | 旧 supervisor 的 tick/server task join 后才新代；old_join_seq < new_spawn_seq，max_active=1 |
| host shutdown | 仅关闭证据 | 两实例 transport/supervisor（tick 先 join）、host LSP pool、全部 server 有界回收，orphan=0 |

## 6. 串行命令台账与 W0–W4 闸门

以下均为**实施阶段命令，本文撰写未执行**。工作目录固定为目标 worktree；各条前台串行，无并发 cargo。对单函数采用完整名称+`--exact --nocapture --test-threads=1`；模块回归以 `::` 结尾防止命中同前缀 `_baseline`/spike。特别注意：`host::mcp_v4_wave2` 会命中已存在的 `host::mcp_v4_wave2_baseline`，因此终态验收必须使用具名函数的 `--exact`，不能以模块过滤代替。
预期类型：**E1**=`running 1 test`、具名 `... ok`、`test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; <实际值> filtered out; finished in <实际值>`；**EN** 同形但 N≥1，须核对目标函数清单；**B2** 为恰2个 baseline。尖括号只描述格式，acceptance 必须填原文，不能照抄模板。
每条保留真实 exit；验收记录必须写**新增/修改函数名、实际命中名单、exit、完整 `test result:`**。目标命令 `0 tests`、只命中旧用例、ignored、缺具名函数、非零 exit 均 BLOCKED。全量 lib 中天生无测试的 binary 也逐条登记 `0 tests`（该条失败、不算通过），若主计划要求四命令全部绿因此受阻，报告裁决缺口，不能静默豁免。

| ID / 闸门 / 预期 | 精确命令 |
| --- | --- |
| B01 / W0、W3、W4 / B2 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2_baseline --nocapture` |
| T01 / W1 / E1 | `cargo test -p peri-acp-types --lib -- builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance --exact --nocapture --test-threads=1` |
| C01 / W1 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::cron_server_maps_three_tools_without_tick --exact --nocapture --test-threads=1`（A32 后 tick 不在 handler：本行的可观察量由「handler 无 tick 驱动」承担，原计划名 `cron_server_spawns_one_tick_guard` 已作废） |
| C02 / W1、W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers --exact --nocapture --test-threads=1` |
| C03 / W1 / E1 | `cargo test -p peri-middlewares --lib -- cron::tests::test_tick_fires_trigger --exact --nocapture --test-threads=1` |
| L01 / W1 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::lsp::tests::list_tools_follows_configured_server_set --exact --nocapture --test-threads=1` |
| L02 / W1 / E1 | `cargo test -p peri-lsp --lib -- pool::tests::port_did_change_and_did_save_route_paths --exact --nocapture --test-threads=1` |
| L03 / W1 / E1 | `cargo test -p peri-middlewares --lib -- lsp::middleware::tests::write_sync_orders_change_then_save --exact --nocapture --test-threads=1` |
| L04 / W1 / E1 | `cargo test -p peri-acp --lib -- host::requests::tests::lsp_sync_mock_counts --exact --nocapture --test-threads=1` |
| L05 / W1 / E1 | `cargo test -p peri-acp --lib -- host::stdio::run_server_integration_tests::lsp_sync_stdio_counts --exact --nocapture --test-threads=1` |
| F02 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges --exact --nocapture --test-threads=1` |
| W01 / W1、W2 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::web::tests::web_handler_factory_covers_implemented_instances_only --exact --nocapture --test-threads=1` |
| W02 / W1 / EN | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests:: --nocapture --test-threads=1` |
| W03 / W1 / EN | `cargo test -p peri-middlewares --lib -- mcp::builtin::cron::tests:: --nocapture --test-threads=1` |
| W04 / W1 / EN | `cargo test -p peri-middlewares --lib -- mcp::builtin::lsp::tests:: --nocapture --test-threads=1` |
| W05 / W1 / EN | `cargo test -p peri-middlewares --lib -- mcp::builtin::runtime::tests:: --nocapture --test-threads=1` |
| W06 / W1 / EN | `cargo test -p peri-middlewares --lib -- cron::tests:: --nocapture --test-threads=1` |
| M01 / W2、W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin_apply_tests::wave2_overlay_contract --exact --nocapture --test-threads=1` |
| M02 / W2、W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::production_startup_path_connects_both_builtin_instances --exact --nocapture --test-threads=1` |
| M03 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::startup_gate_stages_declared_direct_tools_and_required_set --exact --nocapture --test-threads=1` |
| M04 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime_tests::policy_close_five_dimensions --exact --nocapture --test-threads=1` |
| M05 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests::builtin_prompt_declaration_matches_registry --exact --nocapture --test-threads=1` |
| M06 / W3 / E1 | `cargo test -p peri-middlewares --lib -- tool_search::declaration::tests::wave2_has_no_prompt_declaration --exact --nocapture --test-threads=1` |
| M07 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests::effective_tool_names_covers_registry --exact --nocapture --test-threads=1` |
| M08 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests::overlay_inserts_complete_default_entries --exact --nocapture --test-threads=1` |
| A01 / W2 / E1 | `cargo test -p peri-middlewares --lib -- assembly::tests::lsp_pool_port_injected_registers_middleware --exact --nocapture --test-threads=1` |
| A02 / W2 / E1 | `cargo test -p peri-middlewares --lib -- assembly::tests::middleware_names_match_production_blueprint --exact --nocapture --test-threads=1` |
| A03 / W2 / E1 | `cargo test -p peri-middlewares --lib -- assembly::tests::production_chain_has_only_lsp_sync_slot --exact --nocapture --test-threads=1` |
| A04 / W2 / EN | `cargo test -p peri-middlewares --lib -- assembly::tests:: --nocapture --test-threads=1` |
| S01 / W2 / E1 | `cargo test -p peri-middlewares --lib -- permission::tests::builtin_effective_names_match_original_name_policy --exact --nocapture --test-threads=1` |
| S02 / W2 / E1 | `cargo test -p peri-middlewares --lib -- subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names --exact --nocapture --test-threads=1` |
| S03 / W2 / EN | 新增 Cron/LSP 注册表条目后的既有归一入口回归：`peri-tui/src/kit/tool_display.rs`；`cargo test -p peri-tui --lib -- kit::tool_display::tests:: --nocapture --test-threads=1` |
| S04 / W2 / EN | 新增 Cron/LSP 注册表条目后的既有归一入口回归：`peri-tui/src/truncate.rs`；`cargo test -p peri-tui --lib -- truncate::tests:: --nocapture --test-threads=1` |
| T02 / W2 / E1 | `cargo test -p peri-acp-types --lib -- meta_harness::tests::builtin_instance_policy_keys_match_declaration_table --exact --nocapture --test-threads=1` |
| T03 / W2 / E1 | `cargo test -p peri-acp-types --lib -- meta_harness::tests::middleware_names_and_builtin_policy_keys_are_disjoint --exact --nocapture --test-threads=1` |
| T04 / W2 / E1 | `cargo test -p peri-acp-types --lib -- meta_harness::tests::known_key_union_covers_builtin_policy_keys --exact --nocapture --test-threads=1` |
| T05 / W2 / E1 | `cargo test -p peri-acp-types --lib -- meta_harness::tests::known_builtin_keys_are_exhaustive --exact --nocapture --test-threads=1` |
| T06 / W2 / E1 | `cargo test -p peri-acp-types --lib -- meta_harness::tests::cron_tools_are_not_middleware_static_tools --exact --nocapture --test-threads=1` |
| H00 / W2 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2_baseline::wave2_baseline_lsp_tool_visible_when_server_configured --exact --nocapture --test-threads=1` |
| H01 / W2、W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave2_ready_gate_matrix --exact --nocapture --test-threads=1` |
| H02 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary --exact --nocapture --test-threads=1` |
| H03 / W2、W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_handler_constructed_after_config_merge --exact --nocapture --test-threads=1` |
| H04 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::cron_register_tick_approval_continuation --exact --nocapture --test-threads=1` |
| H05 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::reconnect_has_single_tick_driver --exact --nocapture --test-threads=1` |
| H06 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_sync_does_not_change_tool_result --exact --nocapture --test-threads=1` |
| H07 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_sync_close_cross_matrix --exact --nocapture --test-threads=1` |
| H08 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown --exact --nocapture --test-threads=1` |
| H09 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lifecycle_state_matrix --exact --nocapture --test-threads=1` |
| H10 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::off_has_zero_builtin_injection --exact --nocapture --test-threads=1` |
| H11 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::search_execution_uses_effective_names --exact --nocapture --test-threads=1` |
| H12 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::wave1_compatibility_with_wave2 --exact --nocapture --test-threads=1` |
| H13 / W2、W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::context_injected_before_initialize_and_rejects_duplicate --exact --nocapture --test-threads=1` |
| H14 / W3 / E1 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_bridge_timeout_cancels_and_converges --exact --nocapture --test-threads=1` |
| I01 / W3 / E1 | `cargo test -p peri-middlewares --test mcp_isolation_contract`（目标名直接执行；不把 `mcp_isolation_contract::` 当过滤前缀） |
| D03 / W2、W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::all_registered_instances_have_handler --exact --nocapture --test-threads=1` |
| D04 / W2、W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::builtin_source_propagates --exact --nocapture --test-threads=1` |
| D05 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::call_tool_uses_shared_result_mapping --exact --nocapture --test-threads=1` |
| D06 / W3 / E1 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests::call_tool_error_text_is_fixed_and_redacted --exact --nocapture --test-threads=1` |
| R01 / W3 / EN | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture:: --nocapture --test-threads=1` |
| R02 / W3 / EN | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin:: --nocapture --test-threads=1` |
| R03 / W3 / EN | `cargo test -p peri-acp --lib -- host::mcp_v4_startup_tests:: --nocapture --test-threads=1` |
| R04 / W3 / EN | `cargo test -p peri-middlewares --test mcp_isolation_contract`（保留 stdio 回归，并在同一测试目标内新增真实 builtin 路径；不以 `mcp_isolation_contract::` 作为过滤前缀） |
| R05 / W3 / EN | `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --nocapture --test-threads=1` |
| G01 / W4 / 非测试 | `cargo build --workspace` |
| G02 / W4 / 非测试 | `cargo clippy --workspace --all-targets -- -D warnings` |
| G03 / W4 / 各 binary EN | `cargo test --workspace --lib` |
| G04 / W4 / 非测试 | `bash scripts/check-layer-imports.sh` |

W1 必须 C-01…04、L-01…03、H-01 完成且 W01–W06 非零；W2 必须 H-02…05、S-01…05、A/S/T/H 前置全绿，D02 已预审/落实；W3 必须 B/M/H/I/D/R 指定观察面全部闭合。W4 在同一最终 HEAD 重跑行级新/修改函数及 B01/I01，再执行 G01–G04，不能复用代码变更前绿色输出。
G01/G02 抄录 `Finished` 行、exit=0（不虚构 `test result:`）；G04 抄录规则总数与违规边=0。G03 按 binary 留完整结果、合计 passed/failed/ignored，不把 workspace 总成功掩盖零命中。D01=三件缺陷逐符号审阅；D02=`DOC-UPDATE-001` 人工清单，不存在 shell `test result:`，用核对项总数/完成数/残留数代替。

## 7. acceptance 写法、语义变更与判定

“三态”指三列事实：**目标归属 / 当前实现 / 本次运行时证据**；不是只有三种结果。每行另设 `PASS / PARTIAL / UNVERIFIED / BLOCKED` 四选一结论。优先级：已知失败或前置裁决未定→BLOCKED；已证部分→PARTIAL；完全无运行证据→UNVERIFIED；全部子断言有证据→PASS。

| 结论 | 必须满足 / 禁止事项 |
| --- | --- |
| PASS | 该行全部子断言达到阈值；新增/修改契约给新增/修改函数、完整命令、exit=0、非零 passed、原始计数；旧测试通过只算回归 |
| PARTIAL | 明列已证明与未证明的面，例如端口到达已证、language server 到达未证；不升级整条 E2E |
| UNVERIFIED | 无可安全观察 seam、未运行、仅静态阅读；说明如何证伪与 owner，不编造函数或数值 |
| BLOCKED | 0 tests、缺挂载、编译/断言失败、超时、环境缺 Node、前置闸门/裁决不满足；原失败保留，修复后另增证据 |

| acceptance 位置 | 写入内容 |
| --- | --- |
| §2 | 冻结；不改旧打印/状态/计数，不把后来的 HEAD 贴到旧记录 |
| §4 | A/B 三面；24 行引用 §5/§6 证据 ID；追加语义变更实测，不改裁决 |
| §5 | cron 两次审批分别计数、真实队列/continuation；LSP 接收；五维矩阵；multi-cwd/生命周期 |
| §6 | 每命令 cwd、HEAD、夹具版本、函数名、exit、完整 stdout 关键行/`test result:`、passed；原始日志路径与采集时间 |
| §7 | 未闭合项、三列事实与四态；D01/D02、最终门禁与不覆盖面；禁止总体一句“迁移完成”替代逐面结论 |

| 语义变更 | 必写字段 / 证据 |
| --- | --- |
| A20 | 迁移前裸名摘要、迁移后 effective 摘要缺席、搜索 XOR 分支变化；逐字证据块与语义解释分列 |
| A22 | 单 cwd URI 对照、多 cwd 共享 host root 的退化、session/delete 不关 pool、shutdown deadline/启动数/回收数/orphan；用户文档位置；wave 3 经 ToolContext 恢复 per-session 的依赖 |
| A24 / F8 | 策略关闭、MCP `disabled:true`、`PERI_MCP_BUILTIN=off`、物理 close 四类分列；8 个策略组合逐格计数；disabled 看连接/ready/handler，off 看零注入，物理 close 看 supervisor/task/server 收敛；仅策略关闭允许保留 handler/pool/readiness | `policy_close_five_dimensions`、`lifecycle_state_matrix`、`off_has_zero_builtin_injection`；H07/H09/H10；未裁决格按对应观察面 BLOCKED |
| A30 / F2 | `LspPoolPort` 的 `ready_for`、didChange→didSave、typed `LspSyncError`、无默认实现、装配期 cwd、错误继续 save；bridge `TOOL_CALL_TIMEOUT=120s` 与 `LspTool::timeout()==None` 的外层差异 | L02–L05、H06、H14；替身计数/顺序、延迟夹具超时/取消/收敛，不以未报错或单个错误返回 PASS |
| A32 | `BuiltinInstanceSupervisor` / `TickGuard{cancel, join}` 位于 `peri-middlewares/src/mcp/builtin/runtime.rs`；`close_builtin_task(s)` 位于 `peri-middlewares/src/mcp/client/lifecycle.rs`，先 tick cancel+join 再 server task；`CronMcpServer` 不持 tick | C02、H05；关闭后 >2×interval 无新触发且 `tick_is_finished()==true`，reconnect 每 interval 恰 1 tick |
| A33 | `peri-acp/src/host/assemble.rs` 装配 cwd / `Arc<CronScheduler>` / LSP pool / `tick_enabled` / 关闭集，早于 `run_initialize`；`assembly/mcp.rs` 不参与；重复注入 typed `Err` | H13；`inject_seq < initialize_seq`、首个上下文保留、重复注入失败 |
| F5 | `peri-tui/src/kit/tool_display.rs`、`peri-tui/src/truncate.rs` 已有归一入口；新增 Cron/LSP 注册表条目后只做回归断言，不新增归一改造 | S03/S04；按实际命中名单与 `test result:` 记录 |
| F6 | 既有 isolation stdio 回归关闭 builtin 注入，不能证明 builtin handler/tick/state 隔离；保留 stdio 回归并新增真实 builtin handler + in-process transport/tap 路径 | I01/R04；真实 builtin 隔离断言与 stdio 回归分列，命令目标为 `mcp_isolation_contract` |
| F10 | tick 验收只覆盖 ACP 共享 scheduler/MCP driver；`peri-tui/src/app/cron_state.rs`、`peri-tui/src/app/mod.rs` 的 TUI 私有 scheduler/tick 不在范围 | C01/C02/H05；禁止全仓 grep 零 tick 判定 |
| IF-D14/R20 | Cron `InvalidExpression`（“cron 表达式无效: {0}”）、`TaskLimitReached`（“已达到定时任务上限（{0}）”）；LSP `MissingParam`、`InvalidOperation`、`RequestFailed`、`NotReady`、`InvalidPosition`、`NoServerForExtension`，逐变体列旧模板与新固定文本及函数/D06 输出 |

固定文本事实源为 `peri-middlewares/src/mcp/builtin/web.rs::execution_failure_text`；不能把业务 Err 错记为 transport RPC error。D06 用合成无敏感数据触发八类真实错误，逐字比较该模板：`tool \`{tool}\` failed to execute; the failure detail is withheld by policy (no paths, environment values, or credentials are exposed). Verify the input and retry.` 旧模板取 `cron/mod.rs::CronError` 与 `lsp/tool.rs::LspToolError`，不抄真实失败参数。
D02 至少核对主计划 S-05 清单的 `docs/code-index/**`、`docs/reference/mcp-ecosystem.md`、`docs/design/middleware-system.md`、`peri-middlewares/src/skills/builtin/skills/cron/SKILL.md` 与模型可见工具调用文本；保留历史记录/判定表的合法裸名，模型可见调用改 effective name。敏感清单数量/顺序不降强度。

未闭合项模板（每项一份；仅追加新轮次，不覆盖旧失败）：
```text
编号 / 验收行 / R或A号 / owner / 状态：
目标归属：主计划原断言；当前实现：符号+路径+HEAD；本次证据：运行时间或未运行。
新增/修改函数完整名；命令原文；exit；完整 test result；逐字打印/计数：
已证子项 / 未证子项 / 失败或限制原文：
为何不能升级；需要裁决或 seam；下一步命令；解除阈值：
语义退化登记 / 用户文档位置 / 后续 wave 依赖：
```

## 8. 反假绿清单

| 假绿 | 防线 |
| --- | --- |
| `mcp::builtin` 命中 spike，`permission` 命中 auto_classifier，`host::mcp_v4_wave2` 命中 baseline | 单函数 `--exact`；模块结尾 `::`；核对逐字函数清单 |
| `_test.rs` 存在但没挂载、把文件名当 module、把 integration binary 当 module | 核对 `#[path]` 与 `mod`；精确命令 0 tests 即 BLOCKED；不得放宽 filter 绕过 |
| 非零旧测试当作新增契约闭合 | acceptance 列本波新增/修改函数与断言差异；R01–R05 只作回归 |
| 只 println 不 assert；只看注册表不实际调用 | 计数先断言；H04/H06/H11 真链路；打印格式不是证据结果 |
| 只跑 lib 不跑 tests/ | I01 与 R04/R05 独立运行；G03 不覆盖它们 |
| XOR 全绿但仍裸名旧实现 | H02 额外断言 effective=true、bare=false；H11 验 ExecuteExtraTool；不改变 baseline XOR |
| 搜索顺序/摘要 JSON 子串冒充能力证据 | 按名字成员与条目行解析；排名只保留现场，不作等价 |
| 只断言 ready 标志，不看初始化/list 与 1R | 分阶段事件序号+首模型计数；失败注入针对 cron/lsp |
| 只见一个 cron trigger 就称单 driver | tick/spawn/active/join 分开计数，观察 >2×interval 与新代窗口；不以 Drop 代 join |
| LSP 工具成功就称同步成功 | fake server 实际收到 change/save；端口替身只闭合端口边界；工具结果逐字对照 |
| no orphan 只看 Arc/PID | wait/reap、任务 join 与启动清单逐个关联，期限内归零 |
| 临时 HOME/env 串扰、真实 credentials 泄漏 | 复用串行守卫并恢复 env；无真实 secret。只记录合成 fixture 标识，不 dump env/headers |
| 日志管道吞 exit、手工改写输出、混用不同 HEAD | 保存真实 exit 与原始 stdout；HEAD/夹具版本逐命令登记；修复后追加新证据 |

## 9. 与代码冲突项 / 不可证伪项

| 编号 | 事实与影响 | 建议处置（不静默改裁决） |
| --- | --- | --- |
| C1 | 任务指定 `b1651aee`，现场 HEAD=`a81e0ba6`；撰写期间主计划 v2→v3 与其他 sub-plan 出现他方改动 | **NOTE（文档采信问题）**：以现场采信 HEAD 记录；本文件仍只改目标文件，不替他方 commit/回滚；最终 acceptance 按每命令 cwd、HEAD、夹具版本记录，不能把 HEAD 漂移当测试证据 |
| C2 | `host/mcp_v4_wave2_baseline_test.rs::wave2_baseline_lsp_tool_visible_when_server_configured` 先 `WireFixtureHarness::initialized`，后写 settings 并设置 `SessionContext.lsp_servers/lsp_pool`；H-04 要求 initialize 前 host 注入 | **部分闭合（A33/H13）**：注入时序由 `assemble.rs` 早于 initialize 的具名断言闭合；仍保留“同一夹具”字节不变与最小接线适配冲突，适配后的 A/B 必须登记 diff，不冒充原 blob，H00/B01 以实际命中与结果判定 |
| C3 | 主计划 §5 R12/R15/R17 的验收行索引与 §8 第20–22行文字错位；§8 第12行旧模块路径与实际挂载路径不一致 | **NOTE（文档索引问题）**：本 sub-plan 按实际模块路径重建 24 行映射和精确 filter；不创建假模块；主计划索引修订仍由 owner 处理 |
| C4 | §3 IF-P3-04 三态表把 instance close/策略关闭合成一行并列 handler task 销毁；策略关闭应保留 handler/tick | **已裁决（A32/F8）**：§5 已将策略关闭、disabled、off、物理 close 分列；只有 MetaHarness `policy_key=false` 保留 handler/pool/readiness，物理 close 才由 supervisor 收敛；M04/H07/H09 按对应观察面验收 |
| C5 | disabled/off 下 host pool 是否构造、同步是否绕过工具关闭；“独立状态”不能等同 capability root/凭据独立 | **部分闭合（F8/I01）**：§5 已冻结 disabled/off 的连接、同步、tick、ready、零注入观察面；capability root/凭据隔离仍按主计划 §10.6 标 `UNVERIFIED`，不得由 transport/state 断言升级 |
| C6 | `after_tool` 无 ToolContext；需冻结 cwd、读前 ready、change Err 后 save；host shutdown deadline 尚无本波运行事实 | **同步部分已裁决（A30/A33）**：端口形状、装配期 cwd、ready→read、change 失败仍 save 已落入第 8 行和 4.2；shutdown deadline 仍保留为运行前置，未有 deadline 只能 BLOCKED，不写“有界” |
| C7 | cron 任务分钟粒度；tokio tick 与 scheduler UTC 时钟不是同一时钟；单次触发不能证明 driver/join | **未闭合（C-02/H05）**：仍需受控时钟/计数 seam；按 A32 只验证 supervisor 的 spawn/cancel/join、`tick_is_finished()` 与每 interval 计数，不直接调用 trigger 绕过真实 tick；缺 seam 则 BLOCKED |
| C8 | §8 行14“缺陷仍存在”、行17文档、行18全门禁没有单一具名行为函数 | **证据类型已闭合（F3/F4）**：三行明确为静态登记、人工 `DOC-UPDATE-001`、命令门禁；分别记录符号/段落、核对项计数、实际命中名单/exit/`test result:`，不虚构 Rust 测试 |
| C9 | wave2 host/cron/lsp/dispatch 新函数与 isolation 函数当前未落地；旧 startup/runtime 测试不具备 cron/lsp 证据 | **仍未闭合（W1–W3/I01）**：本次仅修验证口径；必须先挂载真实 builtin isolation 函数，再执行具名 H/M/D/I 命令；I01 使用 integration target 名 `mcp_isolation_contract`，不能用旧用例或 `0 tests` 闭合 |
| C10 | W0 已存打印有截断/占位，probe 输出措辞与 §2 原文有差异；`max_results=50` 不总等于全索引 | **NOTE（历史证据问题）**：保留 §2 原文和限制；后续 acceptance 记录采集 HEAD/夹具、实际命中名单、exit、完整 `test result:`；缺失计数仍只能 PARTIAL，不能追认逐字完整 |

## 10. 施工顺序 + 提交切分

| 次序 | 工作 / 验证 | 提交建议（未来获准后执行） |
| --- | --- | --- |
| 1 W0 保全 | 确认现场与 C1/C2；冻结 acceptance §2；解决观测/时序裁决，保存 A 证据 | 夹具与挂载独立一步；原 baseline 最小适配与理由可审，不改生产以迁就测试 |
| 2 W1 | C/L/H owner 完成行为、dispatch、tick 生命周期及端口；执行 T01/C/L/W 闸门 | 行为断言一步；不与验收文本混写，不留未挂载“测试” |
| 3 W2 | H/S owner 完成装配、归一、槽位、文档；执行 A/S/T/M 与 H 前置 | 装配断言与 host seam 一步；共享文件仅 owner 写，V 提交需求/复核 |
| 4 W3 | V-02/03/04 执行 B/M/H/I/D/R；新合同逐行闭合，失败保留 | E2E/隔离/五维断言一步；夹具、断言、生产改动可分别审阅 |
| 5 W4 | V-05 复核 S-05/DOC-UPDATE-001；V-06 最终 HEAD 行级重跑和四门禁；追加 acceptance §4–§7 | 验收记录单独一步，包含逐字输出、退化与未闭合项；文档实现由 S-05 独立提交 |

本次交付只有本文；不创建夹具、不执行上述命令、不改 acceptance、不 commit。后续某闸门不满足就停在该闸门并登记，不能以收口文档的完整性替代实现完成。
