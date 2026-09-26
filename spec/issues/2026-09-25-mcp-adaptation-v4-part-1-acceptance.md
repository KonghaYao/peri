# MCP adaptation v4-part-1 — 现场验收记录

**状态**：PARTIAL（契约 1–4、7 通过；契约 5、6 按能力分级的降级证据，见 §3、§4）
**优先级**：高
**类型**：验收记录 / MCP 启动准入与一等工具注入
**创建日期**：2026-09-25
**最后核查**：2026-09-26（task D-05，W6；§5.1 为独立验证阶段的收口复跑）
**事实源**：`docs/design/mcp-adaptation-v4-part-1.md`（契约语义）、`spec/issues/2026-09-25-mcp-adaptation-v4-part-1-plan.md`（批次与接口冻结）
**范围**：只记录契约 1–4、7 的落地与契约 5、6 的分级证据；不实现生产代码，不回填设计文档。

## 1. 口径

本记录使用三态列，任何一格都不得跨态引用：

- **目标归属**：设计文档的 v4 目标划分（「完全下放 / 部分下放 / 宿主保留」）。**不是**实现状态。
- **当前实现**：代码与契约测试可核对的现状（含未迁移项）。
- **本次运行时证据**：本次现场执行的命令、用例数、exit status 与承担该断言的测试名。无命令则写 `无`，不得用代码阅读替代。

裁决规则（沿用 plan §8/§9）：0 tests 视为失败；绿色局部单测不升级为整体迁移结论；契约 5/6 只给降级证据时必须保持 `PARTIAL`。

## 2. 契约矩阵（1–7）

