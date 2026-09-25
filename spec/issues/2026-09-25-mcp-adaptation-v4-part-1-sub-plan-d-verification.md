# MCP adaptation v4-part-1：Sub-plan D 跨层验证与文档一致性

> **主 plan v2 覆盖（优先于本文）**：见 [`2026-09-25-mcp-adaptation-v4-part-1-plan.md`](2026-09-25-mcp-adaptation-v4-part-1-plan.md) §5。本文与主 plan 冲突处一律以主 plan 为准，涉及本文件的具体覆盖：**R3**（`docs/code-index/**` 唯一 owner 是 D-06）、**R9**（本文 D-01 行作废：`peri-middlewares/tests/` 外部集成测试触达不到 readiness 内部 seam；其跨层职责由 B-07 host seam 与 D-02 crate 内 seam 承接）、**R10**（`peri-agent/src/session/exec/stage_builder.rs` 归 B-05 所有）、**R13**（D-06 依赖 D-05，排在 D-05 之后的独立 Wave）。

## 1. 元信息

- **范围**：验收契约 5、6、7，以及契约 2/3/4 的跨层验证；本计划不实现生产代码，不实现五个目标 MCP 的真实迁移。
- **实施边界**：本 workflow 只落地契约 1–4、7；为契约 5/6 增加契约测试和文档口径，不把“测试通过”写成五个 MCP 已完成隔离迁移。
- **依赖 sub-plan**：依赖 A 的 `system_mcp` / `system_mcp_tools` 配置字段及错误语义；依赖 B 的 `await_system_ready` 类等待接口、ready/timeout/failure 状态及 pool 生命周期；依赖 C 的 system tools 注入函数、RCRA direct tool projection 和 effective MCP tool name 语义。
- **接口冻结占位**：以下名称是计划占位，最终测试只使用 A/B/C 冻结后的 public API：`system_mcp`、`system_mcp_tools`、`await_system_ready` 类函数、system tools 注入函数。若最终符号或参数不同，不通过访问私有字段或修改 A/B/C 源文件适配，而是在测试文件内增加一层最小的局部 fixture/adapter；若没有可观察的 public seam，则把该测试标为 blocked，并在验收记录中写明缺口。
- **产出**：跨层运行时断言、宿主 Permission/HITL/effective tool name/cancel 不绕过的证据、契约 5/6 的降级证据及限制、契约 7 的状态口径和独立验收记录模板。
- **文件所有权**：本 plan 只新增/修改 `peri-middlewares/tests/**`、必要时 `e2e/**` 下新测试和 `docs/**`、`spec/issues/**` 中本 plan 所属产物；不修改 A/B/C 所有的源文件。

## 2. 事实基线

### 2.1 设计事实源与边界

- 权威设计文档说明：MCP transport 可以是 builtin 内存 transport 或 external stdio/HTTP，但每个实例必须独立拥有 transport、状态、凭据和 capability root；共享 library/schema/fixture 不等于共享实例（`docs/design/mcp-adaptation-v4-part-1.md:22-31`）。
- 目标是五个相互隔离的 MCP，且 MCP 之间不得隐式调用；当前 workflow 明确不实现五个实例迁移（`docs/design/mcp-adaptation-v4-part-1.md:67-77`）。
- System MCP 必须在 react loop 前完成 transport、initialize、能力协商、健康检查；超时/失败不得发布 ready（`docs/design/mcp-adaptation-v4-part-1.md:33-41`）。工具注入必须直接进入 RCRA 工具列表，不能经 `ToolSearchMiddleware`，但仍保留 effective tool name namespace（`docs/design/mcp-adaptation-v4-part-1.md:43-65`）。
- 七条验收契约中，契约 5/6/7 的规范文本分别在 `docs/design/mcp-adaptation-v4-part-1.md:222-234`；本计划的测试不得用静态清单代替 transport、宿主装配和 RCRA 工具视图上的运行时证据。

### 2.2 Rust 测试基建