| 契约 | 目标归属（设计） | 当前实现 | 本次运行时证据 | 裁决 |
| --- | --- | --- | --- | --- |
| 1 配置解析拒绝「无 `system_mcp = true` 却声明 `system_mcp_tools`」 | 配置层契约，不涉及迁移 | `McpServerConfig` 手写 `Deserialize` 解析即校验（`peri-acp-types/src/plugin.rs:202`、`:220`）；`validate` 同一规则源另在 `mcp::config` 的 direct、global、merged 三个入口、`plugin::loader::load_enabled_plugins_for_mcp`（MCP 专用严格入口）、`TransportConfig::try_from` 复检 | `-- system_mcp` exit 0 / 8 passed（`test_system_mcp_tools_requires_true`、`test_system_mcp_key_aliases`、`test_system_mcp_rejects_null_and_wrong_types`、`test_system_mcp_empty_tools_roundtrip`、`test_system_mcp_timeout_requires_true` 等，plugin.rs 共 8 个测试全部命中）；`mcp::config::tests` exit 0 / 45 passed（`test_system_mcp_project_rejects_tools_without_true`、`test_system_mcp_global_rejects_invalid_maps`、`test_system_mcp_merged_errors_are_not_empty_success`、`test_system_mcp_plugin_strict_error_reaches_merge`、`test_system_mcp_typed_validation_includes_disabled`）；`plugin::loader` exit 0 / 58 passed（严格入参 + 宽容聚合各一条）；`mcp::transport` exit 0 / 10 passed；`-- test_dynamic_mcp_rejects_system_mcp_fields` exit 0 / 1 passed | **PASS** |
| 2 未完成 transport / initialize / 协商 / 必需工具检查前不得进入可启动 react loop；失败或 timeout 返回错误 | `McpMiddleware` 在 1R 等待（v4 目标） | 新增启动闸门 hook `before_react_start`（`peri-agent/src/middleware/trait.rs:87`），调用点位于首批 `before_agent` 之后、Compact 之前（`peri-agent/src/agent/stages/mod.rs:902-909`）；失败/超时→`LoopResult::Error`，`Interrupted`→`LoopResult::Interrupted`；`McpMiddleware::before_react_start` 在 `peri-middlewares/src/mcp/middleware.rs:703` | `host::mcp_v4_startup_tests` exit 0 / 11 passed：真实子进程 + 真实 rmcp stdio + counting model，`system_mcp_transport_failure_fails_first_prompt_without_model_call`、`system_mcp_connected_without_tool_discovery_is_not_ready`、`system_mcp_timeout_is_fatal_not_cancelled`、`system_mcp_disconnected_peer_fails_first_prompt`、`system_mcp_missing_required_tool_fails_before_reason` 均断言模型调用 0 次、`ExecutionFailureKind::Internal`、`TurnEnded(Error)`、ACP 投影 `-32000`+`kind=internal`；`system_mcp_gate_runs_after_receive_and_before_reason` 固定闸门位置；`ordinary_mcp_pending_does_not_block_startup` / `ordinary_mcp_failure_does_not_block_startup` 反向对照。`mcp::middleware` exit 0 / 40 passed | **PASS** |
| 3 `system_mcp_tools` 经所属 namespace 解析、schema 可构造 bridge、直接进入 RCRA 工具列表；普通 deferred 工具仍走 ToolSearch | direct 注入不经 ToolSearch（v4 目标） | `system_tools.rs` 解析/提升（`prepare_system_tools` + `McpToolBridge::with_direct`）；候选经 `StartupState` 提交，`tool_catalog.replace_static_mcp_tools` 原子替换 static base（`peri-agent/src/session/tool_catalog.rs:260`） | `host::mcp_v4_startup_tests` exit 0 / 11 passed：`system_mcp_ready_exposes_required_tools_on_first_model_request` 断言**首个 LLM 请求** `tools` 含 `mcp__sys__echo`、不含同 server 非必需 `mcp__sys__glob`、普通 MCP 工具 `mcp__ord__ping` 仍在 deferred 摘要；`mcp::system_tools` exit 0 / 17 passed；`mcp::mcp_v4_seam` exit 0 / 6 passed（namespace 净化、原始名匹配、跨 server 不解析、schema 失败零提升）；`mcp::tool_bridge` exit 0 / 14 passed；`tool_search` exit 0 / 65 passed | **PASS** |
| 4 必需工具为空数组只验证 ready，不注入额外工具 | 同上 | `Some([])` 与 `None` 可区分（`empty_config`/roundtrip 测试）；空数组不提升任何工具 | 两条分支分别有独立断言：`system_mcp_empty_required_tools_ready_without_injection`（host，exit 0 / 11 passed 中）与 `test_system_empty_required_array_adds_no_direct_tools`（`mcp::system_tools` 17 passed）；「`tools/list` 失败 ≠ 空列表」由 `system_mcp_tool_discovery_failure_is_not_an_empty_tool_list` 与 `initialize.rs` 消除 4 处 `unwrap_or_default()` 支撑 | **PASS** |
| 5 五个目标 MCP 的 transport / 状态 / 凭据 / capability root / client pool 不共享；无隐式跨 MCP 调用 | 「5 个彼此隔离的 MCP 实例」 | **未迁移**：Workspace / Artifact / Web / Cron / LSP 五个生产实例不存在；`McpClientPool` 为 pool-wide `capability_profile`，`McpClientHandle` 无 credential 字段 | `--test mcp_isolation_contract -- --test-threads=1` exit 0 / 3 passed：两个真实 stdio 子进程（不同 pid、各自 wire 日志）、不同 `Arc<McpClientHandle>`、不同 pool entry、namespace 路由、A 的 wire 无 B 标识、关闭 A 不影响 B | **PARTIAL**（见 §3） |
| 6 Permission / HITL / Hook / SubAgent / Workflow / Goal / PTC 不得被绕过 | 「不经这 5 个 MCP」= 宿主保留 | direct 注入只改工具可见性，工具调用仍经 middleware chain 与 Permission/HITL | `--test mcp_host_policy_contract -- --test-threads=1` exit 0 / 5 passed（真实 rmcp service + duplex wire 上的 `tools/call` 事实）；`host::mcp_v4_startup_tests` exit 0 / 11 passed（session/事件/ACP 投影/装配层） | **PARTIAL**（逐能力见 §4） |
| 7 未完成迁移前必须区分「目标归属」与「已落地能力」 | 文档纪律 | 本记录三态列；`docs/reference/mcp-ecosystem.md` §9.2 只写配置层 key 与拒绝规则，运行时语义回指设计文档；设计文档未被回填批次/勾选（mtime 2026-09-25 21:54，早于本次实施写入窗口 23:29–00:54） | `git diff --check` exit 0；`mcp-ecosystem.md#` 无外部锚点引用需要同步（全仓 grep 0 命中） | **PASS** |

## 3. 契约 5 的 UNVERIFIED 项（硬要求）

以下三项**没有本次运行时证据**，不得由 D-03 的绿色推断：

| 子项 | 状态 | 原因（代码事实） |
| --- | --- | --- |
| **凭据隔离** | **UNVERIFIED** | `McpClientHandle` 无 credential 字段，pool 的 credential store 由 `initialize` 内 `FileCredentialStore::new()` 建立，没有可安全读取或比较的 per-instance identity public API。本记录与 D-03 都**未**断言、也无法断言两台 server 的凭据不共享；不打印、不比较任何凭据值，测试不含真实 secret。 |
| **capability root 隔离** | **UNVERIFIED** | `McpClientPool::capability_profile` 是 pool-wide 且非 public，`McpConnectionKey` 非 public；D-03 作为外部集成测试读不到二者，因此**未**断言 capability root 不共享。`local-mcp-server` 的 `RootDir` 也明确不是安全沙箱。 |
| **五个目标 MCP 的实例落地** | **未落地（目标归属 ≠ 已落地）** | 无任何可枚举 Workspace / Artifact / Web / Cron / LSP 五实例的事实接口；D-03 只覆盖两台最小 fixture 的「已落地连接局部隔离」。 |

D-03 实际覆盖（PARTIAL 的正面部分）：pool entry 分离、`Arc<McpClientHandle>` 分离、真实 transport wire 分离（两个独立子进程 pid）、namespace 路由与 wire 裸名、无隐式跨实例调用、关闭一台不影响另一台。**不覆盖**：凭据、capability root、五实例迁移、跨进程/跨机器隔离。

## 4. 契约 6 的逐能力分级

| 能力 | 本层证据 | 承担者 | 裁决 |
| --- | --- | --- | --- |
| Permission + HITL（approve） | 审批只看 `(name, input)`；`default_requires_approval` 对 `mcp__` 前缀生效；批准后 wire 恰好一次 `tools/call` | D-04 `hitl_approval_gates_mcp_bridge_by_effective_name_and_calls_server_once` | **PASS**（本层） |
| Permission + HITL（reject） | broker 收到 effective name → `EffectiveToolErrorCode::UserRejected`，wire 零调用 | D-04 `hitl_rejection_never_reaches_the_mcp_server` | **PASS**（本层） |
| effective tool name | 策略/审批见 `mcp__{server}__{tool}`，wire 见裸工具名 | D-04 上述两例 + `deferred_*` | **PASS**（本层） |
| cancel | 在飞 `tools/call` 取消 → `Interrupted`，不重放、无第二次 wire 请求 | D-04 `in_flight_cancellation_ends_the_call_without_a_second_wire_request` | **PASS**（本层） |
| ToolSearch deferral 未被绕过 | deferred bridge 不进 `direct_definitions`（与 Reason 同一 `is_direct() && visible_to_model()` 谓词），可被 `SearchExtraTools` 检索、经 `ExecuteExtraTool` 到达 wire | D-04 `deferred_mcp_bridge_is_reachable_only_through_tool_search` + `tool_search_meta_tool_names_match_the_middleware_registration` | **PASS**（本层） |
| session / 事件 / ACP 投影 / 宿主装配 | render 事件属同一 turn（D-04）；真实 `run_session_loop` 的 `AgentExecutionFailed → TurnEnded(Error) → done`、failure 归属 `McpMiddleware`、ACP `-32000`+`kind=internal`（B-07） | D-04 + `host::mcp_v4_startup_tests` / `host::prompt::tests` | **PARTIAL→强**（装配层在 B-07） |
| Hook | 仅证明调用**未短路链**：`before_tools_batch` 对 MCP bridge 可见。Hook 自身实现未在本层重测 | D-04 `assert_policy_saw` 断言 | **PARTIAL** |
| **SubAgent / Workflow / Goal / PTC** | **本层未验证**：不把 MCP 工具伪装成这些能力，也未重测其实现；只证明调用仍经过 middleware chain 的既有 hook 位 | 归各能力自身既有测试；本记录声明 UNVERIFIED | **UNVERIFIED** |
| 被提升为 direct 的真实 MCP bridge 走审批链 | **BLOCKED**：`with_direct` / `prepare_system_tools` 是 `pub(crate)`，D-04 层只能证明判定输入（审批不看 `BaseTool`，`is_direct` 结构上无法影响审批）与被提升后的**可见性**（B-07）；没有一条用例真正**调用**已提升工具并同时观察审批与 wire | D-04 §BLOCKED 第 1 条；提升后的可见性归 B-07/D-02 | **BLOCKED**（缺口已记录，不由 D-03/D-04 冒充覆盖） |