- 测试规范规定：跨模块端到端验证放在 crate 根 `tests/`，只能访问 crate 的 `pub` API；局部单元测试放在同目录 `_test.rs`（`docs/standards/testing.md:12-19`）。因此本计划的宿主装配 seam 优先放在 `peri-middlewares/tests/`，不复制 A/B/C 的私有单元测试。
- 现有集成测试包括：`middleware_tool_registration.rs:1-42` 只构造 `FilesystemMiddleware`/`TerminalMiddleware` 并检查工具名称；`canonical_tool_invocation_contract.rs:1-128` 构造 `StageContext`、`MiddlewareChain`、`SessionToolCatalog`、`EffectiveToolDispatcher`，可验证工具调用经过 middleware/事件/规范化路径；`run_ptc_code_e2e.rs:1-128` 是 PTC 的进程内真实 Node fixture，但不是 MCP transport/host assembly 测试。
- 现有 MCP client 脚手架已经使用 `rmcp` 和内存 duplex：`client/service_test.rs:1-85` 构造 `tokio::io::duplex`、`AsyncRwTransport`，在进程内对 initialize/discover 响应；`client/transport_test.rs:1-106` 以 `duplex` 断言 Auto/Discover 的真实 JSON-RPC 首请求和 fallback。它们是真实 client transport wire，但不是 rmcp `ServerHandler` 组成的完整 MCP server，也未跨过宿主装配和 RCRA 视图。
- `client/process_test.rs:1-80` 可以启动真实子进程、检查进程树、handshake timeout 和 pool cleanup，但其子进程是 `bash` 生命周期 fixture，不是 MCP server；不能据此宣告 external MCP 端到端成立。
- `mcp/middleware_test.rs:10-52`、`:64-101` 目前主要用手造 `McpClientHandle`/`rmcp::model::Tool` 及 deployment/projected pool 检查 tool projection；`:561-579` 已有 cancel 后 `before_agent` 无动作测试；`:782-828` 已验证连接事件触发 discovery。它们是 MCP middleware 单元契约，不覆盖真实 transport → initialize → assembly → RCRA seam。
- `mcp/config_test.rs:6-120` 是配置解析、协议版本和 OAuth 默认值的单元测试；契约 1 的解析拒绝由 A 负责，D 只在装配后验证错误是否阻止启动。
- 测试规范还要求集成测试跨越声称支持的真实生命周期、使用真实 wire/ordering，静态共享代码和单进程 unit test 不能替代（`docs/standards/testing.md:182-186`）；fixture 必须在测试文件内部定义，不建立共享 workspace test helper（`docs/standards/testing.md:190-199`）。

### 2.3 独立 `local-mcp-server` 与 e2e

- `side-projects/local-mcp-server/Cargo.toml:1-15` 有独立 `[workspace]`，根 `Cargo.toml:1-18` 的 members 不包含它；测试规范明确 side project 不由根 workspace 测试门禁覆盖，必须进入其目录执行本地命令（`docs/standards/testing.md:62-67`、`:317-322`）。
- 独立 server 的测试使用 `env!("CARGO_BIN_EXE_local-mcp-server")`：`side-projects/local-mcp-server/tests/e2e_support/mod.rs:37-43`；其真实 stdio 进程 E2E 的范围和断言见 `tests/e2e_stdio.rs:1-16`，并非根 workspace 的 fixture。
- 全仓只核实到上述独立项目使用 `CARGO_BIN_EXE_local-mcp-server`；未核实到根 workspace 测试以 `CARGO_BIN_EXE_...` 或 path dependency 调用它。结论是根测试不能直接依赖该 binary 的 Cargo 注入；若要使用，必须由独立项目先按自身命令构建并通过显式环境变量传入，且不能作为根测试默认门禁。更稳妥的契约 fixture 是在 `peri-middlewares/tests/` 内用 `tokio::io::duplex` + 局部 rmcp server handler，避免跨 workspace target/lock 和本机 binary 依赖。
- `e2e/CLAUDE.md:1-24` 规定 e2e 是 `tui-tester` + tmux 的真实 TUI 交互测试，推荐单文件命令为 `npm run e2e -- --file tests/<path>.test.ts --serial --retry 0`，分层门禁为 `npm run e2e:l0`/`e2e:l1`/`e2e:release`；数据流为 `run-e2e.mjs → helpers/peri.ts → dev.sh → Peri TUI`（`:26-45`）。该层不是 MCP client/host assembly 的直接 seam，故不把 TUI E2E 当作契约 2/3/4 的唯一证据；只有当 B/C 提供宿主启动配置和可观察 UI 结果时，才新增辅助 TUI 场景。

### 2.4 文档同步规则与验收记录惯例