## 5. 运行命令与现场结果

命令按 plan §6 逐条复跑（W1–W5 全部）。**两次独立执行（后台脚本 + 前台复跑）计数完全一致**，未观察到 flake。

| Task | 命令 | exit | 测试数 | 0 tests? |
| --- | --- | ---: | --- | --- |
| W1 A-02a（闸门） | `cargo check --workspace --all-targets` | 0 | 非测试命令 | N/A |
| W1 A-01 | `cargo test -p peri-acp-types --lib -- system_mcp` | 0 | 8 passed / 0 failed | 否 |
| W1 B-05（a） | `cargo test -p peri-agent --lib tool_catalog` | 0 | 13 / 0 failed | 否 |
| W1 B-05（b） | `cargo test -p peri-agent --doc` | 0 | 10 doctests / 0 failed | 否 |
| W2 A-02b | `cargo test -p peri-middlewares --lib -- mcp::config::tests` | 0 | 45 / 0 failed | 否 |
| W2 B-04 | `cargo test -p peri-agent --lib agent::stages` | 0 | 122 / 0 failed | 否 |
| W2 C-INJ-01 | `cargo test -p peri-middlewares --lib -- mcp::tool_bridge` | 0 | 14 / 0 failed | 否 |
| W2 C-INJ-02 | `cargo test -p peri-middlewares --lib -- mcp::system_tools` | 0 | 17 / 0 failed | 否 |
| W3 B-01 | `cargo test -p peri-middlewares --lib -- mcp::client::tests` | 0 | 39 / 0 failed | 否 |
| W3 B-02 | `cargo test -p peri-middlewares --lib -- mcp::initialize` | 0 | 10 / 0 failed | 否 |
| W3 B-06 | `cargo test -p peri-middlewares --lib -- mcp::dynamic::registry` | 0 | 22 / 0 failed | 否 |
| W4 B-03 | `cargo test -p peri-middlewares --lib -- mcp::middleware` | 0 | 40 / 0 failed | 否 |
| W5 C-INJ-03 | `cargo test -p peri-middlewares --lib -- mcp::system_tools`；`-- tool_search` | 0 / 0 | 17 / 65 passed | 否 |
| W5 A-03 | `cargo test -p peri-middlewares --lib -- test_dynamic_mcp_rejects_system_mcp_fields` | 0 | 1 / 0 failed | 否 |
| W5 A-04 | `git diff --check` | 0 | 非测试命令 | N/A |
| W5 B-07 | `cargo test -p peri-acp --lib -- host::executor_flow_tests` | 0 | 29 / 0 failed | 否 |
| W5 D-02 | `cargo test -p peri-middlewares --lib -- mcp::mcp_v4_seam` | 0 | 6 / 0 failed | 否 |
| W5 D-03 | `cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` | 0 | 3 / 0 failed | 否 |
| W5 D-04 | `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` | 0 | 5 / 0 failed | 否 |

计划命令合计：466 passed / 0 failed，全部 exit 0；无一条命令输出 0 tests（两条非测试命令按定义不产生 `test result` 行）。

**命令覆盖不足的补充复跑**（plan §6 的过滤器未命中以下模块，D-05 追加并记录）：

| 缺口 | 补充命令 | exit | 测试数 |
| --- | --- | ---: | --- |
| B-07 新增用例所在模块 `host::mcp_v4_startup_tests` 未被 `host::executor_flow_tests` 命中 | `cargo test -p peri-acp --lib -- host::mcp_v4_startup_tests` | 0 | 11 / 0 failed |
| `host::prompt::tests`（B-07 的 ACP 投影用例） | `cargo test -p peri-acp --lib -- host::prompt::tests` | 0 | 17 / 0 failed |
| `mcp::client::readiness`（B-01 的 readiness_test）不在 `mcp::client::tests` 内 | `cargo test -p peri-middlewares --lib -- mcp::client::readiness` | 0 | 17 / 0 failed |
| A-02b 的 `plugin::loader::tests` 不在 `mcp::config::tests` 内 | `cargo test -p peri-middlewares --lib -- plugin::loader` | 0 | 58 / 0 failed |
| A-02a 的 `mcp::transport`、`mcp::resource_cache` 只有编译闸门 | `cargo test -p peri-middlewares --lib -- mcp::transport`；`-- mcp::resource_cache` | 0 / 0 | 10 / 23 passed |
| B-05/B-06 的 `stage_builder`（`builder_v2_tests`） | `cargo test -p peri-agent --lib -- stage_builder` | 0 | 2 / 0 failed |

补充命令合计：138 passed / 0 failed，全部 exit 0。全部测试命令合计 **604 passed / 0 failed**。

**辅助证据**：`cargo clippy --workspace --all-targets -- -D warnings` exit 0（cargo 复用已缓存 clippy 结果，无新诊断）。此项不属 plan §6 门禁，仅作参考。

**测试模块 wiring 复核**（plan §9 规则 2）：新增测试文件均已挂载——`mcp/v4_seam_test.rs`→`mcp/mod.rs:62`、`mcp/system_tools_test.rs`→`system_tools.rs:386`、`mcp/client/readiness_test.rs`→`readiness.rs:628`、`peri-acp/src/host/mcp_v4_startup_test.rs`→`host/mod.rs:52`、`stage_builder/tools_test.rs`→`stage_builder/tools.rs:103`；两个 `tests/*.rs` 由 cargo 自动发现。未见未挂载的孤儿测试文件。

### 5.1 独立验证阶段的收口复跑（W6 之后）

以下命令由独立验证方在三个提交冻结后复跑，与前表口径一致（0 tests 视为失败）。

| 命令 | exit | 测试数 | 0 tests? | 结论 |
| --- | ---: | --- | --- | --- |
| `cargo test --workspace --lib` | 0 | 1803 passed / 0 failed / 10 ignored | 否 | 全 workspace lib 目标无回归 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 非测试命令 | N/A | 无 warning |
| `cargo test -p peri-acp-types -p peri-agent -p peri-middlewares --lib`（`cargo fmt --all` 之后） | 0 | 3113 passed / 0 failed / 5 ignored | 否 | 格式化未改变行为 |

**关闭的缺口**（W3 gate 报告的两项）：

| 缺口 | 处置 | 承担用例 |
| --- | --- | --- |
| `catalog_registration` 接线零覆盖：`DynamicMcpErrorCode::ToolNameConflict → StartupRegistrationRejected` 与其余拒绝 → `InconsistentCapability` 的映射无测试（两端各自有测试，中间接线没有） | **已补**：新增 `peri-agent/src/session/exec/stage_builder/tools_test.rs`（3 例），在实现文件内挂载 | `startup_registration_maps_tool_conflict_to_rejection`、`startup_registration_maps_non_conflict_to_inconsistent_capability`、`startup_registration_forwards_session_and_candidate_tools`（`cargo test -p peri-agent --lib -- stage_builder::tools::tests` exit 0 / 3 passed） |
| W1 gate H-1：`stage_builder.rs` 认领但未改动，B-05 主张 `replace_static_mcp_tools` + `refresh()` 已足够 | **确认为正确，非缺口**：`replace_static_mcp_tools` 只更新 static base 与 published，Reason 边界仍完整走 `refresh → working map swap → before_reason_catalog → before_model → pin`（`tool_catalog.rs:214-300`）；首个 LLM 请求入参已由 `system_mcp_ready_exposes_required_tools_on_first_model_request` 端到端断言 | 同上 host 用例 |
| `set_startup_catalog_registration` 上遗留的 `#[allow(dead_code)]` 与「本次提交前尚未接线」注释在接线后已过时 | **已清理**：移除 allow，注释改为「未接线的目录（子 agent 沿用自身 capability）保持 None」；clippy `-D warnings` 仍为 exit 0，反证调用点真实存在 | `peri-agent/src/session/tool_catalog.rs` |

## 6. IF-M4 回退声明

**未实施回退。** 现场实现是 v2 方案（`peri-agent/src/middleware/trait.rs:87`、`peri-agent/src/agent/stages/mod.rs:902-909`），stages/mod.rs 的 diff 仅新增闸门调用与 `react_start_has_run` 标志；首批 `before_agent` 的 `tracing::warn!` 软失败降级**保持原样**（同一文件 `before_agent` 分支未改动）。

因此：**不存在「既有 before_agent 全局错误语义变更」这一契约变更**；v1 回退方案要求的三项补救义务（覆盖登记 / AgentsMd·AtMention·Plugin·SkillPreload 回归 / acceptance 声明）本次**不适用**。`StartupState` 的能力收窄由 2 个 `compile_fail` doctest 固定（`capabilities.rs:133`、`:139`，随 `-p peri-agent --doc` 10 doctests 通过）。