- 根 `CLAUDE.md` 要求先读 `docs/standards/index.md`、按意图查 `docs/code-index/`，变更时同步索引（`CLAUDE.md:37-42`）；MCP/工具/中间件任务还应读 `peri-middlewares/CLAUDE.md`，E2E 任务读 `e2e/CLAUDE.md`（`CLAUDE.md:43-55`）。
- `DOC-UPDATE-001` 要求实现变更检查受影响 standards、模块 CLAUDE、测试 canonical 路由和命令，只更新单一事实源（`docs/standards/documentation.md:27-31`）；`DOC-LINK-001` 要求移动/合并/删除时同步 CLAUDE、standards、code-index、active spec 和引用，并做链接检查（`:51-55`）。
- `docs/standards/index.md:1-40` 是 standards 路由和优先级索引，不复制测试规则；若本次只新增 MCP 验证，不应无理由改 standards，但如新增 canonical 命令或测试边界，必须更新 `docs/standards/testing.md` 并同步其索引路由。
- 当前 MCP 代码索引把 transport、static MCP execution directory、`McpClientPool`、`McpMiddleware` 和真实 client process/service test 作为入口，见 `docs/code-index/peri-middlewares.md:15-23`；`local-mcp-server` 的独立边界、入口和验证命令见 `docs/code-index/local-mcp-server.md:1-34`。代码变更后这两个 index 是必查对象，只有入口/命令/职责事实改变时才实际改动。
- 近期 issue 采用“状态/优先级/类型/日期 → 目标/事实 → 已验证/已证伪/未验证 → 待办/验收标准/相关文件”的证据结构：`spec/issues/2026-09-01-workflow-delivery-git-postcondition.md:1-11`、`:42-56`；flake 记录明确区分本地证据、未复现、未验证平台和 PARTIAL 判定（`spec/issues/2026-09-10-meta-session-readonly-flake-unproven.md:53-90`）。
- **决策**：本次现场命令、实际执行用例数、transport/assembly/RCRA 证据和限制不回填 `docs/design/mcp-adaptation-v4-part-1.md`，也不把本 sub-plan 当最终验收勾选表；在实施完成后新建 `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`。该记录复用上述 issue 结构，增加“契约矩阵（1–7）/运行命令与 exit status/实际用例数/证据强度/blocked 与 skipped/目标归属≠已落地能力/非目标”章节，并以 `PARTIAL` 或 `PASS` 明确整体裁决。契约 5/6 若只有降级断言，必须保持 `PARTIAL`，不能由契约 1–4 的绿色推导整体完成。

## 3. 验收契约 5 的验证设计

### 3.1 可以运行时断言的部分

**测试文件**：`peri-middlewares/tests/mcp_isolation_contract.rs`，建议函数：

- `system_mcp_instances_use_distinct_pool_and_transport_owners`：创建两个配置项（不同 server name、不同 stdio fixture identity 或不同 HTTP fixture endpoint），通过 A/B 冻结后的初始化入口分别构造两个 MCP 连接；在真实 `rmcp` duplex/stdio wire 上记录 initialize、tools/list 和关闭顺序，断言两个实例使用不同 `McpClientPool`/service owner、各自只收到自己的请求、各自关闭不影响另一条 transport。若 B 只允许单 pool 多 server，则断言至少是不同的 `McpConnectionKey::Static { server_name }`、不同 `McpClientHandle` `Arc` 和不同 service/transport owner；不能把同 pool 容器本身误判为实例共享。
- `system_mcp_instances_do_not_implicitly_call_each_other`：fixture A 的唯一工具返回带 A 标记，fixture B 的唯一工具返回带 B 标记；从 RCRA direct tool view 分别调用 A/B effective name，server 端只允许对应请求，断言 A 调用不会收到 B 的请求，且不出现由 A 触发 B 的二次 JSON-RPC 请求。该断言是运行时无隐式依赖证据，而非检查 source list。
- `distinct_configs_keep_distinct_pool_entries_and_config_identity`：通过冻结后的 `system_mcp` 配置装配两个 server，断言 pool snapshot/handle view 中 server name、transport 类型、tool namespace 和 handle `Arc` 各自对应；不比较或打印 credential 值，只断言每个配置只能命中自己的 credential/transport context。若接口只暴露 snapshot，则至少断言不同 entry、不同 transport type/endpoint identity 和 namespace 路由。

**可证明强度**：不同 pool entry、不同 `Arc<McpClientHandle>`、不同 transport wire、不同 process owner、无跨 server 请求，是强运行时证据；它不能证明尚未装配的 Artifact/Web/Cron/LSP 五个生产实例已经存在。