## 7. 文件所有权与 diff 越界复核（plan §10）

- **矩阵内**：`git status` 的 40 个 modified 条目中 37 个、以及 8 个实现类新增文件（`mcp_v4_startup_test.rs`、`client/readiness.rs`、`client/readiness_test.rs`、`mcp_v4_seam_test.rs`、`system_tools.rs`、`system_tools_test.rs`、`tests/mcp_isolation_contract.rs`、`tests/mcp_host_policy_contract.rs`）全部落在 §4 所有权矩阵路径内；未发现任务文件被写入矩阵外路径。
- **矩阵外但被改动（非 W1–W5 产出）**：
  - `.github/workflows/ci.yml`（mtime 2026-09-25 09:06，早于实施窗口 23:29–00:54）：内容为 CI 并发控制、`CARGO_PROFILE_*_DEBUG`、layer-import 门位置调整与测试分步，与 MCP v4 无关，不属 §4 任何 task 产出。
  - `docs/design/README.md`（mtime 2026-09-25 21:53）：在 design 索引表登记 `mcp-adaptation-v4-part-1.md`，属设计文档立项步骤，早于 plan（23:29）与实施窗口。
  - `peri-cool` 子模块指针为 dirty 标记（非本次任务文件）。
  - 上述均无 MCP 运行时语义，记录在此以说明「§4 之外存在 diff」这一事实，不作为越界修复对象。
- **矩阵内但未被改动**：`peri-agent/src/session/exec/stage_builder.rs`、`stage_builder/builder_v2_test.rs`（B-05 认领但无需修改，§5.1 已核实依据）；`initialize.rs` 的 4 处 `unwrap_or_default()` 已消除（grep 0 命中）。
- **独立验证阶段新增**：`peri-agent/src/session/exec/stage_builder/tools_test.rs`（关闭 §5.1 的接线覆盖缺口，落在 `stage_builder/` 路径内）；`peri-agent/src/session/tool_catalog.rs`、`peri-acp-types/src/plugin.rs`、`peri-middlewares/src/mcp/mcp_v4_seam_test.rs` 三处经 `cargo fmt --all` 重排（仅格式，无语义变更，§5.1 已复跑）。
- `docs/design/mcp-adaptation-v4-part-1.md` 未被修改（未回填批次/勾选/耗时）。
- 提交切分：`ca0265e4`（规划与验收记录）、`10162ec8`（实现）、`041f9cac`（文档同步）；`.github/workflows/ci.yml` 与 `peri-cool` 未纳入提交（与本次任务无关）。

## 8. 本次未验证 / 非目标

- **未验证**：契约 5 的凭据隔离、capability root 隔离、五个目标 MCP 实例落地（§3）；契约 6 的 SubAgent / Workflow / Goal / PTC 实现、被提升 direct 工具的端到端审批链（§4）；`Filesystem`/`Terminal`/`Web`/`Cron`/`Lsp` middleware 真实迁移。
- **未运行**：`cargo test --workspace`（含 `peri-tui`）、`e2e/` TUI 场景、`side-projects/local-mcp-server` 独立 workspace 测试、Windows 平台（`host::mcp_v4_startup_tests` 用例带 `#[cfg(not(windows))]`，仅 macOS 本机验证）。plan §6 未把这些列为本批次门禁。
- **非目标**（plan §1.2）：不新增宿主 CLI 入口；不实现 `system_mcp` 的 session 中途声明；不改 `ToolSearchMiddleware` 既有 deferral 契约。

## 9. 整体裁决

- 契约 1、2、3、4、7：**PASS**（契约 2/3/4 为 host seam 层强证据：真实子进程 rmcp stdio + counting model + 首个 LLM 请求入参断言）。
- 契约 5：**PARTIAL** —— 只覆盖已落地连接的局部隔离；凭据、capability root 与五实例迁移为 **UNVERIFIED / 未落地**。
- 契约 6：**PARTIAL** —— Permission/HITL/effective name/cancel/ToolSearch/装配层已断言；Hook 为 PARTIAL，SubAgent/Workflow/Goal/PTC 与被提升 direct 工具链路为 UNVERIFIED/BLOCKED。
- 整体：**v4-part-1 的契约 1–4、7 已落地并有本次运行时证据；契约 5、6 未完成，绿色单测不构成五 MCP 迁移完成的证据。**