### 3.2 只能降级断言的部分

- **凭据不共享**：当前 `McpClientHandle` 的可见字段是 `peer/tools/status/oauth_status/source/url/...`（`peri-middlewares/src/mcp/client/types.rs:83-104`），pool 的共享字段包括 services/processes/configs 和一个 pool-wide `capability_profile`（`peri-middlewares/src/mcp/client.rs:44-100`）；实际 token/credential store 在 initialize 中由 `FileCredentialStore::new()` 创建（`peri-middlewares/src/mcp/initialize.rs:74-85`），没有可安全读取的 per-instance credential identity public API。不能通过比较秘密、日志或 debug 输出来证明“不共享”。降级为：不同配置项分别触发独立 authorization/headers 注入路径，服务端 fixture 记录各自收到的非秘密 sentinel header 名/opaque credential fingerprint（测试不得输出原值）；若 B 未提供可观察 hook，则只能断言 `McpConnectionKey`/配置归属不混淆，并在 acceptance 记录标注“凭据隔离未完整可测”。
- **capability root**：当前 `McpClientPool` 明确暴露的是 deployment-level `capability_profile`，而 `McpClientHandle` 没有 capability root 字段（`peri-middlewares/src/mcp/client.rs:94-99`、`peri-middlewares/src/mcp/client/types.rs:83-104`）。`local-mcp-server` 的 `RootDir` 是独立项目内部的工作区根能力边界，索引明确说明它不是安全沙箱，且 Bash 不受文件 root 限制（`docs/code-index/local-mcp-server.md:6-10`、`:23-26`）。因此不能声称已完成五个 MCP 的 capability-root 隔离。若 B/C 提供 root identity/URI public view，断言两个 fixture 各自只能读写自己的 temp root；否则只做“配置 cwd/root 参数不共享、工具请求不跨 fixture root”的降级断言，并将其证据强度记为 partial。
- **五个目标 MCP 真实归属**：当前 pool/handle 结构只能观察已配置 server，不存在能枚举 Workspace/Artifact/Web/Cron/LSP 五个已迁移实例的事实接口。测试只验证两个最小隔离 fixture 和“没有隐式调用”，不能替代五实例迁移验收；契约 7 记录必须写“目标归属仍来自设计，实例落地未完成”。

### 3.3 运行位置与命令

- 首选 `peri-middlewares/tests/mcp_isolation_contract.rs`，因为它必须跨 crate public API；命令规划为 `cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1`。本次只写计划，不执行 Cargo 命令。
- 不把 `side-projects/local-mcp-server` 作为根测试的隐式 build dependency。若后续决定增加独立项目对照，使用其目录内的 `cargo test --test e2e_stdio`/`e2e_http`，并在 acceptance 记录列为独立 evidence，不把它计入根 workspace 测试通过数。

## 4. 验收契约 6 的验证设计

### 4.1 最可能被绕过的契约

本次改动触及工具可见性、System MCP ready gate、direct injection 和宿主装配，最危险的回归不是 MCP wire 本身，而是把 `system_mcp_tools` 直接写进 RCRA 工具表时绕过既有 `PermissionMiddleware`、HITL broker、事件/session/cancel 和 effective tool name dispatch。设计明确 Permission、HITL、Hook、SubAgent、Workflow、Goal、PTC 仍由宿主持有（`docs/design/mcp-adaptation-v4-part-1.md:161-174`、`:195-209`），所以“direct”只能跳过 `ToolSearchMiddleware`，不能跳过宿主安全和生命周期链。

### 4.2 真实验证方案

**测试文件**：`peri-middlewares/tests/mcp_host_policy_contract.rs`，建议函数：

- `system_direct_mcp_tool_enters_permission_and_hitl_with_effective_name`：用真实 duplex MCP server 返回一个写入型工具；通过 B ready gate 和 C system tools 注入函数装配 `ProductionChainAssembler`/RCRA tool map；调用 RCRA 中的 `mcp__<server>__<tool>`，注入记录 broker。断言 broker 收到的 tool name 是 effective name（不是裸 server tool name），拒绝时返回既有 `AgentError::ToolRejected`/HITL rejection，server 端没有收到 call；批准后才收到一次对应 MCP `tools/call`。这证明 direct injection 只绕过 deferred search，不绕过 Permission/HITL。
- `system_direct_mcp_tool_preserves_event_and_session_identity`：在同一个 `Session`/`StageContext` 中调用批准后的 MCP bridge，记录 before/after tool 事件或 `PolicyRecorder` 看到的 `ToolCall`，断言 session/canonical effective name 和结果事件属于同一 turn；不得从 bridge 内另造 session 或直接向 server 发起绕过 stage 的调用。现有 `canonical_tool_invocation_contract.rs:104-128` 已展示 `StageContext`、`MiddlewareChain`、`SessionToolCatalog` 和 event bus 的可复用脚手架，但新测试必须把真实 `McpToolBridge` 接入，不能只继续使用 `RecordingTool`。
- `system_direct_mcp_tool_cancel_stops_call_and_does_not_publish_ready`：用 server fixture 在 `tools/call` 后挂起，触发 session/agent cancellation；断言调用 future 结束为取消/既有错误，pool/service owner 仍由既有 shutdown owner 收尾，RCRA 不把取消误报成成功/ready。MCP middleware 已有 cancel 后 `before_agent` no-op 单元证据（`mcp/middleware_test.rs:561-579`），client close/transport cancel 也已有真实 duplex/process 生命周期证据（`client/service_test.rs:88-109`、`client/process_test.rs:45-80`）；D 测试要把 cancel 接在宿主 direct tool seam 上。
- `deferred_mcp_tool_still_requires_tool_search_path`：同一 fixture 中把一个非 `system_mcp_tools` 工具留作 deferred，断言它不在 direct RCRA list，而通过现有 ToolSearch projection 后才可见；该测试同时防止为了让 required tools 可见而把所有 MCP 工具都 direct 注入。

### 4.3 与既有宿主契约测试的关系

- Permission 的 approve/reject、MCP effective-name prefix 和 MCP 需要审批已有单元证据：`peri-middlewares/src/permission/mod_test.rs:52-102`、`:123-140`、`:171-200`；D 不复制这些纯函数断言，而验证真实 MCP bridge 走到这些路径。
- Hook 的 cancel/进程树 drain 证据在 `peri-middlewares/src/hooks/lifecycle_test.rs:146-200`；PTC 的 caller cancellation/TaskManager cleanup 在 `peri-middlewares/src/ptc/ptc_test.rs:92-117`；SubAgent 的事件身份/Start-Stop 在 `peri-middlewares/src/subagent/tool/tool_test/events_contract_test.rs:3-9`、`:64-99`。本次不把 MCP 工具伪装成 Hook/SubAgent/PTC，也不重新测试这些实现；只验证 direct injection 没有绕过它们所属的宿主链和 session cancel 入口。
- HITL 目录未发现独立 `_test.rs` 文件，现有 Permission tests 使用 `UserInteractionBroker` 的 approve/reject fixture（`peri-middlewares/src/permission/mod_test.rs:5-40`）。因此若 A/B/C 没有可注入 HITL broker 的装配 public seam，必须把测试状态标为 blocked，而不是用自动批准替代 HITL 证据；自动 broker 只能作为批准分支 fixture，不能证明真实 UI/HITL 交互契约。

## 5. 跨层端到端验证设计

### 5.1 契约 2：ready gate 与启动准入

- **文件/函数**：`peri-middlewares/tests/mcp_system_ready_e2e.rs`：
  - `system_mcp_ready_waits_for_real_transport_initialize_and_tools_list`；
  - `system_mcp_missing_required_tool_blocks_assembly`；
  - `system_mcp_initialize_failure_or_timeout_never_publishes_ready`；
  - `non_system_mcp_does_not_block_react_start`。
- **fixture**：测试文件内局部 rmcp server/duplex fixture，支持可控的 initialize 延迟、能力响应、`tools/list` 工具集、missing tool、malformed schema 和 transport close；使用 `Notify`/显式 barrier，不用墙钟 sleep 作为成功条件。fixture 必须返回真实 JSON-RPC 响应并让 B 的 `await_system_ready` 类接口观察实际状态。
- **seam 断言**：先启动 transport，再调用 B ready wait，再调用宿主装配；未 ready 前 RCRA/react-loop start latch 不得触发；成功时 latch 触发且 pool status 为 ready；失败/timeout 时错误类型/消息明确、ready 不发布、react loop 不启动。不能只断言 B 的 future 返回 `Ok`，也不能只检查 config struct。
- **边界**：A 单测负责非法配置解析；B 单测负责连接/timeout 状态机；D 负责 transport 已完成初始化后是否真的挡住宿主装配和 react start。

### 5.2 契约 3：namespace、schema、direct RCRA 视图

- **文件/函数**：`peri-middlewares/tests/mcp_tool_view_e2e.rs`：
  - `required_tools_are_schema_backed_and_directly_visible_in_rcra_view`；
  - `required_tool_uses_effective_mcp_name_and_calls_own_namespace`；
  - `unknown_required_tool_blocks_rcra_view`。
- **fixture**：真实 MCP duplex server 返回两个工具（一个 required、一个 deferred），required 工具含非空 JSON schema；server 端记录 `tools/list` 和 `tools/call` 的原始 name。宿主 fixture 要使用 A/B/C 冻结接口完成 ready、system tools injection、RCRA tool catalog 构造。
- **seam 断言**：RCRA 可见工具包含 `mcp__<server>__<required>`，参数 schema 等于/语义等价于 server declaration；裸名不出现在 direct view；调用 bridge 时 wire 上是所属 namespace 内的裸 MCP tool name；deferred 工具不因 system list 自动 direct 出现，仍需 ToolSearch。schema parse 失败和 required missing 必须阻止启动。
- **边界**：C 单测可验证 bridge/name/schema 转换；D 要验证转换结果真的进入宿主 RCRA tool map，并从该 map 经 Permission/HITL 调度。

### 5.3 契约 4：空数组

- **文件/函数**：同一 `mcp_tool_view_e2e.rs`：`empty_system_mcp_tools_waits_ready_without_extra_rcra_tools`。
- **fixture**：真实 transport 完成 initialize、能力协商和 `tools/list`，但 `system_mcp_tools = []`；同时配置一个普通 deferred tool 以排除“空数组导致整个 MCP 消失”的误判。
- **seam 断言**：ready 成功；RCRA direct list 不增加该 MCP 的额外 tool；普通 deferred tool 仍按 ToolSearch 路径存在/可检索；server 端仍观察到协议初始化和 tools/list，证明空数组不是跳过 MCP lifecycle。
- **边界**：B 负责 ready，C 负责注入函数的空数组语义，D 负责最终 RCRA view 和 deferred path 的组合结果。

### 5.4 是否新增 `e2e/` TUI 测试

默认不新增 `e2e/` 测试：现有 e2e 启动真实 TUI/tmux，依赖 `dev.sh` 和可能的 provider，不能精确观察 MCP transport、host assembly、RCRA view 三个 seam（`e2e/CLAUDE.md:3-33`）。若 workflow 后续要求 UI 显示 System MCP ready/failure，另新增 `e2e/tests/scenarios/mcp-system-ready.test.ts`，fixture 通过隔离 HOME/配置和本地无网络 MCP server 注入，命令沿用 `e2e/CLAUDE.md:7-24`；它只能作为宿主进程级补充，不替代 Rust seam 测试。

## 6. 文档一致性任务

### 6.1 必须同步的事实源

- **`spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`（新增验收记录）**：记录契约 1–7 的实际结果、命令、exit status、执行用例数、fixture、证据强度和 blocked/unsupported。契约 5/6 使用“完整运行时证据/降级证据/未验证”三态；契约 7 明确“目标归属”来自设计表，“已落地能力”只由本次运行时证据决定。
- **`docs/code-index/peri-middlewares.md`**：若 A/B/C 改变 MCP config、ready、assembly 或 tool bridge 入口，更新 MCP 速查表中对应主文件/入口/验证入口；补充跨层测试文件和命令，但不把目标五实例写成当前实现。当前索引的 MCP 入口在 `docs/code-index/peri-middlewares.md:15-23`。
- **`docs/code-index/peri-acp-types.md`**：若 A 把 `McpServerConfig` 的字段事实源或插件契约入口改变，更新对应 protocol/config 行；只写当前入口，不复制设计目标。
- **`docs/code-index/local-mcp-server.md`**：只有当 fixture/验证命令或 capability-root 语义改变才更新；目前必须保留“独立项目、不属于根 workspace”和“root 不是安全沙箱”的口径（`docs/code-index/local-mcp-server.md:1-10`）。
- **`docs/standards/testing.md`**：仅在本次确认了新的 canonical 跨层测试命令、根 workspace 与独立 project 的边界，或新增“System MCP seam 测试”稳定规则时更新；应放在测试目录/生命周期/命令相关章节，不把一次验收结果写入标准。索引 `docs/standards/index.md:20-26` 只需在标准文件新增/移动时核对，不重复规则。
- **`CLAUDE.md` / `peri-middlewares/CLAUDE.md`**：只在任务路由、MCP 稳定不变量或 canonical command 发生变化时更新；不能为了本次 issue 添加动态 inventory 或“迁移已完成”清单。DOC-UPDATE-001 要求只改受影响事实源（`docs/standards/documentation.md:27-31`）。

### 6.2 明确不能改/不能写的口径

- 不回填 `docs/design/mcp-adaptation-v4-part-1.md` 的具体迁移批次、提交号、现场勾选或测试耗时；设计文档已经规定这些应进入 issue/验收记录（`:9`、`:234`）。
- 设计表中的“目标：完全下放/部分下放/宿主保留”必须继续标成目标归属，而不是实现状态（`docs/design/mcp-adaptation-v4-part-1.md:176-180`）。验收记录新增“已落地能力”列，逐项填当前可观察的实例、工具 view、ready/error 和宿主链证据；未有证据的目标写 `未落地/未验证`。
- 不得用 `McpClientPool::new_empty()`、手造 `McpClientHandle`、绿色的 config/middleware 单测宣告契约 5/6/整体迁移完成；这些只能作为单元基线，现有例子见 `mcp/middleware_test.rs:30-52`。

## 7. 任务表

| Task ID | 标题 | 目标文件 | 改动摘要 | 验证命令 | 预估 diff 规模 | 依赖（A/B/C 的哪些产出） |
|---|---|---|---|---|---:|---|
| D-01 | System MCP ready 跨层测试 | `peri-middlewares/tests/mcp_system_ready_e2e.rs` | 局部真实 rmcp/duplex fixture；验证 transport→initialize→tools/list→ready→宿主启动及 failure/timeout/non-system 分支 | `cargo test -p peri-middlewares --test mcp_system_ready_e2e -- --test-threads=1` | 180–280 行 | A：`system_mcp`/`system_mcp_tools` 解析；B：`await_system_ready` 类接口、错误和状态；C：装配入口可观察 ready gate |
| D-02 | RCRA tool view 跨层测试 | `peri-middlewares/tests/mcp_tool_view_e2e.rs` | 验证 required schema/namespace/direct list/empty array/deferred search，实际调用真实 MCP wire | `cargo test -p peri-middlewares --test mcp_tool_view_e2e -- --test-threads=1` | 220–340 行 | B：ready 后 handle/tool declaration；C：system tools 注入函数、effective name、RCRA view |
| D-03 | MCP 实例隔离契约测试 | `peri-middlewares/tests/mcp_isolation_contract.rs` | 两个真实 transport/config fixture，断言 pool entry、owner、namespace、无隐式跨 MCP 调用；凭据/root 按可见 API 分级 | `cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` | 180–280 行 | B：独立 connection/service owner 或可观察 connection key；A：配置身份；若有 root/credential view 则使用，无则降级 |
| D-04 | 宿主策略/生命周期契约测试 | `peri-middlewares/tests/mcp_host_policy_contract.rs` | direct MCP tool 经过 Permission/HITL/effective name/session event/cancel，deferred 仍走 ToolSearch | `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` | 240–380 行 | B：ready/tool call cancel；C：注入函数和 tool view；宿主 assembly：可注入 broker/cancel/event 的 public seam |
| D-05 | 验收记录与契约 7 口径 | `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md` | 新建现场验收记录；契约矩阵、命令/exit status/用例数、证据强度、目标归属≠已落地能力、PARTIAL 规则 | `git diff --check`；验收阶段再执行 D-01–D-04 命令并记录终态 | 100–180 行 | A/B/C 所有接口冻结和 D-01–D-04 结果 |
| D-06 | MCP code-index/标准口径同步 | `docs/code-index/peri-middlewares.md`、必要时 `docs/code-index/peri-acp-types.md`、`docs/standards/testing.md`、`CLAUDE.md`/模块指引 | 只同步受影响入口、canonical 命令和测试路由；不更新设计文档进度 | Markdown link check；`git diff --check`；若改 Rust 测试再跑对应 cargo 命令 | 20–80 行 | A/B/C 最终路径和 D-01–D-05 实际命令 |

任务之间不共享目标文件：D-01–D-04 各自独占一个集成测试文件；D-05 独占验收记录；D-06 只改索引/标准/指引文件。若 D-06 发现无需同步某文件，应在验收记录写“核对但未变更”，而不是为制造 diff 改文档。

## 8. 风险与未知

- **接口冻结漂移**：如果 A/B/C 最终没有按 `system_mcp`、`system_mcp_tools`、`await_system_ready` 类函数、system tools 注入函数提供 public seam，D 先在测试文件内使用最小 adapter 适配参数重命名；adapter 只能组合 public 行为，不能 downcast 私有实现、读私有字段或重复生产逻辑。若无法观察 ready 阻塞、RCRA view 或 Permission dispatch，则测试标记 `blocked`，并把“缺失 seam”列入跨 plan 依赖，不修改 A/B/C 文件。
- **公开字段不足**：当前 `McpClientHandle` 不暴露 transport、credential、capability root（`peri-middlewares/src/mcp/client/types.rs:83-104`），pool 的 capability profile 是 pool-wide（`peri-middlewares/src/mcp/client.rs:94-99`）。契约 5 的凭据/root 只能降级到配置归属、连接 owner、server-side sentinel 请求和 root 行为；这不构成完整契约 5 证据，验收必须保持 PARTIAL。
- **真实 MCP server fixture 的边界**：现有 duplex fixture 是手写 JSON-RPC responder，不是完整 rmcp server；引入 rmcp `ServerHandler` 需先核对当前 crate feature/API。若 API 不稳定，保留手写 wire fixture，但在 acceptance 记录说明它证明 client transport wire，不证明独立 server 实例装配；不能改用静态工具清单。
- **local-mcp-server 依赖风险**：独立 workspace 的 `CARGO_BIN_EXE_local-mcp-server` 只在其自身测试编译上下文有效；跨 workspace 直接依赖会产生 target/lock 竞争和不可复现的 binary 前置条件。除非 workflow 明确提供外部 binary 路径，否则不纳入根 D 测试门禁。
- **HITL seam 未知**：未发现独立 HITL `_test.rs`；Permission fixture 有 broker，但真实 UI/HITL 交互需 public broker/session seam。没有该 seam 时不以自动批准测试冒充 HITL，标为 blocked。
- **取消和异步稳定性**：ready timeout、server hang、进程关闭可能受 tokio scheduling 影响。使用 `Notify`、barrier、手动 server state 和 `--test-threads=1`；不以“睡眠后应该完成”作为唯一证据。测试规范要求确定性、精确错误断言和独立运行（`docs/standards/testing.md:127-180`）。
- **既有 flake**：`spec/issues/2026-09-20-p2-parallel-lib-test-atom-races.md:1-12` 记录 macOS 全量并行测试偶发失败，`2026-09-10-meta-session-readonly-flake-unproven.md:53-90` 记录本地证据不足和平台未验证。D 测试不运行全 workspace 并行门禁；使用定向、串行、临时 HOME/fixture，并在验收记录记录首次失败、重跑次数和 flake.firstAttemptFailed。任何重跑通过都不能清除第一次失败证据。
- **证据强度误读**：D-01/D-02 通过只说明契约 2–4 在一个真实 fixture seam 上成立；D-03 的降级断言不等于五 MCP 隔离；D-04 通过只说明当前 direct injection 未绕过已暴露宿主路径；四个测试全绿也不等于 v4 目标归属全部落地。验收裁决必须按契约矩阵逐项给出，不得以绿色局部单测宣告整体迁移。

## 9. 非目标

- 不实现 Workspace、Artifact、Web、Cron、LSP 五个真实 MCP 实例，不迁移 middleware，不共享或拆分生产 pool/transport/credentials/capability root。
- 不修改 A/B/C 所有的 `plugin.rs`、`mcp/config.rs`、`config_test.rs`、`mcp/middleware.rs`、`mcp/client/**`、`readiness.rs`、`tool_bridge.rs`、`system_tools.rs` 或其他生产源码；需要这些变更时只在跨 plan 依赖中记录。
- 不把 `ToolSearchMiddleware` 删除或改成 MCP；required direct injection 仅验证跳过 deferred search，保留宿主 Permission/HITL/Hook/SubAgent/Workflow/Goal/PTC 语义。
- 不把 `local-mcp-server` 纳入根 workspace，不把独立项目测试数量算入根 workspace 验收，不新增真实外部网络或真实用户凭据测试。
- 不修改权威设计文档以记录批次、提交、现场验收、耗时或完成清单；这些只写独立 acceptance issue。
- 不运行 `cargo build`、`cargo test`、`cargo run`，不提交 git commit；本 session 仅完成侦察和本 sub-plan 文档。